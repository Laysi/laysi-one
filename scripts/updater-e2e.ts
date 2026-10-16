#!/usr/bin/env bun
// End-to-end test of the host-side updater (backend/crates/laysi-updater)
// against a throwaway stack: real podman, podman-compose, systemd user units,
// postgres and rustfs, with Laysi's own three images replaced by small fakes
// pushed to a local registry, so each release can be broken on purpose.
//
//   bun scripts/updater-e2e.ts public-key
//   bun scripts/updater-e2e.ts run --updater <laysi-updater built with test keys> --yes
//
// `run` takes over the user it runs as: it installs into
// ~/.local/share/laysi-one, writes systemd user units and runs a registry on
// localhost:5000. It is meant for a CI runner or a throwaway container, never
// a workstation. It needs a systemd user manager (lingering, with
// XDG_RUNTIME_DIR set): without one healthchecks never run and the path unit
// cannot be tested.
//
// The updater has to trust the key the manifests here are signed with. That
// key is a fixed test key, so the binary is built with
//
//   LAYSI_UPDATER_TEST_KEYS=$(bun scripts/updater-e2e.ts public-key) \
//     cargo build --release -p laysi-updater --features test-keys
//
// and only a build with that feature trusts it.
//
// The fake api runs a "migration" (a row in e2e_migrations), refuses to start
// on a database holding a migration it does not know, the way sqlx's
// VersionMissing does, and passes its healthcheck once it is serving. Each
// release can make it exit at start or never pass.

import { createHash, randomUUID } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, truncateSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";

const REPO = resolve(import.meta.dir, "..");
// Fixed and public on purpose: only a `test-keys` build trusts it.
const TEST_SEED = "e2e0".repeat(16);
const REGISTRY_PORT = 5000;
const REGISTRY = `localhost:${REGISTRY_PORT}/laysi`;
const PROJECT = "laysi-one";
const HOME = process.env.HOME ?? "";
const ROOT = join(HOME, ".local/share/laysi-one");
const RESULTS = join(ROOT, "results");
const REQUESTS = join(ROOT, "requests");
const BACKUPS = join(ROOT, "backup");
// laysi_license_token::update::SWITCHING_FILE: api and mcp refuse writes
// while it exists.
const MARKER = join(RESULTS, "switching.marker");
const CONTAINER_MARKER = "/var/lib/laysi-update/results/switching.marker";
// laysi_license_token::update::FALLBACK_BOOT_FILE: the screen shows a notice
// while it exists.
const FALLBACK = join(RESULTS, "fallback-boot.marker");
const CONTAINER_FALLBACK = "/var/lib/laysi-update/results/fallback-boot.marker";
const UNITS = ["laysi-updater.path", "laysi-updater.service", "laysi-updater-boot.service", "laysi-updater-recover.service", "laysi-updater-fallback.service"];
const FINAL = new Set([
  "succeeded",
  "succeeded-updater-kept",
  "failed-unchanged",
  "rolled-back",
  "rollback-failed",
  "refused-signature",
  "refused-downgrade",
  "refused-same-version",
  "refused-installed-unknown",
  "refused-disk-space",
  "refused-busy",
  "refused-blocked",
]);

// Pinned by docker-compose.prod.yml; the fakes are built on the same bytes.
const prodCompose = readFileSync(join(REPO, "docker-compose.prod.yml"), "utf8");
const shipCompose = readFileSync(join(REPO, "docker-compose.ship.yml"), "utf8");
function pinned(service: string): string {
  const block = prodCompose.split(/\n  (?=[a-z]+:\n)/).find((b) => b.startsWith(`${service}:\n`));
  const image = block?.match(/\n    image: (\S+)/)?.[1];
  if (!image?.includes("@sha256:")) fail(`docker-compose.prod.yml does not pin ${service} by digest`);
  return image;
}
const ALPINE = pinned("secrets");
const RUSTFS = pinned("rustfs");

// ---------------------------------------------------------------------------
// Plumbing

function fail(message: string): never {
  console.error(`\nFAIL: ${message}`);
  process.exit(1);
}

function log(message: string) {
  console.log(`[${new Date().toISOString().slice(11, 19)}] ${message}`);
}

interface Run {
  code: number;
  out: string;
  err: string;
}

function sh(cmd: string[], opts: { stdin?: string | Buffer; ok?: boolean; env?: Record<string, string> } = {}): Run {
  const p = Bun.spawnSync(cmd, {
    stdin: opts.stdin === undefined ? "ignore" : Buffer.from(opts.stdin),
    stdout: "pipe",
    stderr: "pipe",
    env: { ...process.env, ...(opts.env ?? {}) },
  });
  const run = { code: p.exitCode ?? -1, out: p.stdout.toString(), err: p.stderr.toString() };
  if (opts.ok !== false && run.code !== 0) {
    fail(`${cmd.join(" ")} exited with ${run.code}\n${run.err.trim()}\n${run.out.trim()}`);
  }
  return run;
}

const podman = (...args: string[]) => sh(["podman", ...args]).out.trim();
const systemctl = (...args: string[]) => sh(["systemctl", "--user", ...args], { ok: false });

async function sleep(ms: number) {
  await new Promise((r) => setTimeout(r, ms));
}

async function waitFor<T>(what: string, seconds: number, probe: () => T | undefined | false, everyMs = 500): Promise<T> {
  const deadline = Date.now() + seconds * 1000;
  for (;;) {
    const value = probe();
    if (value) return value;
    if (Date.now() > deadline) fail(`timed out after ${seconds} s waiting for ${what}`);
    await sleep(everyMs);
  }
}

function check(condition: unknown, message: string) {
  if (!condition) fail(message);
  console.log(`  ok  ${message}`);
}

const sha256 = (data: string | Buffer) => createHash("sha256").update(data).digest("hex");

// ---------------------------------------------------------------------------
// Releases: fake images, compose files and signed manifests

type ApiMode = "ok" | "exit" | "never-healthy";

interface Spec {
  version: string;
  api?: ApiMode;
  secretsExtra?: boolean;
  rustfs?: string;
  updater?: "same" | "changed" | "broken";
  /** A digest already pushed, used as the backup image instead of a new build. */
  backup?: string;
}

interface Release {
  spec: Spec;
  migration: string;
  digests: { api: string; web: string; backup: string };
  manifest: string;
  compose: string;
}

// Runs until stopped, and stops on SIGTERM the way the real services do: a
// shell as PID 1 ignores it, and every stop would wait out its timeout.
const SERVE = `trap 'exit 0' TERM INT; while :; do sleep 1 & wait $!; done`;

const releases = new Map<string, Release>();
let updaterPath = "";
let workDir = "";

function fakeServer(spec: Spec, migration: string, known: string[]): string {
  return `#!/bin/sh
set -u
if [ "\${1:-}" = --healthcheck ]; then
    [ -e /tmp/ready ]
    exit $?
fi
[ "${spec.api ?? "ok"}" = exit ] && { echo "e2e api ${spec.version}: exits at start" >&2; exit 1; }
url=$(cat "$DATABASE_URL_FILE")
until psql "$url" -Atc 'select 1' >/dev/null 2>&1; do sleep 1; done
# Any failure ends the process, the way a server that cannot migrate exits,
# and the restart policy tries again.
psql "$url" -q -v ON_ERROR_STOP=1 -c 'create table if not exists e2e_migrations (version text primary key)' || exit 1
applied=$(psql "$url" -Atc 'select version from e2e_migrations') || exit 1
for m in $applied; do
    case " ${known.join(" ")} " in
        *" $m "*) ;;
        *) echo "e2e api ${spec.version}: migration $m was applied to the database but is missing in the resolved migrations" >&2; exit 1 ;;
    esac
done
if [ "$LAYSI_SERVE" = app ]; then
    psql "$url" -q -v ON_ERROR_STOP=1 -c "insert into e2e_migrations values ('${migration}') on conflict do nothing" || exit 1
fi
[ "${spec.api ?? "ok"}" = never-healthy ] || touch /tmp/ready
${SERVE}
`;
}

function composeFor(spec: Spec): string {
  let compose = prodCompose;
  if (spec.secretsExtra) {
    const anchor = "[ -s /secrets/setup_token ] || gen > /secrets/setup_token\n";
    if (!compose.includes(anchor)) fail("the secrets script changed; update the e2e anchor");
    compose = compose.replace(anchor, `${anchor}        [ -s /secrets/e2e_extra ] || gen > /secrets/e2e_extra\n`);
  }
  if (spec.rustfs) compose = compose.replace(RUSTFS, spec.rustfs);
  return compose;
}

function push(tag: string, context: string): string {
  podman("build", "-q", "--pull=never", "-t", tag, context);
  const digestFile = join(workDir, "digest");
  podman("push", "-q", "--tls-verify=false", "--digestfile", digestFile, tag);
  podman("rmi", tag);
  const digest = readFileSync(digestFile, "utf8").trim();
  if (!/^sha256:[0-9a-f]{64}$/.test(digest)) fail(`push of ${tag} gave ${digest}`);
  return digest;
}

/**
 * Pushes an index holding only the image `digest` names, the way a rebuild
 * that changes nothing comes back under a new index digest. Written to the
 * registry directly: `podman manifest push` without --all flattens a list
 * to the image it holds.
 */
async function rewrap(repo: string, digest: string, tag: string): Promise<string> {
  const [host, ...path] = repo.split("/");
  const manifests = `http://${host}/v2/${path.join("/")}/manifests`;
  const child = await fetch(`${manifests}/${digest}`, {
    headers: { Accept: "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json" },
  });
  if (!child.ok) fail(`reading ${repo}@${digest}: ${child.status}`);
  const mediaType = child.headers.get("content-type") ?? "";
  const size = (await child.arrayBuffer()).byteLength;
  const indexType = mediaType.includes("docker") ? "application/vnd.docker.distribution.manifest.list.v2+json" : "application/vnd.oci.image.index.v1+json";
  const architecture = process.arch === "arm64" ? "arm64" : "amd64";
  const body = JSON.stringify({ schemaVersion: 2, mediaType: indexType, manifests: [{ mediaType, digest, size, platform: { architecture, os: "linux" } }] });
  const put = await fetch(`${manifests}/${tag}`, { method: "PUT", headers: { "Content-Type": indexType }, body });
  if (!put.ok) fail(`pushing an index for ${repo}@${digest}: ${put.status} ${await put.text()}`);
  const index = `sha256:${sha256(body)}`;
  if (put.headers.get("docker-content-digest") !== index) fail(`the registry stored ${put.headers.get("docker-content-digest")}, not ${index}`);
  return index;
}

function buildRelease(spec: Spec, known: string[]): Release {
  const migration = `m-${spec.version}`;
  const dir = mkdtempSync(join(workDir, "release-"));
  const compose = composeFor(spec);
  writeFileSync(join(dir, "docker-compose.prod.yml"), compose);
  writeFileSync(join(dir, "docker-compose.ship.yml"), shipCompose);
  writeFileSync(join(dir, "laysi-server"), fakeServer(spec, migration, [...known, migration]));
  chmodSync(join(dir, "laysi-server"), 0o755);
  const updater = readFileSync(updaterPath);
  switch (spec.updater ?? "same") {
    case "same":
      writeFileSync(join(dir, "laysi-updater"), updater);
      break;
    case "changed":
      // Trailing bytes after an ELF image change its hash, not what runs.
      writeFileSync(join(dir, "laysi-updater"), Buffer.concat([updater, Buffer.from(`\n${spec.version}\n`)]));
      break;
    case "broken":
      writeFileSync(join(dir, "laysi-updater"), "#!/bin/sh\necho 'cannot read the journal' >&2\nexit 1\n");
      break;
  }
  chmodSync(join(dir, "laysi-updater"), 0o755);
  writeFileSync(
    join(dir, "Containerfile.api"),
    `FROM localhost/laysi-e2e-base
LABEL e2e.release=${spec.version}
COPY laysi-server /usr/local/bin/laysi-server
COPY docker-compose.prod.yml docker-compose.ship.yml laysi-updater /usr/local/share/laysi-one/
USER laysi
ENTRYPOINT ["/usr/local/bin/laysi-server"]
`,
  );
  writeFileSync(join(dir, "Containerfile.plain"), `FROM ${ALPINE}\nLABEL e2e.release=${spec.version}\nCMD ["sh", "-c", ${JSON.stringify(SERVE)}]\n`);
  const build = (name: string, file: string) => {
    podman("build", "-q", "--pull=never", "-f", join(dir, file), "-t", `${REGISTRY}/laysi-one-${name}:${spec.version}`, dir);
    const digestFile = join(dir, `${name}.digest`);
    podman("push", "-q", "--tls-verify=false", "--digestfile", digestFile, `${REGISTRY}/laysi-one-${name}:${spec.version}`);
    podman("rmi", `${REGISTRY}/laysi-one-${name}:${spec.version}`);
    return readFileSync(digestFile, "utf8").trim();
  };
  const digests = { api: build("api", "Containerfile.api"), web: build("web", "Containerfile.plain"), backup: spec.backup ?? build("backup", "Containerfile.plain") };
  const manifest = sign(spec.version, digests);
  const release = { spec, migration, digests, manifest, compose };
  releases.set(spec.version, release);
  return release;
}

function sign(version: string, digests: { api: string; web: string; backup: string }): string {
  const out = join(workDir, `manifest-${version}-${randomUUID()}`);
  sh(["bun", join(REPO, "scripts/release-manifest.ts"), "sign", "--version", version, "--api", digests.api, "--web", digests.web, "--backup", digests.backup, "--out", out], {
    env: { RELEASE_MANIFEST_SIGNING_KEY: TEST_SEED },
  });
  return readFileSync(out, "utf8");
}

// ---------------------------------------------------------------------------
// The machine as the test sees it

interface Result {
  [key: string]: string;
}

function readResult(id: string): Result | undefined {
  const path = join(RESULTS, id);
  if (!existsSync(path)) return undefined;
  const result: Result = {};
  for (const line of readFileSync(path, "utf8").split("\n").slice(1)) {
    const at = line.indexOf(" ");
    if (at > 0) result[line.slice(0, at)] = line.slice(at + 1);
  }
  return result;
}

function journalPhase(): string | undefined {
  const path = join(ROOT, "state/journal");
  if (!existsSync(path)) return undefined;
  return readFileSync(path, "utf8").match(/^phase (\S+)$/m)?.[1];
}

function switching(): boolean {
  return existsSync(MARKER);
}

/// A rollback ended in rollback-failed with a dump still to restore, which
/// retry-rollback restores: the switching marker stays until then.
function restorePending(): boolean {
  const path = join(ROOT, "state/journal");
  if (!existsSync(path)) return false;
  const journal = readFileSync(path, "utf8");
  return /^phase rollback-failed$/m.test(journal) && /^restore-db yes$/m.test(journal) && backups().length > 0;
}

function installed(): string {
  return readFileSync(join(ROOT, "state/installed"), "utf8").match(/^version (\S+)$/m)?.[1] ?? "";
}

interface Ctr {
  name: string;
  service: string;
  running: boolean;
  image: string;
  imageId: string;
  digest: string;
  health: string;
  restart: string;
  started: string;
  created: string;
}

function containers(): Ctr[] {
  // While the updater recreates containers, one listed here can be gone by
  // the time it is inspected: list again.
  let inspected: any[] | undefined;
  for (let attempt = 0; inspected === undefined; attempt++) {
    const ids = podman("ps", "-a", "-q", "--filter", `label=io.podman.compose.project=${PROJECT}`).split("\n").filter(Boolean);
    if (ids.length === 0) return [];
    const run = sh(["podman", "container", "inspect", ...ids], { ok: attempt < 4 ? false : undefined });
    if (run.code === 0) inspected = JSON.parse(run.out);
  }
  return (inspected ?? []).map((c: any) => ({
    name: c.Name,
    service: c.Config?.Labels?.["com.docker.compose.service"] ?? "",
    running: c.State?.Running === true,
    image: c.ImageName,
    imageId: c.Image ?? "",
    digest: c.ImageDigest,
    health: (c.State?.Health ?? c.State?.Healthcheck)?.Status ?? "",
    restart: c.HostConfig?.RestartPolicy?.Name ?? "",
    started: c.State?.StartedAt ?? "",
    created: c.Created ?? "",
  }));
}

function byService(): Map<string, Ctr> {
  const map = new Map<string, Ctr>();
  for (const c of containers()) {
    const seen = map.get(c.service);
    if (!seen || c.created > seen.created) map.set(c.service, c);
  }
  return map;
}

/** Whether local storage holds an image pulled by this digest. */
function hasImage(digest: string): boolean {
  return JSON.parse(podman("images", "--format", "json")).some((i: any) => (i.RepoDigests ?? []).some((d: string) => d.endsWith(`@${digest}`)));
}

/** Whether a container was created from this digest and runs an image local storage knows by it. */
function runs(c: Ctr, digest: string): boolean {
  if (!c.image.endsWith(`@${digest}`)) return false;
  const image = JSON.parse(podman("image", "inspect", c.imageId))[0];
  return image?.Digest === digest || (image?.RepoDigests ?? []).some((d: string) => d.endsWith(`@${digest}`));
}

function psql(sql: string): string {
  return podman("exec", `${PROJECT}-postgres`, "psql", "-U", "laysi", "-d", "laysi", "-Atc", sql);
}

function migrations(): string[] {
  return psql("select version from e2e_migrations order by version").split("\n").filter(Boolean);
}

function requestsLeft(): string[] {
  // The directory belongs to the api's uid; listing it is allowed.
  return readdirSync(REQUESTS);
}

function backups(): string[] {
  return readdirSync(BACKUPS);
}

function encodeRequest(id: string, manifest: string): string {
  return `laysi-update-request 1\nrequest ${id}\n${manifest}`;
}

/// What the product does when the administrator presses the button: write a
/// temporary file in the requests directory and rename it, inside the api.
function writeRequest(id: string, manifest: string) {
  sh(["podman", "exec", "-i", `${PROJECT}-api`, "sh", "-c", "cat > /var/lib/laysi-update/requests/.incoming && mv /var/lib/laysi-update/requests/.incoming /var/lib/laysi-update/requests/request"], {
    stdin: encodeRequest(id, manifest),
  });
}

function unitState(unit: string, property = "ActiveState"): string {
  return systemctl("show", "-p", property, "--value", unit).out.trim();
}

async function settledRun(): Promise<void> {
  await waitFor("the update service to finish and the requests directory to empty", 1800, () => {
    const state = unitState("laysi-updater.service");
    const others = ["laysi-updater-recover.service", "laysi-updater-fallback.service"].map((u) => unitState(u));
    return state !== "active" && state !== "activating" && state !== "deactivating" && others.every((s) => s !== "activating" && s !== "active") && requestsLeft().length === 0;
  });
}

async function update(id: string, manifest: string, seconds = 1800, during?: () => Promise<void>): Promise<Result> {
  writeRequest(id, manifest);
  if (during) await during();
  const result = await waitFor(`a final result for ${id}`, seconds, () => {
    const r = readResult(id);
    return r && FINAL.has(r.status) ? r : undefined;
  });
  await settledRun();
  log(`${id}: ${result.status}${result.reason ? ` (${result.reason})` : ""}${result["rollback-reason"] ? ` / ${result["rollback-reason"]}` : ""}`);
  if (result.tools) log(`${id}: ${result.tools}`);
  if (restorePending()) check(switching(), "the switching marker stays while the dump is still to be restored");
  else check(!switching(), `no switching marker is left after ${result.status}`);
  return result;
}

// The ports docker-compose.prod.yml publishes by default. Rootless podman
// forwards each with a process that stays in the cgroup of the unit that
// started the container, and dies with it.
const PUBLISHED: Record<string, number> = { web: 8088, mcp: 8089 };

/// A TCP connection to the host port opens. The fakes serve nothing, but the
/// forwarder listens on the host whatever is behind it, so this says whether
/// the forwarder is alive.
function accepts(port: number): boolean {
  return sh(["timeout", "5", "bash", "-c", `exec 3<>/dev/tcp/127.0.0.1/${port}`], { ok: false }).code === 0;
}

/// Every service of the compose project running, all on the release the
/// host records, reachable on the ports it publishes, nothing left over.
function assertSettledOn(version: string) {
  const release = releases.get(version);
  if (!release) fail(`unknown release ${version}`);
  check(installed() === version, `the host records ${version}`);
  const services = byService();
  const own: Record<string, string> = { api: release.digests.api, mcp: release.digests.api, web: release.digests.web, backup: release.digests.backup };
  for (const service of ["postgres", "rustfs", "api", "mcp", "backup", "web"]) {
    const c = services.get(service);
    if (!c?.running) fail(`${service} is not running on ${version}`);
    if (own[service] && !runs(c, own[service])) fail(`${service} runs ${c.image} (${c.digest}), not ${version}'s ${own[service]}`);
    if (c.health && c.health !== "healthy") fail(`${service} is ${c.health}`);
  }
  for (const [service, port] of Object.entries(PUBLISHED)) {
    check(accepts(port), `${service}'s published port ${port} accepts connections`);
  }
  const rustfs = release.spec.rustfs ?? RUSTFS;
  const rustfsCtr = services.get("rustfs");
  check(!!rustfsCtr && runs(rustfsCtr, rustfs.split("@")[1]), `rustfs runs the image ${version} names`);
  check(migrations().at(-1) === release.migration || migrations().includes(release.migration), `the database carries ${version}'s migration`);
  check(requestsLeft().length === 0, "the requests directory is empty");
  check(journalPhase() === undefined, "no journal is left");
  check(!switching(), "no switching marker is left");
  check(unitState("laysi-updater.path") === "active", "the path unit is still watching");
}

/// The power goes: every container dies where it stands, then the machine
/// boots and the boot unit runs, the only thing that starts the stack.
function powerCutAndBoot(): Run {
  const ids = podman("ps", "-q", "--filter", `label=io.podman.compose.project=${PROJECT}`).split("\n").filter(Boolean);
  if (ids.length) sh(["podman", "kill", ...ids], { ok: false });
  systemctl("stop", "laysi-updater-boot.service");
  systemctl("reset-failed", ...UNITS);
  return systemctl("start", "laysi-updater-boot.service");
}

function startEvents(since: string): { name: string; status: string; health?: string; time: string; image: string }[] {
  const out = sh(["podman", "events", "--since", since, "--until", "0s", "--filter", "type=container", "--format", "json"]).out;
  return out
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l))
    .map((e) => ({ name: e.Name, status: e.Status, health: e.health_status, time: e.Time, image: e.Image }));
}

/// web and mcp started only after api and the rest of the release had passed.
function assertEntriesLast(since: string, version: string) {
  const release = releases.get(version)!;
  const events = startEvents(since);
  const apiHealthy = events.find((e) => e.name === `${PROJECT}-api` && e.status === "health_status" && e.health === "healthy" && e.image.includes(release.digests.api));
  const entryStarts = events.filter((e) => e.status === "start" && (e.name === `${PROJECT}-web` || e.name === `${PROJECT}-mcp`));
  for (const start of entryStarts) {
    if (!apiHealthy || start.time < apiHealthy.time) {
      fail(`${start.name} started at ${start.time}, before api on ${version} was healthy (${apiHealthy?.time ?? "never"})`);
    }
  }
  console.log(`  ok  web and mcp started only after api on ${version} passed`);
}

function assertEntriesNeverOn(since: string, version: string) {
  const release = releases.get(version)!;
  const started = startEvents(since).filter(
    (e) => e.status === "start" && (e.name === `${PROJECT}-web` || e.name === `${PROJECT}-mcp`) && (e.image.includes(release.digests.web) || e.image.includes(release.digests.api)),
  );
  check(started.length === 0, `web and mcp never ran on ${version}`);
}

/// Kill the update once the journal is in one of `phases`. Some last well
/// under a second, so the journal is polled often and a group is given.
async function killAt(phases: string[], id: string, manifest: string) {
  writeRequest(id, manifest);
  const phase = await waitFor(`the journal to reach ${phases.join(" or ")}`, 1200, () => {
    const now = journalPhase();
    return now && phases.includes(now) ? now : undefined;
  }, 20);
  systemctl("kill", "--signal=SIGKILL", "laysi-updater.service");
  log(`killed the update in ${phase}`);
}

// ---------------------------------------------------------------------------
// Setup

function preflight() {
  check(process.platform === "linux", "running on Linux");
  check(!!process.env.XDG_RUNTIME_DIR, "XDG_RUNTIME_DIR is set");
  const manager = systemctl("show-environment");
  check(manager.code === 0, "a systemd user manager answers");
  const linger = sh(["loginctl", "show-user", process.env.USER ?? "", "--property=Linger", "--value"], { ok: false }).out.trim();
  check(linger === "yes", "the user lingers");
  log(sh(["podman", "--version"]).out.trim());
  log(sh(["podman-compose", "--version"]).out.trim().split("\n").join(" / "));
  // Services find each other by name; CNI on Ubuntu has no DNS plugin.
  check(podman("info", "--format", "{{.Host.NetworkBackend}}") === "netavark", "podman uses netavark");
  log(sh([updaterPath, "version"]).out.trim());
  check(sh([updaterPath, "version"]).out.includes("test keys"), "the updater under test trusts the test key");
  check(!existsSync(join(ROOT, "state/installed")), `${ROOT} is not installed yet`);
}

function setupRegistry() {
  const conf = join(HOME, ".config/containers/registries.conf");
  mkdirSync(join(HOME, ".config/containers"), { recursive: true });
  const entry = `[[registry]]\nlocation = "localhost:${REGISTRY_PORT}"\ninsecure = true\n`;
  const current = existsSync(conf) ? readFileSync(conf, "utf8") : "";
  if (!current.includes(`location = "localhost:${REGISTRY_PORT}"`)) writeFileSync(conf, `${current}\n${entry}`);
  sh(["podman", "rm", "-f", "laysi-e2e-registry"], { ok: false });
  podman("run", "-d", "--name", "laysi-e2e-registry", "-p", `127.0.0.1:${REGISTRY_PORT}:5000`, "docker.io/library/registry:2");
}

function buildBase() {
  const dir = mkdtempSync(join(workDir, "base-"));
  writeFileSync(
    join(dir, "Containerfile"),
    `FROM ${ALPINE}\nRUN apk add --no-cache postgresql-client && adduser -D -u 10001 laysi\n`,
  );
  podman("pull", "-q", ALPINE);
  podman("build", "-q", "-t", "localhost/laysi-e2e-base", dir);
}

// ---------------------------------------------------------------------------
// The scenarios

const downtimes: { version: string; seconds: number }[] = [];

function downtime(result: Result, version: string) {
  const from = Date.parse(result["downtime-from"]);
  const to = Date.parse(result["downtime-to"]);
  if (Number.isFinite(from) && Number.isFinite(to)) {
    downtimes.push({ version, seconds: (to - from) / 1000 });
    log(`downtime of ${version}: ${(to - from) / 1000} s (step 4 to the end of step 8)`);
  }
}

async function main() {
  const { values, positionals } = parseArgs({
    args: Bun.argv.slice(2),
    options: { updater: { type: "string" }, yes: { type: "boolean" }, keep: { type: "boolean" } },
    allowPositionals: true,
  });
  const command = positionals[0];
  if (command === "public-key") {
    const out = sh(["bun", join(REPO, "scripts/release-manifest.ts"), "public-key"], { env: { RELEASE_MANIFEST_SIGNING_KEY: TEST_SEED } }).out;
    console.log(out.trim().split(/\s+/)[0]);
    return;
  }
  if (command !== "run" || !values.updater || !values.yes) {
    fail("usage: bun scripts/updater-e2e.ts public-key | run --updater <binary> --yes");
  }
  updaterPath = resolve(values.updater);
  workDir = mkdtempSync(join(tmpdir(), "laysi-updater-e2e-"));

  preflight();
  setupRegistry();
  buildBase();

  // --- install ------------------------------------------------------------
  const known: string[] = [];
  const v1 = buildRelease({ version: "v1.0.0" }, known);
  known.push(v1.migration);
  const manifestFile = join(workDir, "v1.manifest");
  writeFileSync(manifestFile, v1.manifest);
  const installer = join(workDir, "laysi-updater");
  copyFileSync(updaterPath, installer);
  chmodSync(installer, 0o755);
  log("install v1.0.0");
  sh([installer, "install", "--manifest", manifestFile, "--registry", REGISTRY]);
  assertSettledOn("v1.0.0");
  for (const unit of ["laysi-updater.path", "laysi-updater-boot.service"]) {
    check(unitState(unit, "UnitFileState") === "enabled", `${unit} is enabled`);
  }
  check(containers().every((c) => c.restart !== "always"), "no container restarts on its own at boot (restart: always)");

  // podman's own boot unit starts restart: always and nothing else, so it
  // never runs ahead of the updater's recovery.
  const stackRunning = () => podman("ps", "-q", "--filter", `label=io.podman.compose.project=${PROJECT}`).split("\n").filter(Boolean);
  sh(["podman", "kill", ...stackRunning()], { ok: false });
  systemctl("restart", "podman-restart.service");
  check(stackRunning().length === 0, "podman-restart.service starts none of the stack");

  // The power goes right after the install: the boot unit alone brings it back.
  check(powerCutAndBoot().code === 0, "the boot unit starts the stack after a power cut");
  assertSettledOn("v1.0.0");

  // A broken release would otherwise hold the test for the 20 minutes a
  // shipped machine gives migrations.
  const toolEnv = join(ROOT, "state/updater.env");
  writeFileSync(toolEnv, `${readFileSync(toolEnv, "utf8")}LAYSI_UPDATER_HEALTH_TIMEOUT_SECS=240\nLAYSI_UPDATER_ENTRY_TIMEOUT_SECS=120\n`);

  psql("create table if not exists e2e_rows (note text)");
  psql("insert into e2e_rows values ('written before any update')");

  // --- never updated, and the current updater cannot run --------------------
  // Until an update replaces the updater, the copy install leaves as the
  // previous one is the only other updater the machine has.
  const bin = join(ROOT, "bin");
  const shipped = readFileSync(join(bin, "laysi-updater"));
  check(sha256(shipped) === sha256(readFileSync(installer)), "install puts itself in place as the current updater");
  check(sha256(readFileSync(join(bin, "laysi-updater.previous"))) === sha256(shipped), "and as the previous one");
  const breakages: [string, Buffer][] = [
    ["exits non-zero", Buffer.from("#!/bin/sh\nexit 1\n")],
    ["is truncated", shipped.subarray(0, 4096)],
  ];
  for (const [what, broken] of breakages) {
    writeFileSync(join(bin, "laysi-updater"), broken);
    const before = unitState("laysi-updater-fallback.service", "InvocationID");
    powerCutAndBoot();
    await waitFor(`the fallback unit to run after the current updater that ${what}`, 900, () =>
      unitState("laysi-updater-fallback.service", "InvocationID") !== before &&
      !["activating", "active"].includes(unitState("laysi-updater-fallback.service")));
    check(unitState("laysi-updater-boot.service", "Result") !== "success", `the current updater that ${what} failed at boot`);
    check(unitState("laysi-updater-fallback.service", "Result") === "success", "the previous updater started the stack instead");
    assertSettledOn("v1.0.0");
  }
  // A request it cannot read: the previous updater's ExecStopPost answers it
  // and clears it, or the path unit would start the run again and again.
  const freshFallback = unitState("laysi-updater-fallback.service", "InvocationID");
  const fresh = await update("e2e-fresh-broken-run", v1.manifest, 300);
  check(fresh.status === "failed-unchanged" && (fresh.reason ?? "").includes("開始更新前"), "a request the current updater cannot run is answered as not started");
  const runOnce = unitState("laysi-updater.service", "InvocationID");
  await sleep(5000);
  check(unitState("laysi-updater.service", "InvocationID") === runOnce && requestsLeft().length === 0, "and the run is not started again");
  check(unitState("laysi-updater.path") === "active", "the path unit is still watching");
  await waitFor("the fallback unit to run after the failed recovery", 300, () =>
    unitState("laysi-updater-fallback.service", "InvocationID") !== freshFallback &&
    !["activating", "active"].includes(unitState("laysi-updater-fallback.service")));
  check(installed() === "v1.0.0", "and nothing was installed");
  writeFileSync(join(bin, "laysi-updater"), shipped);
  systemctl("reset-failed", ...UNITS);
  assertSettledOn("v1.0.0");

  // --- the api's and mcp's side of the directories ---------------------------
  const api = `${PROJECT}-api`;
  const mcp = `${PROJECT}-mcp`;
  check(sh(["podman", "exec", api, "touch", "/var/lib/laysi-update/results/x"], { ok: false }).code !== 0, "the api cannot write into the results directory");
  check(sh(["podman", "exec", mcp, "test", "-d", "/var/lib/laysi-update/results"], { ok: false }).code === 0, "mcp mounts the results directory");
  check(sh(["podman", "exec", mcp, "touch", "/var/lib/laysi-update/results/x"], { ok: false }).code !== 0, "mcp cannot write into the results directory");
  check(sh(["podman", "exec", mcp, "test", "-e", "/var/lib/laysi-update/requests"], { ok: false }).code !== 0, "mcp has no requests directory, so it cannot ask for an update");
  sh(["podman", "exec", api, "ln", "-s", "/etc/passwd", "/var/lib/laysi-update/requests/request"]);
  await settledRun();
  check(requestsLeft().length === 0, "a symlinked request is cleared without being followed");
  sh(["podman", "exec", api, "mkfifo", "/var/lib/laysi-update/requests/request"]);
  await settledRun();
  check(requestsLeft().length === 0, "a FIFO request is cleared without the updater hanging");
  sh(["podman", "exec", api, "sh", "-c", "mkdir -p /var/lib/laysi-update/requests/request && ln -s /etc /var/lib/laysi-update/requests/request/etc"]);
  await settledRun();
  check(requestsLeft().length === 0, "a directory in place of the request is cleared");
  check(readFileSync("/etc/passwd", "utf8").length > 0, "nothing outside the directory was touched");

  // --- refusals --------------------------------------------------------------
  let r = await update("e2e-same-version", v1.manifest);
  check(r.status === "refused-same-version" && r.running === "v1.0.0", "the same version is refused and answered");
  check(/^podman=\S+ podman-compose=\S+$/.test(r.tools ?? ""), `the result records the tools the run used (${r.tools})`);
  check(sh(["podman", "exec", mcp, "test", "-f", "/var/lib/laysi-update/results/e2e-same-version"], { ok: false }).code === 0, "mcp reads the same results the api does");
  const tampered = v1.manifest.replace(/signature (\S+) (\S)/, (_m, id, c) => `signature ${id} ${c === "0" ? "1" : "0"}`);
  r = await update("e2e-bad-signature", tampered);
  check(r.status === "refused-signature", "a manifest whose signature does not verify is refused");
  const older = sign("v0.9.0", v1.digests);
  r = await update("e2e-downgrade-0", older);
  check(r.status === "refused-downgrade", "an older release is refused");

  const v2 = buildRelease({ version: "v1.1.0", api: "exit" }, known);
  const tools = readFileSync(toolEnv, "utf8");
  writeFileSync(toolEnv, `${tools}LAYSI_UPDATER_DISK_RESERVE_BYTES=${2n ** 60n}\n`);
  const before = containers();
  r = await update("e2e-disk-space", v2.manifest);
  writeFileSync(toolEnv, tools);
  check(r.status === "refused-disk-space", "too little disk is refused");
  check(!r["downtime-from"] && JSON.stringify(containers().map((c) => c.created)) === JSON.stringify(before.map((c) => c.created)), "nothing was stopped or recreated before the refusal");
  check(backups().length === 0, "no backup was taken");

  // --- broken releases roll back with the database -------------------------
  let since = new Date().toISOString();
  r = await update("e2e-api-exits", v2.manifest);
  check(r.status === "rolled-back" && r.running === "v1.0.0", "an api that exits at start is rolled back");
  assertSettledOn("v1.0.0");
  assertEntriesNeverOn(since, "v1.1.0");
  check(psql("select count(*) from e2e_rows") === "1", "the row written before step 4 is still there");
  check(backups().length === 0, "the database backup is deleted after the rollback");

  const v3 = buildRelease({ version: "v1.2.0", api: "never-healthy" }, known);
  since = new Date().toISOString();
  r = await update("e2e-never-healthy", v3.manifest, 1800, async () => {
    // The new api runs, never passing, for the whole health timeout: the
    // marker is there, and the api sees it, while its writes could still be
    // taken back.
    await waitFor("the new api to run", 600, () => journalPhase() === "switching" && byService().get("api")?.running && byService().get("api")?.digest === v3.digests.api);
    check(switching(), "the switching marker is there while the new release comes up");
    check(readResult("e2e-never-healthy")?.status === "switching", "and the result says switching");
    check(sh(["podman", "exec", api, "test", "-e", CONTAINER_MARKER], { ok: false }).code === 0, "the api running on the new release sees the marker");
  });
  check(r.status === "rolled-back", "a release whose healthcheck never passes is rolled back");
  assertSettledOn("v1.0.0");
  assertEntriesNeverOn(since, "v1.2.0");
  check(!migrations().includes(v3.migration), "its migration went back out with the restore");
  check(psql("select count(*) from e2e_rows") === "1", "the data written before step 4 survived the restore");
  check(backups().length === 0, "the database backup is deleted after the rollback");

  // --- the first restore of a rollback fails: retried once, at once ---------
  // The updater's podman is swapped for one that fails the next restore,
  // the way pg_restore fails on a full disk, and runs everything else.
  const envBeforeRetry = readFileSync(toolEnv, "utf8");
  const realPodman = envBeforeRetry.match(/^LAYSI_PODMAN=(\S+)$/m)?.[1];
  check(!!realPodman, "updater.env names the podman the updater runs");
  const failOnce = join(workDir, "fail-next-restore");
  const failingPodman = join(workDir, "podman-failing-restore");
  writeFileSync(
    failingPodman,
    `#!/bin/sh
case "$*" in
*"pg_restore --create"*)
  if [ -e ${failOnce} ]; then
    rm -f ${failOnce}
    cat > /dev/null
    echo 'pg_restore: error: could not write to file: No space left on device' >&2
    exit 1
  fi ;;
esac
exec ${realPodman} "$@"
`,
  );
  chmodSync(failingPodman, 0o755);
  writeFileSync(failOnce, "");
  writeFileSync(toolEnv, `${envBeforeRetry}LAYSI_PODMAN=${failingPodman}\n`);
  const vRetry = buildRelease({ version: "v1.2.1", api: "exit" }, known);
  r = await update("e2e-restore-retry", vRetry.manifest);
  writeFileSync(toolEnv, envBeforeRetry);
  check(!existsSync(failOnce), "the first restore of the rollback failed");
  check(r.status === "rolled-back" && r.running === "v1.0.0", "a rollback whose first restore fails restores again and is rolled back");
  check(/No space left on device/.test(r["rollback-reason"] ?? "") && /重試後成功/.test(r["rollback-reason"] ?? ""), "rollback-reason says why the first restore failed and that the retry succeeded");
  assertSettledOn("v1.0.0");
  check(!migrations().includes(vRetry.migration), "the retried restore took the new migration back out");
  check(psql("select count(*) from e2e_rows") === "1", "the data written before step 4 survived the retried restore");
  check(backups().length === 0, "the dump is deleted once the retry restored it");
  const retryImages = JSON.parse(podman("images", "--format", "json")).filter((i: any) => (i.RepoDigests ?? []).some((d: string) => Object.values(vRetry.digests).some((x) => d.endsWith(x))));
  check(retryImages.length === 0, "the new release's images are gone");

  // --- the restore fails twice: rollback-failed, then retry-rollback --------
  // Every restore fails while the file is there, the way a full disk would
  // until someone frees it.
  const failRestores = join(workDir, "fail-restores");
  const podmanFailingRestores = join(workDir, "podman-failing-restores");
  writeFileSync(
    podmanFailingRestores,
    `#!/bin/sh
case "$*" in
*"pg_restore --create"*)
  if [ -e ${failRestores} ]; then
    cat > /dev/null
    echo 'pg_restore: error: could not write to file: No space left on device' >&2
    exit 1
  fi ;;
esac
exec ${realPodman} "$@"
`,
  );
  chmodSync(podmanFailingRestores, 0o755);
  writeFileSync(failRestores, "");
  writeFileSync(toolEnv, `${envBeforeRetry}LAYSI_PODMAN=${podmanFailingRestores}\n`);
  const vStuck = buildRelease({ version: "v1.2.2", api: "exit" }, known);
  r = await update("e2e-rollback-stuck", vStuck.manifest);
  check(r.status === "rollback-failed", "a rollback whose restore fails twice ends in rollback-failed");
  check(journalPhase() === "rollback-failed" && backups().length === 1, "the journal stays and the dump is kept");
  powerCutAndBoot();
  await settledRun();
  const up = [...byService().values()].filter((c) => c.running).map((c) => c.service);
  check(up.length === 1 && up[0] === "postgres", "a boot starts postgres alone");
  check(switching(), "and the switching marker stays");
  check(sh(["podman", "exec", `${PROJECT}-postgres`, "psql", "-U", "laysi", "-d", "laysi", "-Atc", "select 1"], { ok: false }).code !== 0, "the database is gone");
  // The disk is freed; the updater runs the podman it was installed with.
  rmSync(failRestores);
  writeFileSync(toolEnv, envBeforeRetry);
  const retried = sh([join(ROOT, "bin/laysi-updater"), "retry-rollback"]);
  log(retried.out.trim());
  r = readResult("e2e-rollback-stuck")!;
  check(r.status === "rolled-back" && r.running === "v1.0.0", "retry-rollback restores the dump and rolls back to the release before the update");
  check(/人工指令重新還原/.test(r["rollback-reason"] ?? ""), "rollback-reason says the restore was retried by hand");
  assertSettledOn("v1.0.0");
  check(!migrations().includes(vStuck.migration), "the new release's migration is not in the restored database");
  check(psql("select count(*) from e2e_rows") === "1", "the data written before step 4 is back");
  check(backups().length === 0, "the dump is deleted once restored");
  // The next update, a good release, follows right below.

  // --- a good release, with a new secret and a new updater ------------------
  const secretsBefore = podman("exec", api, "sh", "-c", "sha256sum /secrets/pg_password /secrets/setup_token /secrets/database_url");
  const updaterBefore = sha256(readFileSync(join(ROOT, "bin/laysi-updater")));
  const v4 = buildRelease({ version: "v1.3.0", secretsExtra: true, updater: "changed" }, known);
  known.push(v4.migration);
  since = new Date().toISOString();
  r = await update("e2e-good-1", v4.manifest);
  check(r.status === "succeeded" && r.running === "v1.3.0", "a good release is installed");
  downtime(r, "v1.3.0");
  assertSettledOn("v1.3.0");
  assertEntriesLast(since, "v1.3.0");
  check(podman("exec", api, "sh", "-c", "test -s /secrets/e2e_extra && echo yes") === "yes", "the new release's secrets generated its new secret");
  check(podman("exec", api, "sh", "-c", "sha256sum /secrets/pg_password /secrets/setup_token /secrets/database_url") === secretsBefore, "the existing secrets are unchanged");
  check(sha256(readFileSync(join(ROOT, "bin/laysi-updater.previous"))) === updaterBefore, "the previous updater is kept beside the new one");
  check(sha256(readFileSync(join(ROOT, "bin/laysi-updater"))) !== updaterBefore, "the updater was replaced with the new release's");
  check(backups().length === 0, "the database backup is deleted after success");
  const ours = JSON.parse(podman("images", "--format", "json")).filter((i: any) => (i.RepoDigests ?? []).some((d: string) => d.startsWith(`${REGISTRY}/`)));
  const allowed = new Set([...Object.values(v1.digests), ...Object.values(v4.digests)]);
  check(
    ours.every((i: any) => (i.RepoDigests ?? []).some((d: string) => allowed.has(d.split("@")[1]))),
    "only the current and the previous release's images are left",
  );

  // --- a release that only changes rustfs -----------------------------------
  // Squashed, so the pinned rustfs is not its parent image: podman does not
  // remove an image that another one is built on, and the clean-up below has
  // to be able to remove the pinned one.
  const rustfsDir = mkdtempSync(join(workDir, "rustfs-"));
  writeFileSync(join(rustfsDir, "Containerfile"), `FROM ${RUSTFS}\nLABEL e2e.rustfs=2\n`);
  podman("pull", "-q", RUSTFS);
  podman("build", "-q", "--pull=never", "--squash-all", "-t", `localhost:${REGISTRY_PORT}/rustfs/rustfs:e2e`, rustfsDir);
  const rustfsFile = join(rustfsDir, "digest");
  podman("push", "-q", "--tls-verify=false", "--digestfile", rustfsFile, `localhost:${REGISTRY_PORT}/rustfs/rustfs:e2e`);
  podman("rmi", `localhost:${REGISTRY_PORT}/rustfs/rustfs:e2e`);
  const newRustfs = `localhost:${REGISTRY_PORT}/rustfs/rustfs@${readFileSync(rustfsFile, "utf8").trim()}`;
  const v5 = buildRelease({ version: "v1.4.0", rustfs: newRustfs }, known);
  known.push(v5.migration);
  r = await update("e2e-rustfs-only", v5.manifest);
  check(r.status === "succeeded", "a release that only changes rustfs is installed");
  downtime(r, "v1.4.0");
  assertSettledOn("v1.4.0");
  const [pinnedRustfs, movedRustfs] = [RUSTFS, newRustfs].map((ref) => ref.split("@")[1]);
  check(hasImage(pinnedRustfs) && hasImage(movedRustfs), "the previous release's rustfs is kept beside the new one");

  // --- a new updater that fails its self-check ------------------------------
  const updaterNow = sha256(readFileSync(join(ROOT, "bin/laysi-updater")));
  const v6 = buildRelease({ version: "v1.5.0", rustfs: newRustfs, updater: "broken" }, known);
  known.push(v6.migration);
  r = await update("e2e-broken-updater", v6.manifest);
  check(r.status === "succeeded-updater-kept" && (r.reason ?? "").includes("自我檢查"), "a new updater that fails its self-check is not installed, and the result says why");
  check(sha256(readFileSync(join(ROOT, "bin/laysi-updater"))) === updaterNow, "the running updater stays");
  assertSettledOn("v1.5.0");
  // v1.3.0 ran the pinned rustfs from another repository; neither v1.4.0 nor
  // v1.5.0 names it.
  check(!hasImage(pinnedRustfs) && hasImage(movedRustfs), "a third-party image two releases old is removed, the current one kept");

  // --- the current updater fails at boot: the previous one starts the stack -
  const current = join(ROOT, "bin/laysi-updater");
  const saved = readFileSync(current);
  writeFileSync(current, "#!/bin/sh\nexit 1\n");
  powerCutAndBoot();
  await waitFor("the fallback unit to start the stack", 900, () => {
    const s = unitState("laysi-updater-fallback.service", "Result");
    const done = !["activating", "active"].includes(unitState("laysi-updater-fallback.service"));
    return done && s === "success";
  });
  check(unitState("laysi-updater-boot.service", "Result") !== "success", "the current updater failed at boot");
  check(existsSync(FALLBACK) && readFileSync(FALLBACK, "utf8").startsWith("at "), "the previous updater left the fallback marker");
  check((statSync(FALLBACK).mode & 0o777) === 0o644, "readable by the api's uid");
  check(sh(["podman", "exec", api, "test", "-r", CONTAINER_FALLBACK], { ok: false }).code === 0, "and the api sees it");
  // A request it cannot run either: the previous updater's ExecStopPost
  // answers it as never started, which busy would not be.
  const v7 = buildRelease({ version: "v1.6.0", rustfs: newRustfs }, known);
  known.push(v7.migration);
  const fallbackBefore = unitState("laysi-updater-fallback.service", "InvocationID");
  r = await update("e2e-broken-run", v7.manifest, 300);
  check(r.status === "failed-unchanged" && (r.reason ?? "").includes("開始更新前"), "a request the current updater cannot run is answered as not started, not busy");
  await waitFor("the fallback unit to run after the failed recovery", 300, () => {
    const again = unitState("laysi-updater-fallback.service", "InvocationID") !== fallbackBefore;
    return again && !["activating", "active"].includes(unitState("laysi-updater-fallback.service"));
  });
  check(installed() === "v1.5.0", "and nothing was installed");
  check(existsSync(FALLBACK), "the fallback marker stays while the current updater keeps failing");
  // A working updater again: its next start removes the marker.
  writeFileSync(current, saved);
  const restarted = powerCutAndBoot();
  check(restarted.code === 0 && !existsSync(FALLBACK), "the current updater's next start removes the fallback marker");
  assertSettledOn("v1.5.0");
  systemctl("reset-failed", ...UNITS);

  // --- the lock -------------------------------------------------------------
  // -o: only flock itself holds the lock, so killing it releases it.
  const holder = Bun.spawn(["flock", "-o", join(ROOT, "state/lock"), "sleep", "30"]);
  await sleep(1000);
  r = await update("e2e-locked-out", v7.manifest, 25);
  holder.kill();
  check(r.status === "refused-busy", "a request while another run holds the lock is refused");
  check(installed() === "v1.5.0", "and nothing was installed");

  // A request written while an update runs is dropped, not queued; a boot-time
  // run started meanwhile waits for the lock instead of running alongside.
  writeRequest("e2e-first-0001", v7.manifest);
  await waitFor("the update to stop services", 600, () => journalPhase() === "backed-up" || journalPhase() === "files-saved" || journalPhase() === "switching");
  sh(["podman", "exec", "-i", api, "true"], { ok: false });
  systemctl("start", "--no-block", "laysi-updater-recover.service");
  const v8 = buildRelease({ version: "v1.7.0", rustfs: newRustfs }, known);
  // The api is stopped by now; write the second request as the api would.
  sh(["podman", "unshare", "sh", "-c", `printf '%s' "$0" > ${REQUESTS}/.second && mv ${REQUESTS}/.second ${REQUESTS}/request`, encodeRequest("e2e-second-001", v8.manifest)]);
  const first = await waitFor("the first request's result", 1800, () => {
    const x = readResult("e2e-first-0001");
    return x && FINAL.has(x.status) ? x : undefined;
  });
  await settledRun();
  check(first.status === "succeeded", "the first request was installed");
  downtime(first, "v1.6.0");
  check(readResult("e2e-second-001")?.status === "refused-busy", "the request written during the run was refused, not queued");
  assertSettledOn("v1.6.0");
  known.push(v8.migration);

  // --- interrupted runs -----------------------------------------------------
  // Killed after the switch: the recovery unit finishes the update.
  await killAt(["switching", "core-up"], "e2e-kill-switch", v8.manifest);
  const killed = await waitFor("the recovery to answer", 1800, () => {
    const x = readResult("e2e-kill-switch");
    return x && FINAL.has(x.status) ? x : undefined;
  });
  await settledRun();
  check(["succeeded", "rolled-back"].includes(killed.status), `an update killed after the switch ends on one release (${killed.status})`);
  check(!switching(), "the recovery removed the switching marker");
  assertSettledOn(installed());

  // Killed while restoring the database: the recovery restores it again.
  const v9 = buildRelease({ version: "v1.8.0", api: "never-healthy", rustfs: newRustfs }, known);
  await killAt(["rolling-back", "restoring"], "e2e-kill-restore", v9.manifest);
  const restored = await waitFor("the recovery to answer", 1800, () => {
    const x = readResult("e2e-kill-restore");
    return x && FINAL.has(x.status) ? x : undefined;
  });
  await settledRun();
  check(restored.status === "rolled-back", "a rollback killed while restoring is finished");
  check(!switching(), "the recovery removed the switching marker");
  check(!migrations().includes(v9.migration), "and the database is the one from before the update");
  const settledVersion = installed();
  assertSettledOn(settledVersion);

  // A power cut before the switch: nothing recovers until the machine boots.
  systemctl("mask", "--runtime", "laysi-updater-recover.service");
  const v10 = buildRelease({ version: "v1.9.0", rustfs: newRustfs }, known);
  await killAt(["stopping", "backed-up", "files-saved", "files-swapped"], "e2e-power-cut-1", v10.manifest);
  await sleep(2000);
  check(switching(), "the switching marker outlives an update cut off mid-switch");
  systemctl("unmask", "--runtime", "laysi-updater-recover.service");
  check(powerCutAndBoot().code === 0, "the boot after a power cut mid-update succeeds");
  check(readResult("e2e-power-cut-1")?.status === "rolled-back", "an update cut before the switch is returned at boot");
  assertSettledOn(settledVersion);
  check(psql("select count(*) from e2e_rows") === "1", "the data is still there");

  // A power cut also leaves the layers of containers created just before it
  // half written: their link and lower files are there and empty, podman ps
  // still lists them and every inspect of them fails. Containers hold no
  // data, so the boot removes them and starts their services again.
  const damaged = ["web", "backup"].map((service) => {
    const c = byService().get(service);
    if (!c) fail(`no ${service} container`);
    return c;
  });
  podman("stop", ...damaged.map((c) => c.name));
  for (const c of damaged) {
    const upper = podman("container", "inspect", "--format", "{{.GraphDriver.Data.UpperDir}}", c.name);
    for (const file of ["link", "lower"]) truncateSync(join(dirname(upper), file), 0);
    check(sh(["podman", "container", "inspect", c.name], { ok: false }).code !== 0, `${c.service}'s container can no longer be inspected`);
  }
  check(powerCutAndBoot().code === 0, "the boot after a power cut that left container layers half written succeeds");
  assertSettledOn(settledVersion);
  for (const c of damaged) check(byService().get(c.service)?.created !== c.created, `${c.service} runs in a container created again`);

  // --- the rollback fails too: stay, and never restore again ---------------
  const v11 = buildRelease({ version: "v1.10.0", api: "never-healthy", rustfs: newRustfs }, known);
  writeRequest("e2e-rollback-fails", v11.manifest);
  await waitFor("the switch", 1200, () => journalPhase() === "switching");
  // The old web image disappears and cannot be pulled again.
  podman("stop", "laysi-e2e-registry");
  const oldWeb = releases.get(settledVersion)!.digests.web;
  const oldWebId = JSON.parse(podman("images", "--format", "json")).find((i: any) => (i.RepoDigests ?? []).some((d: string) => d.endsWith(oldWeb)))?.Id;
  if (oldWebId) sh(["podman", "rmi", "-f", oldWebId], { ok: false });
  const failed = await waitFor("the rollback to fail", 1800, () => {
    const x = readResult("e2e-rollback-fails");
    return x && FINAL.has(x.status) ? x : undefined;
  });
  await settledRun();
  check(failed.status === "rollback-failed" && !!failed.reason && !!failed["rollback-reason"], "a rollback that fails is final and records both reasons");
  check(installed() === settledVersion, "the host stays on the previous release");
  check(journalPhase() === "rollback-failed", "the journal is left in its final state");
  check(!switching(), "the switching marker is gone once the failure is final");
  psql("insert into e2e_rows values ('written after the failed rollback')");
  powerCutAndBoot();
  await settledRun();
  check(psql("select count(*) from e2e_rows") === "2", "the next boot did not restore the database again");
  check(!switching(), "and left no switching marker");
  podman("start", "laysi-e2e-registry");

  // A good release brings the machine back.
  const v12 = buildRelease({ version: "v1.11.0", rustfs: newRustfs }, known);
  known.push(v12.migration);
  r = await update("e2e-after-failure", v12.manifest);
  check(r.status === "succeeded", "a later good release recovers the machine");
  assertSettledOn("v1.11.0");

  // --- an image that did not change, under a new index digest ---------------
  // The host already holds v1.11.0's backup image: podman files the new
  // digest as one more name of it and goes on reporting the one it was first
  // pulled by.
  const sameBackup = await rewrap(`${REGISTRY}/laysi-one-backup`, v12.digests.backup, "v1.11.1");
  const unchanged = buildRelease({ version: "v1.11.1", rustfs: newRustfs, backup: sameBackup }, known);
  known.push(unchanged.migration);
  r = await update("e2e-unchanged-backup", unchanged.manifest);
  check(r.status === "succeeded", "a release whose backup image is the previous one under a new index digest is installed");
  check(byService().get("backup")?.digest === v12.digests.backup, "podman reports the backup image by the digest it was first pulled by");
  assertSettledOn("v1.11.1");

  // Once no release names it, that image goes with the clean-up. podman
  // refuses to remove an image with two names by its ID.
  const vRenamedNext = buildRelease({ version: "v1.11.2", rustfs: newRustfs }, known);
  known.push(vRenamedNext.migration);
  r = await update("e2e-after-unchanged", vRenamedNext.manifest);
  check(r.status === "succeeded", "the release after it is installed");
  assertSettledOn("v1.11.2");
  check(hasImage(sameBackup), "the previous release's backup image is kept");
  const vRenamedGone = buildRelease({ version: "v1.11.3", api: "exit", rustfs: newRustfs }, known);
  r = await update("e2e-renamed-removed", vRenamedGone.manifest);
  check(r.status === "rolled-back" && r.running === "v1.11.2", "the release after that is rolled back");
  assertSettledOn("v1.11.2");
  check(!hasImage(v12.digests.backup) && !hasImage(sameBackup), "the backup image pulled under two digests is removed once no release names it");

  // --- many runs in a row ---------------------------------------------------
  for (let n = 0; n < 10; n++) {
    r = await update(`e2e-repeat-${String(n).padStart(4, "0")}`, vRenamedNext.manifest);
    if (r.status !== "refused-same-version") fail(`repeat ${n}: ${r.status}`);
  }
  // Six requests faster than the service runs: the default start limit would
  // have stopped the path unit here.
  for (let n = 0; n < 6; n++) {
    writeRequest(`e2e-burst-${String(n).padStart(4, "0")}`, vRenamedNext.manifest);
    await sleep(150);
  }
  await settledRun();
  await sleep(2000);
  check(unitState("laysi-updater.path") === "active", "after every run the path unit is still watching (ActiveState=active)");
  check(requestsLeft().length === 0, "and the requests directory is empty");

  // --- rustfs moves again, and the next update is rolled back ---------------
  // The rollback puts v1.12.0's files over `.previous`, so no later update
  // reads v1.11.0's again: the rustfs of the repository only v1.11.0 used
  // has to go with the rollback. Squashed for the reason given above.
  const nextDir = mkdtempSync(join(workDir, "rustfs-next-"));
  writeFileSync(join(nextDir, "Containerfile"), `FROM ${newRustfs}\nLABEL e2e.rustfs=3\n`);
  podman("build", "-q", "--pull=never", "--squash-all", "-t", `localhost:${REGISTRY_PORT}/rustfs-next/rustfs:e2e`, nextDir);
  const nextFile = join(nextDir, "digest");
  podman("push", "-q", "--tls-verify=false", "--digestfile", nextFile, `localhost:${REGISTRY_PORT}/rustfs-next/rustfs:e2e`);
  podman("rmi", `localhost:${REGISTRY_PORT}/rustfs-next/rustfs:e2e`);
  const nextRustfs = `localhost:${REGISTRY_PORT}/rustfs-next/rustfs@${readFileSync(nextFile, "utf8").trim()}`;
  const nextRustfsDigest = nextRustfs.split("@")[1];
  const v13 = buildRelease({ version: "v1.12.0", rustfs: nextRustfs }, known);
  known.push(v13.migration);
  r = await update("e2e-rustfs-again", v13.manifest);
  check(r.status === "succeeded", "a release that moves rustfs to a third repository is installed");
  downtime(r, "v1.12.0");
  assertSettledOn("v1.12.0");
  check(hasImage(movedRustfs) && hasImage(nextRustfsDigest), "the previous release's rustfs is kept beside the new one");
  const v14 = buildRelease({ version: "v1.13.0", api: "never-healthy", rustfs: nextRustfs }, known);
  r = await update("e2e-rustfs-rollback", v14.manifest);
  check(r.status === "rolled-back" && r.running === "v1.12.0", "the release after it never passes and is rolled back");
  assertSettledOn("v1.12.0");
  check(!hasImage(movedRustfs) && hasImage(nextRustfsDigest), "the rollback removes the rustfs only the release two back used, the current one kept");

  console.log("\nDowntime, step 4 to the end of step 8:");
  for (const d of downtimes) console.log(`  ${d.version}: ${d.seconds} s`);
  console.log("\nall checks passed");
  if (!values.keep) rmSync(workDir, { recursive: true, force: true });
}

await main();
