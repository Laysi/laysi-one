#!/usr/bin/env bun
// The release page, checked without cutting a tag.
//
//   bun scripts/release-notes-test.ts
//
// CHANGES_FILE is how the notes Laysi/release-notes-action writes get into the
// page. Unset, or pointing at nothing, the page still has to come out whole
// with the commit list in that section -- that is what running this by hand
// does, and the compose file attached to the release is the distribution.

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const script = join(import.meta.dir, "..", ".github", "release-notes.sh");
const previousTag = join(import.meta.dir, "..", ".github", "previous-tag.sh");
const repo = mkdtempSync(join(tmpdir(), "release-notes-"));

// Without the developer's own git config: tag.gpgSign there turns every
// `git tag` below into one that wants a message, and versionsort.suffix
// there changes the order the scripts read.
const isolated = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1" };

function git(...args: string[]): void {
  const r = Bun.spawnSync(["git", ...args], {
    cwd: repo,
    env: {
      ...process.env,
      ...isolated,
      GIT_AUTHOR_NAME: "t",
      GIT_AUTHOR_EMAIL: "t@t",
      GIT_COMMITTER_NAME: "t",
      GIT_COMMITTER_EMAIL: "t@t",
    },
  });
  if (r.exitCode !== 0) throw new Error(`git ${args[0]}: ${r.stderr.toString()}`);
}

// Each commit touches a file: the log is filtered by path, so empty commits
// would be dropped and every assertion below would pass on nothing.
function commit(subject: string, file = "f.txt"): void {
  mkdirSync(join(repo, file, ".."), { recursive: true });
  writeFileSync(join(repo, file), subject);
  git("add", file);
  git("commit", "-q", "-m", subject);
}

git("init", "-q", "-b", "main");
commit("feat(payroll): 補算同一期薪資");
git("tag", "v0.1.0");
commit("ci: 換一版 runner");
commit("fix(leave): 休假時數不含週末");
git("tag", "v0.2.0");

function run(args: string[], env: Record<string, string> = {}): { out: string; code: number } {
  const r = Bun.spawnSync(["sh", script, ...args], {
    cwd: repo,
    env: { ...process.env, ...isolated, ...env },
  });
  return { out: r.stdout.toString(), code: r.exitCode };
}

function previousOf(tag: string): string {
  const r = Bun.spawnSync(["sh", previousTag, tag], { cwd: repo, env: { ...process.env, ...isolated } });
  if (r.exitCode !== 0) throw new Error(`previous-tag ${tag}: ${r.stderr.toString()}`);
  return r.stdout.toString().trim();
}

let failed = 0;

function ok(label: string, condition: boolean): void {
  if (!condition) failed += 1;
  console.log(`${condition ? "  ok" : "FAIL"}  ${label}`);
}

const plain = run(["v0.2.0"]);
ok("沒有 CHANGES_FILE 時仍然產生說明", plain.code === 0);
ok("變更段落是 commit 清單", plain.out.includes("休假時數不含週末"));
ok("不列上一版的 commit", !plain.out.includes("補算同一期薪資"));
ok("不列 ci 與 chore", !plain.out.includes("換一版 runner"));
ok("帶著安裝說明", plain.out.includes("docker-compose.prod.yml"));
ok("帶著比較連結", plain.out.includes("compare/v0.1.0...v0.2.0"));
ok("帶著授權段落", plain.out.includes("LICENSE.txt"));
ok("正式版帶著示範站", plain.out.includes("demo.laysi.one"));
ok("正式版不標成候選版", !plain.out.includes("發布候選版"));
ok("第一個標籤沒有上一版", previousOf("v0.1.0") === "");

const changes = join(repo, "ai.md");
writeFileSync(changes, "### 修正\n- **休假**：時數不含週末");
const withAi = run(["v0.2.0"], { CHANGES_FILE: changes });
ok("CHANGES_FILE 取代 commit 清單", withAi.out.includes("**休假**：時數不含週末"));
ok("取代後不再列 commit", !withAi.out.includes("- fix(leave)"));
ok("取代後安裝說明仍然在", withAi.out.includes("docker-compose.prod.yml"));

// The action always hands back a path; an empty file is what it writes when
// the model is unset, so this is the ordinary case and not an error.
writeFileSync(changes, "");
const empty = run(["v0.2.0"], { CHANGES_FILE: changes });
ok("CHANGES_FILE 是空的就回退成 commit 清單", empty.out.includes("休假時數不含週末"));
const missing = run(["v0.2.0"], { CHANGES_FILE: join(repo, "nope.md") });
ok("CHANGES_FILE 不存在也不失敗", missing.code === 0);
ok("CHANGES_FILE 不存在時回退成 commit 清單", missing.out.includes("休假時數不含週末"));

// license-server ships on its own tags and no customer reading this page runs
// it, so its commits stay out of the list.
commit("fix(license): 換一版簽章", "license-server/x.rs");
git("tag", "v0.3.0");
ok("不列 license-server 的 commit", !run(["v0.3.0"]).out.includes("換一版簽章"));

// Release candidates. rc.10 comes straight after rc.2 so that a string sort,
// which puts it first, cannot pass.
commit("feat(ar): 應收單拆營業稅");
git("tag", "v0.4.0-rc.1");
commit("fix(ar): 稅額逐張取整");
git("tag", "v0.4.0-rc.2");
commit("fix(ar): 折讓單也拆稅");
git("tag", "v0.4.0-rc.10");
git("tag", "v0.4.0");
commit("fix(payroll): 補發不重算勞保");
git("tag", "v0.4.1");
commit("feat(hr): 眷屬加退保");
git("tag", "v0.5.0-rc.1");

ok("第一個候選版接在上一個正式版之後", previousOf("v0.4.0-rc.1") === "v0.3.0");
ok("候選版接在上一個候選版之後", previousOf("v0.4.0-rc.2") === "v0.4.0-rc.1");
ok("rc.10 接在 rc.2 之後", previousOf("v0.4.0-rc.10") === "v0.4.0-rc.2");
ok("正式版跳過自己的候選版", previousOf("v0.4.0") === "v0.3.0");
ok("下一個正式版接在正式版之後", previousOf("v0.4.1") === "v0.4.0");
ok("正式版之後的候選版接在正式版之後", previousOf("v0.5.0-rc.1") === "v0.4.1");

const rc2 = run(["v0.4.0-rc.2"]);
ok("候選版標成候選版", rc2.out.includes("本版是發布候選版"));
ok("候選版不指向示範站", !rc2.out.includes("demo.laysi.one"));
ok("候選版只列上一個候選版之後的變更", rc2.out.includes("稅額逐張取整") && !rc2.out.includes("應收單拆營業稅"));
ok("候選版的比較連結從上一個候選版起算", rc2.out.includes("compare/v0.4.0-rc.1...v0.4.0-rc.2"));

const final = run(["v0.4.0"]);
ok("正式版列出每一輪候選版的變更", final.out.includes("應收單拆營業稅") && final.out.includes("折讓單也拆稅"));
ok("正式版的比較連結從上一個正式版起算", final.out.includes("compare/v0.3.0...v0.4.0"));
ok("下一個正式版不重列上一版的變更", !run(["v0.4.1"]).out.includes("折讓單也拆稅"));

rmSync(repo, { recursive: true, force: true });
console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
