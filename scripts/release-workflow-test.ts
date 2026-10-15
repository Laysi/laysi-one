#!/usr/bin/env bun
// What release.yml does with a release candidate, checked without cutting a
// tag.
//
//   bun scripts/release-workflow-test.ts
//
// A tag with a `-` in it (v0.2.0-rc.1) is built and signed like any other but
// must not move `latest` on ghcr.io, must be published as a prerelease, and
// must not deploy the demo. The first and last are expressions only the runner
// evaluates, so they are compared as written. The publish step is shell, so
// it runs here against a stand-in `gh` that records what it was asked.

import { YAML } from "bun";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const workflow = YAML.parse(readFileSync(join(root, ".github", "workflows", "release.yml"), "utf8")) as {
  jobs: Record<string, Job>;
};

type Step = { id?: string; name?: string; uses?: string; run?: string; with?: Record<string, string> };
type Job = { if?: string; uses?: string; with?: Record<string, string>; steps?: Step[] };

const FINAL_ONLY = "${{ !contains(github.ref_name, '-') }}";

let failed = 0;

function ok(label: string, condition: boolean): void {
  if (!condition) failed += 1;
  console.log(`${condition ? "  ok" : "FAIL"}  ${label}`);
}

function step(job: string, match: (s: Step) => boolean): Step {
  const found = workflow.jobs[job]?.steps?.find(match);
  if (!found) throw new Error(`release.yml: no such step in ${job}`);
  return found;
}

const meta = step("images", (s) => s.uses?.startsWith("docker/metadata-action@") ?? false);
const latest = (meta.with?.tags ?? "").split("\n").filter((line) => line.includes("latest"));
ok("映像不自動加 latest", meta.with?.flavor === "latest=false");
ok("latest 只有一行", latest.length === 1);
ok("latest 只給正式版", latest[0]?.trim() === `type=raw,value=latest,enable=${FINAL_ONLY}`);

const demos = Object.entries(workflow.jobs).filter(([, job]) => job.with?.target === "demo");
ok("有部署示範站的 job", demos.length > 0);
for (const [name, job] of demos) ok(`${name} 只部署正式版`, job.if === FINAL_ONLY);

const previous = step("release", (s) => s.id === "previous");
const changes = step("release", (s) => s.id === "changes");
ok("上一版由 previous-tag.sh 決定", previous.run?.includes("sh .github/previous-tag.sh") ?? false);
ok("變更說明沿用同一個上一版", changes.with?.["previous-tag"] === "${{ steps.previous.outputs.tag }}");

const publish = step("release", (s) => s.name === "Publish the release").run ?? "";
ok("發布步驟只讀環境變數", publish !== "" && !publish.includes("${{"));

const dir = mkdtempSync(join(tmpdir(), "release-workflow-"));
mkdirSync(join(dir, ".github"));
copyFileSync(join(root, ".github", "published-manifest.sh"), join(dir, ".github", "published-manifest.sh"));
mkdirSync(join(dir, "bin"));
const log = join(dir, "gh.log");
writeFileSync(
  join(dir, "bin", "gh"),
  `#!/bin/sh
printf '%s\\n' "$*" >> "$GH_LOG"
case "$1 $2" in
"release create") exit "$CREATE_EXIT" ;;
esac
exit 0
`,
);
chmodSync(join(dir, "bin", "gh"), 0o755);

// GitHub runs a step's script with bash -eo pipefail.
function publishAs(tag: string, createExit: number): string[] {
  rmSync(log, { force: true });
  const r = Bun.spawnSync(["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", publish], {
    cwd: dir,
    env: {
      ...process.env,
      PATH: `${join(dir, "bin")}:${process.env.PATH}`,
      GH_LOG: log,
      CREATE_EXIT: String(createExit),
      GH_TOKEN: "t",
      GITHUB_REF_NAME: tag,
      GITHUB_REPOSITORY: "Laysi/laysi-one",
    },
  });
  if (r.exitCode !== 0) throw new Error(`publish ${tag}: ${r.stderr.toString()}`);
  return readFileSync(log, "utf8").trim().split("\n");
}

function called(calls: string[], command: string): string {
  return calls.find((c) => c.startsWith(command)) ?? "";
}

const rcNew = publishAs("v0.2.0-rc.1", 0);
ok("候選版建立為 prerelease", called(rcNew, "release create v0.2.0-rc.1 ").includes("--prerelease=true"));
const finalNew = publishAs("v0.2.0", 0);
ok("正式版建立為正式 release", called(finalNew, "release create v0.2.0 ").includes("--prerelease=false"));

// The release already exists: cut in the web UI, or a re-run.
const rcEdit = publishAs("v0.2.0-rc.1", 1);
ok("既有的候選版改成 prerelease", called(rcEdit, "release edit v0.2.0-rc.1 ").includes("--prerelease=true"));
const finalEdit = publishAs("v0.2.0", 1);
ok("既有的正式版取消 prerelease", called(finalEdit, "release edit v0.2.0 ").includes("--prerelease=false"));

rmSync(dir, { recursive: true, force: true });
console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
