#!/usr/bin/env bun
// The release manifest signer, checked against the Rust verifier.
//
//   bun scripts/release-manifest-test.ts
//
// Signs with throwaway keys drawn here, through the same command release.yml
// runs, and verifies the result with the verifier the updater and the product
// use (the verify-release-manifest example of laysi-license-token). Needs
// cargo; the first run compiles the example.

import { createHash, createPrivateKey, createPublicKey, randomBytes } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const script = join(import.meta.dir, "release-manifest.ts");
const fixture = join(root, "backend", "crates", "laysi-license-token", "testdata", "release-manifest.txt");
const dir = mkdtempSync(join(tmpdir(), "release-manifest-"));

const API = `sha256:${"1".repeat(64)}`;
const WEB = `sha256:${"2".repeat(64)}`;
const BACKUP = `sha256:${"3".repeat(64)}`;
const PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

function publicHex(seedHex: string): string {
  const key = createPrivateKey({ key: Buffer.concat([PKCS8_PREFIX, Buffer.from(seedHex, "hex")]), format: "der", type: "pkcs8" });
  const spki = createPublicKey(key).export({ format: "der", type: "spki" }) as Buffer;
  return spki.subarray(spki.length - 32).toString("hex");
}

function keyId(seedHex: string): string {
  return createHash("sha256").update(Buffer.from(publicHex(seedHex), "hex")).digest("hex").slice(0, 16);
}

interface Run {
  out: string;
  err: string;
  code: number;
}

function sh(cmd: string[], env: Record<string, string> = {}, cwd = root): Run {
  const r = Bun.spawnSync(cmd, { cwd, env: { ...process.env, ...env } });
  return { out: r.stdout.toString(), err: r.stderr.toString(), code: r.exitCode ?? -1 };
}

function signer(args: string[], secret?: string): Run {
  const env: Record<string, string> = { RELEASE_MANIFEST_SIGNING_KEY: secret ?? "" };
  return sh(["bun", script, ...args], env);
}

function signTo(out: string, secret: string, version = "v0.1.13"): Run {
  return signer(["sign", "--version", version, "--api", API, "--web", WEB, "--backup", BACKUP, "--out", out], secret);
}

function verifier(file: string, ...args: string[]): Run {
  return sh(
    ["cargo", "run", "-q", "-p", "laysi-license-token", "--example", "verify-release-manifest", "--", file, ...args],
    {},
    join(root, "backend"),
  );
}

let failed = 0;

function ok(label: string, condition: boolean, detail = ""): void {
  if (!condition) failed += 1;
  console.log(`${condition ? "  ok" : "FAIL"}  ${label}${!condition && detail ? `\n      ${detail}` : ""}`);
}

const a = randomBytes(32).toString("hex");
const b = randomBytes(32).toString("hex");
const c = randomBytes(32).toString("hex");
const secrets = [a, b, c];

function leaks(r: Run): boolean {
  return secrets.some((s) => r.out.includes(s) || r.err.includes(s));
}

const one = join(dir, "one.txt");
const signed = signTo(one, a);
ok("以一把金鑰簽出清單", signed.code === 0, signed.err);
const text = readFileSync(one, "utf8");
ok("清單帶著該金鑰的識別碼", text.includes(`signature ${keyId(a)} `));

const good = verifier(one, "--key", publicHex(a), "--version", "v0.1.13");
ok("Rust 驗證端接受", good.code === 0, good.err);
ok("讀出版本與三份 digest", good.out.includes("version v0.1.13") && good.out.includes(`image backup ${BACKUP}`));

const wrongKey = verifier(one, "--key", publicHex(c));
ok("錯誤金鑰簽的清單被拒絕", wrongKey.code !== 0 && wrongKey.err.includes("信任"), wrongKey.err);

const compiledIn = verifier(one);
ok("測試金鑰不被編進來的金鑰接受", compiledIn.code !== 0, compiledIn.out);

const wrongVersion = verifier(one, "--key", publicHex(a), "--version", "v0.1.14");
ok("版本與標籤不符時失敗", wrongVersion.code !== 0);

const tampered = join(dir, "tampered.txt");
writeFileSync(tampered, text.replace(API, `sha256:${"4".repeat(64)}`));
const edited = verifier(tampered, "--key", publicHex(a));
ok("改過 digest 的清單被拒絕", edited.code !== 0 && edited.err.includes("不符"), edited.err);

const two = join(dir, "two.txt");
ok("兩把金鑰一起簽", signTo(two, `${a}\n${b}`).code === 0);
const twoText = readFileSync(two, "utf8");
ok("每把金鑰一行簽章", twoText.split("\n").filter((l) => l.startsWith("signature ")).length === 2);
const trustedPlusUnknown = verifier(two, "--key", publicHex(a));
ok("一個信任、一個認不得的簽章被接受", trustedPlusUnknown.code === 0, trustedPlusUnknown.err);
const onlyUnknown = verifier(two, "--key", publicHex(c));
ok("只有認不得的簽章被拒絕", onlyUnknown.code !== 0);

const derived = signer(["public-key"], `${a} ${b}`);
ok("public-key 印出每把金鑰的公鑰", derived.code === 0 && derived.out === `${publicHex(a)}\n${publicHex(b)}\n`, derived.out);

const refusals: [string, Run][] = [
  ["未設定金鑰時拒絕簽章", signTo(join(dir, "x1"), "")],
  ["金鑰不是 64 個十六進位字元時拒絕", signTo(join(dir, "x2"), a.slice(1))],
  ["重複的金鑰被拒絕", signTo(join(dir, "x3"), `${a} ${a}`)],
  ["版本號沒有 v 時拒絕", signTo(join(dir, "x4"), a, "0.1.13")],
  ["版本號帶建置資訊時拒絕", signTo(join(dir, "x5"), a, "v0.1.13+abc")],
  [
    "digest 格式不符時拒絕",
    signer(["sign", "--version", "v0.1.13", "--api", "sha256:XYZ", "--web", WEB, "--backup", BACKUP, "--out", join(dir, "x6")], a),
  ],
  ["缺少 digest 時拒絕", signer(["sign", "--version", "v0.1.13", "--api", API, "--web", WEB, "--out", join(dir, "x7")], a)],
];
for (const [label, r] of refusals) ok(label, r.code !== 0, r.out);

const everything = [signed, good, derived, ...refusals.map(([, r]) => r)];
ok("任何輸出都不含私鑰", !everything.some(leaks));

// Ed25519 is deterministic, so the committed fixture the Rust tests read is
// exactly what the script writes for the same key and inputs.
const regenerated = join(dir, "fixture.txt");
signTo(regenerated, "07".repeat(32));
ok("testdata/release-manifest.txt 與腳本輸出一致", readFileSync(regenerated, "utf8") === readFileSync(fixture, "utf8"));

// .github/published-manifest.sh decides whether a version already went out,
// so an error from GitHub has to fail it rather than read as "not yet". A
// stub `gh` on PATH plays each answer.
const stubDir = join(dir, "bin");
mkdirSync(stubDir);
writeFileSync(
  join(stubDir, "gh"),
  `#!/bin/sh
case "$1 $2" in
  "api --paginate")
    [ "$STUB" = api-error ] && { echo "gh: Server Error (HTTP 502)" >&2; exit 1; }
    echo v0.1.12
    [ "$STUB" = no-release ] || echo v0.1.13 ;;
  "release view")
    [ "$STUB" = view-error ] && exit 1
    echo docker-compose.prod.yml
    [ "$STUB" = no-asset ] || echo release-manifest.txt ;;
  "release download")
    [ "$STUB" = download-error ] && exit 1
    while [ $# -gt 0 ]; do [ "$1" = --dir ] && out="$2"; shift; done
    cp "${fixture}" "$out/" ;;
  *) exit 1 ;;
esac
`,
  { mode: 0o755 },
);
const published = (stub: string): { code: number; found: string | null } => {
  const out = join(dir, `published-${stub}.txt`);
  const r = sh(["sh", join(root, ".github", "published-manifest.sh"), "v0.1.13", out], {
    STUB: stub,
    GITHUB_REPOSITORY: "Laysi/laysi-one",
    PATH: `${stubDir}:${process.env.PATH}`,
  });
  return { code: r.code, found: existsSync(out) ? readFileSync(out, "utf8") : null };
};
const none = published("no-release");
ok("沒有 release 時回報沒有清單", none.code === 0 && none.found === null);
const noAsset = published("no-asset");
ok("release 沒有清單時回報沒有清單", noAsset.code === 0 && noAsset.found === null);
const found = published("found");
ok("已附清單時取回原檔", found.code === 0 && found.found === readFileSync(fixture, "utf8"));
for (const stub of ["api-error", "view-error", "download-error"]) {
  ok(`GitHub 回錯誤時失敗（${stub}）`, published(stub).code !== 0);
}

// check-trusted is the signing-key step of release.yml, which runs on a tag
// only. Each case gives it a release.rs of its own, in the shapes rustfmt
// gives the constant: two lines for one key, one key per line for several.
const rs = (name: string, text: string): string => {
  const path = join(dir, `${name}.rs`);
  writeFileSync(path, text);
  return path;
};
const declare = (...keys: string[]): string =>
  keys.length === 1
    ? `pub const TRUSTED_KEYS: &[&str] =\n    &["${keys[0]}"];\n`
    : `pub const TRUSTED_KEYS: &[&str] = &[\n${keys.map((k) => `    "${k}",\n`).join("")}];\n`;
const trustsA = rs("trusts-a", `/// Compiled in.\n${declare(publicHex(a))}\npub const MAX_MANIFEST_BYTES: usize = 4096;\n`);
const trustsAB = rs("trusts-ab", declare(publicHex(a), publicHex(b)));
const oneLine = rs("one-line", `pub const TRUSTED_KEYS: &[&str] = &["${publicHex(a)}"];\n`);
const elsewhere = rs(
  "elsewhere",
  `/// Not this one: "${publicHex(c)}".\n${declare(publicHex(a))}\nconst OTHER: &str = "${publicHex(c)}";\n`,
);
const renamed = rs("renamed", declare(publicHex(a)).replace("TRUSTED_KEYS", "TRUSTED"));
const check = (source: string | null, secret: string): Run =>
  signer(["check-trusted", ...(source ? ["--source", source] : [])], secret);

const trustedRuns: Run[] = [];
const expect = (label: string, r: Run, pass: boolean, says = ""): void => {
  trustedRuns.push(r);
  ok(label, pass ? r.code === 0 : r.code !== 0 && r.err.includes(says), `${r.code} ${r.out}${r.err}`);
};
expect("secret 的公鑰在 TRUSTED_KEYS 裡時通過", check(trustsA, a), true);
expect("多把公鑰分行列出時，兩把金鑰一起簽也通過", check(trustsAB, `${a} ${b}`), true);
expect("常數寫在同一行時通過", check(oneLine, a), true);
expect("secret 的公鑰不在 TRUSTED_KEYS 裡時失敗", check(trustsA, c), false, `${publicHex(c)}（${keyId(c)}）不在`);
expect("換金鑰時每一把都要在清單裡", check(trustsA, `${a} ${c}`), false, publicHex(c));
expect("常數以外的地方出現的公鑰不算", check(elsewhere, c), false, "不在");
expect("常數改名時失敗，不會略過比對", check(renamed, a), false, "找不到");
expect("未設定金鑰時失敗", check(trustsA, ""), false, "未設定");
// The repository's own release.rs: its constant has to be found, or the next
// tag fails here instead. A throwaway key is never one it trusts.
expect("repo 裡的 release.rs 讀得出 TRUSTED_KEYS，測試金鑰不在其中", check(null, c), false, "不在");
const listed = check(trustsAB, `${a} ${b}`);
ok("兩邊的公鑰都印出來", listed.out.includes(publicHex(a)) && listed.out.includes(publicHex(b)), listed.out);
ok("check-trusted 的輸出不含私鑰", ![...trustedRuns, listed].some(leaks));

rmSync(dir, { recursive: true, force: true });
if (failed > 0) {
  console.error(`${failed} 項失敗`);
  process.exit(1);
}
