#!/usr/bin/env bun
// Release check for the third-party images docker-compose.prod.yml pins:
// postgres and rustfs, when this change moves either to another digest.
//
//   bun scripts/third-party-image-check.ts [--base origin/main]
//
// A rollback by the host-side updater restores the database from a dump
// into the previous postgres image, and leaves RustFS's objects as they are:
// they are not part of the rollback at all. So both images have to read the
// same data in either direction, which is what this runs:
//
// - rustfs: the old image writes objects into a fresh volume, the new image
//   reads them byte for byte and writes more, the old image reads all of them.
// - postgres: the same with rows in a database initialised the way the
//   compose file does (ICU zh-TW), and the ICU library of both images
//   compared. A different ICU version means every text index under the zh-TW
//   collation may sort differently: the change then has to bring a migration
//   that runs REINDEX and ALTER DATABASE ... REFRESH COLLATION VERSION, or
//   it does not pass.
//
// Images are compared by the digest they resolve to, so pinning a tag that
// already resolved to the same digest checks nothing. Uses docker, or the
// CLI in CONTAINER_CLI.

import { createHash, createHmac, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { parseArgs } from "node:util";

const REPO = resolve(import.meta.dir, "..");
const CLI = process.env.CONTAINER_CLI ?? "docker";
const COMPOSE = "docker-compose.prod.yml";

// Thrown rather than exiting, so the `finally` blocks still remove the
// containers and volumes a failed check created.
class CheckFailed extends Error {}

function fail(message: string): never {
  throw new CheckFailed(message);
}

function run(cmd: string[], ok = true): { code: number; out: string; err: string } {
  const p = Bun.spawnSync(cmd, { stdout: "pipe", stderr: "pipe" });
  const r = { code: p.exitCode ?? -1, out: p.stdout.toString().trim(), err: p.stderr.toString().trim() };
  if (ok && r.code !== 0) fail(`${cmd.join(" ")}: ${r.err || r.out}`);
  return r;
}

const cli = (...args: string[]) => run([CLI, ...args]).out;

function imageOf(compose: string, service: string): string {
  const block = compose.split(/\n  (?=[a-z]+:\n)/).find((b) => b.startsWith(`${service}:\n`));
  const image = block?.match(/\n    image: (\S+)/)?.[1];
  if (!image) fail(`${COMPOSE} has no image for ${service}`);
  return image;
}

/// The digest a reference resolves to now: the index digest it was pulled by.
function resolveDigest(reference: string): string {
  const pinned = reference.match(/@(sha256:[0-9a-f]{64})$/)?.[1];
  if (pinned) return pinned;
  cli("pull", "-q", reference);
  const digests: string[] = JSON.parse(cli("image", "inspect", "--format", "{{json .RepoDigests}}", reference));
  const digest = digests[0]?.split("@")[1];
  if (!digest) fail(`${reference} has no digest`);
  return digest;
}

async function waitUntil(what: string, probe: () => boolean | Promise<boolean>, seconds = 120) {
  const deadline = Date.now() + seconds * 1000;
  while (!(await probe())) {
    if (Date.now() > deadline) fail(`timed out waiting for ${what}`);
    await Bun.sleep(1000);
  }
}

const tag = randomBytes(4).toString("hex");

// --- rustfs ---------------------------------------------------------------

const RUSTFS_KEY = "laysi-check";
const RUSTFS_SECRET = randomBytes(16).toString("hex");

async function withRustfs<T>(image: string, volume: string, f: (s3: Bun.S3Client, endpoint: string) => Promise<T>): Promise<T> {
  const name = `laysi-check-rustfs-${tag}`;
  try {
    cli("run", "-d", "--name", name, "-p", "127.0.0.1::9000", "-v", `${volume}:/data`, "-e", "RUSTFS_VOLUMES=/data", "-e", `RUSTFS_ACCESS_KEY=${RUSTFS_KEY}`, "-e", `RUSTFS_SECRET_KEY=${RUSTFS_SECRET}`, image);
    const port = cli("port", name, "9000").split("\n")[0].split(":").at(-1);
    const s3 = new Bun.S3Client({ accessKeyId: RUSTFS_KEY, secretAccessKey: RUSTFS_SECRET, endpoint: `http://127.0.0.1:${port}`, bucket: "check", region: "us-east-1" });
    // Answering HTTP is not serving: RustFS refuses requests until its
    // storage has formed a quorum. A signed request that gets an S3 answer,
    // "no such bucket" included, means it is serving.
    await waitUntil(`${image} to serve`, async () => {
      try {
        await s3.exists("probe");
        return true;
      } catch (e: any) {
        return e?.code === "NoSuchBucket";
      }
    });
    return await f(s3, `http://127.0.0.1:${port}`);
  } finally {
    run([CLI, "rm", "-f", name], false);
  }
}

/// Bun's S3 client has no bucket call, so CreateBucket is one SigV4-signed
/// `PUT /<bucket>` with an empty body.
async function createBucket(endpoint: string, bucket: string) {
  const url = new URL(`/${bucket}`, endpoint);
  const amzDate = new Date().toISOString().replace(/[-:]|\.\d{3}/g, "");
  const day = amzDate.slice(0, 8);
  const scope = `${day}/us-east-1/s3/aws4_request`;
  const empty = createHash("sha256").update("").digest("hex");
  const headers = { host: url.host, "x-amz-content-sha256": empty, "x-amz-date": amzDate };
  const signed = Object.keys(headers).join(";");
  const canonical = ["PUT", url.pathname, "", ...Object.entries(headers).map(([k, v]) => `${k}:${v}`), "", signed, empty].join("\n");
  const toSign = ["AWS4-HMAC-SHA256", amzDate, scope, createHash("sha256").update(canonical).digest("hex")].join("\n");
  let key: Buffer = Buffer.from(`AWS4${RUSTFS_SECRET}`);
  for (const part of [day, "us-east-1", "s3", "aws4_request"]) key = createHmac("sha256", key).update(part).digest();
  const signature = createHmac("sha256", key).update(toSign).digest("hex");
  const res = await fetch(url, {
    method: "PUT",
    headers: {
      "x-amz-content-sha256": empty,
      "x-amz-date": amzDate,
      authorization: `AWS4-HMAC-SHA256 Credential=${RUSTFS_KEY}/${scope}, SignedHeaders=${signed}, Signature=${signature}`,
    },
  });
  if (!res.ok && res.status !== 409) fail(`creating the bucket: ${res.status} ${await res.text()}`);
}

async function checkRustfs(before: string, after: string) {
  console.log(`rustfs: ${before} -> ${after}`);
  const volume = `laysi-check-rustfs-${tag}`;
  cli("volume", "create", volume);
  const objects = new Map<string, Uint8Array>();
  const put = async (s3: Bun.S3Client, key: string, size: number) => {
    const bytes = new Uint8Array(randomBytes(size));
    await s3.write(key, bytes);
    objects.set(key, bytes);
  };
  const readAll = async (s3: Bun.S3Client, who: string) => {
    for (const [key, bytes] of objects) {
      const got = new Uint8Array(await s3.file(key).arrayBuffer());
      if (Buffer.compare(Buffer.from(got), Buffer.from(bytes)) !== 0) fail(`${who} reads ${key} differently`);
    }
    console.log(`  ok  ${who} reads all ${objects.size} objects byte for byte`);
  };
  try {
    await withRustfs(before, volume, async (s3, endpoint) => {
      await createBucket(endpoint, "check");
      for (let i = 0; i < 8; i++) await put(s3, `old/${i}`, 1024 * (i * 97 + 1));
      await put(s3, "old/large", 12 * 1024 * 1024);
    });
    await withRustfs(after, volume, async (s3) => {
      await readAll(s3, "the new image");
      for (let i = 0; i < 4; i++) await put(s3, `new/${i}`, 4096 * (i + 1));
    });
    await withRustfs(before, volume, (s3) => readAll(s3, "the old image, once the new one has written,"));
  } finally {
    run([CLI, "volume", "rm", "-f", volume], false);
  }
}

// --- postgres -------------------------------------------------------------

function icuVersion(image: string): string {
  return cli("run", "--rm", "--entrypoint", "sh", image, "-c", "dpkg-query -W -f='${Version}' 'libicu[0-9]*'");
}

async function withPostgres<T>(image: string, volume: string, f: (sql: (q: string) => string) => T): Promise<T> {
  const name = `laysi-check-pg-${tag}`;
  try {
    cli("run", "-d", "--name", name, "-v", `${volume}:/var/lib/postgresql`, "-e", "POSTGRES_USER=laysi", "-e", "POSTGRES_PASSWORD=check", "-e", "POSTGRES_DB=laysi", "-e", "POSTGRES_INITDB_ARGS=--encoding=UTF8 --locale-provider=icu --icu-locale=zh-TW --locale=C.UTF-8", image);
    const sql = (q: string) => cli("exec", name, "psql", "-v", "ON_ERROR_STOP=1", "-U", "laysi", "-d", "laysi", "-Atc", q);
    await waitUntil(`${image} to accept connections`, () => run([CLI, "exec", name, "psql", "-U", "laysi", "-d", "laysi", "-Atc", "select 1"], false).code === 0);
    return f(sql);
  } finally {
    run([CLI, "rm", "-f", name], false);
  }
}

function changedMigrations(base: string): string {
  const files = run(["git", "-C", REPO, "diff", "--name-only", "--diff-filter=A", `${base}...HEAD`, "--", "backend/migrations"]).out.split("\n").filter(Boolean);
  return files.map((f) => readFileSync(resolve(REPO, f), "utf8")).join("\n");
}

async function checkPostgres(before: string, after: string, base: string) {
  console.log(`postgres: ${before} -> ${after}`);
  const [oldIcu, newIcu] = [icuVersion(before), icuVersion(after)];
  console.log(`  ICU ${oldIcu} -> ${newIcu}`);
  if (oldIcu.split(".")[0] !== newIcu.split(".")[0]) {
    const added = changedMigrations(base);
    const reindexes = /\bREINDEX\b/i.test(added) && /REFRESH\s+COLLATION\s+VERSION/i.test(added);
    if (!reindexes) {
      fail(
        `the new postgres image has ICU ${newIcu} where the old one has ${oldIcu}: text indexes under the zh-TW collation have to be rebuilt. ` +
          "Add a migration that runs REINDEX and ALTER DATABASE ... REFRESH COLLATION VERSION, or keep the image on the old Debian base.",
      );
    }
    console.log("  ok  the change brings a migration that reindexes and refreshes the collation version");
  } else {
    console.log("  ok  same ICU major version");
  }
  const volume = `laysi-check-pg-${tag}`;
  cli("volume", "create", volume);
  const words = ["臺北", "台中", "高雄", "Ａ公司", "a公司", "乙", "甲", "丙", "zeta", "Alpha"];
  try {
    const sorted = await withPostgres(before, volume, (sql) => {
      sql("create table check_rows (name text primary key)");
      for (const w of words) sql(`insert into check_rows values ('${w}')`);
      return sql("select string_agg(name, ',' order by name) from check_rows");
    });
    await withPostgres(after, volume, (sql) => {
      const now = sql("select string_agg(name, ',' order by name) from check_rows");
      if (now !== sorted) fail(`the new image sorts the rows differently:\n  ${sorted}\n  ${now}`);
      sql("insert into check_rows values ('新的一列')");
      if (sql("select count(*) from check_rows where name > ''") !== String(words.length + 1)) fail("the new image's index scan misses rows");
      console.log("  ok  the new image reads and sorts the old rows, and writes");
    });
    await withPostgres(before, volume, (sql) => {
      if (sql("select count(*) from check_rows") !== String(words.length + 1)) fail("the old image does not read what the new one wrote");
      console.log("  ok  the old image reads what the new one wrote");
    });
  } finally {
    run([CLI, "volume", "rm", "-f", volume], false);
  }
}

// ---------------------------------------------------------------------------

async function main() {
  const { values } = parseArgs({ args: Bun.argv.slice(2), options: { base: { type: "string", default: "origin/main" } } });
  const base = values.base!;
  const current = readFileSync(resolve(REPO, COMPOSE), "utf8");
  const previous = run(["git", "-C", REPO, "show", `${base}:${COMPOSE}`]).out + "\n";

  let checked = 0;
  for (const service of ["rustfs", "postgres"] as const) {
    const [before, after] = [imageOf(previous, service), imageOf(current, service)];
    if (before === after) continue;
    if (!after.includes("@sha256:")) fail(`${COMPOSE} names ${service} by tag: ${after}`);
    const [from, to] = [resolveDigest(before), resolveDigest(after)];
    if (from === to) {
      console.log(`${service}: ${before} and ${after} are the same image`);
      continue;
    }
    checked++;
    if (service === "rustfs") await checkRustfs(before, after);
    else await checkPostgres(before, after, base);
  }
  console.log(checked ? "the changed images read the same data both ways" : "no third-party image changed");
}

try {
  await main();
} catch (e) {
  console.error(e instanceof CheckFailed ? `FAIL: ${e.message}` : e);
  process.exitCode = 1;
}
