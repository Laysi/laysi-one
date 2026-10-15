#!/usr/bin/env bun
// The ICU decision of third-party-image-check.ts, checked without docker.
//
//   bun scripts/third-party-image-check-test.ts
//
// The check itself runs only on a pull request that moves the postgres
// digest, so its failing path would otherwise never run in CI.

import { join } from "node:path";

import { icuVerdict } from "./third-party-image-check";

let failed = 0;

function ok(label: string, condition: boolean, detail = ""): void {
  if (!condition) failed += 1;
  console.log(`${condition ? "  ok" : "FAIL"}  ${label}${!condition && detail ? `\n      ${detail}` : ""}`);
}

const OLD = "72.1-3+deb12u1";
const NEW = "76.1-4";
const REINDEX = "REINDEX DATABASE laysi;\n";
const REFRESH = "ALTER DATABASE laysi REFRESH COLLATION VERSION;\n";

let reads = 0;
const migrations = (...texts: string[]) => () => {
  reads += 1;
  return texts;
};

const same = icuVerdict(OLD, "72.1-3+deb12u2", migrations());
ok("ICU 主版本相同時通過", same.ok && same.message === "same ICU major version", same.message);
ok("ICU 主版本相同時不讀 migration", reads === 0);
ok("次版本不同、主版本相同時通過", icuVerdict("76.1-4", "76.2-1", migrations()).ok);

const none = icuVerdict(OLD, NEW, migrations());
ok("ICU 主版本不同且沒有 migration 時不通過", !none.ok, none.message);
ok("不通過的訊息寫出兩個 ICU 版本", none.message.includes(`ICU ${NEW}`) && none.message.includes(OLD), none.message);
ok("不通過的訊息寫出要加的敘述", none.message.includes("REINDEX") && none.message.includes("REFRESH COLLATION VERSION"), none.message);
ok("ICU 主版本不同時讀 migration", reads === 1);

ok("只有 REINDEX 時不通過", !icuVerdict(OLD, NEW, migrations(REINDEX)).ok);
ok("只有 REFRESH COLLATION VERSION 時不通過", !icuVerdict(OLD, NEW, migrations(REFRESH)).ok);
ok("無關的 migration 不算", !icuVerdict(OLD, NEW, migrations("CREATE TABLE t (id int);\n")).ok);
ok("REINDEXES 這類較長的字不算 REINDEX", !icuVerdict(OLD, NEW, migrations(`-- REINDEXES\n${REFRESH}`)).ok);

const both = icuVerdict(OLD, NEW, migrations(REINDEX + REFRESH));
ok("同一份 migration 兩者都有時通過", both.ok, both.message);
ok(
  "通過的訊息說明帶了重建索引",
  both.message === "the change brings a migration that reindexes and refreshes the collation version",
  both.message,
);
ok("兩者分在兩份新增的 migration 時通過", icuVerdict(OLD, NEW, migrations(REINDEX, REFRESH)).ok);
ok("不分大小寫", icuVerdict(OLD, NEW, migrations("reindex database laysi;\nalter database laysi refresh collation version;\n")).ok);
ok("REFRESH COLLATION VERSION 中間換行或多個空白仍算", icuVerdict(OLD, NEW, migrations(`${REINDEX}ALTER DATABASE laysi REFRESH\n  COLLATION   VERSION;\n`)).ok);
ok("降回較舊的 ICU 同樣要重建索引", !icuVerdict(NEW, OLD, migrations()).ok);

// Importing the module for the cases above must not run the check itself.
const imported = Bun.spawnSync(["bun", "-e", `await import(${JSON.stringify(join(import.meta.dir, "third-party-image-check.ts"))})`], {
  stdout: "pipe",
  stderr: "pipe",
});
ok(
  "載入模組時不執行檢查",
  imported.exitCode === 0 && imported.stdout.toString() === "" && imported.stderr.toString() === "",
  `${imported.exitCode} ${imported.stdout} ${imported.stderr}`,
);

if (failed > 0) {
  console.error(`${failed} 項失敗`);
  process.exit(1);
}
