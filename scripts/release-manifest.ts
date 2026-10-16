#!/usr/bin/env bun
// Signs the release manifest: which image digests a `v*` tag is made of.
//
//   RELEASE_MANIFEST_SIGNING_KEY=... bun scripts/release-manifest.ts sign \
//       --version v0.1.13 --api sha256:... --web sha256:... --backup sha256:... \
//       --out release-manifest.txt
//   RELEASE_MANIFEST_SIGNING_KEY=... bun scripts/release-manifest.ts public-key
//   RELEASE_MANIFEST_SIGNING_KEY=... bun scripts/release-manifest.ts check-trusted [--source release.rs]
//   bun scripts/release-manifest.ts keygen [--repo Laysi/laysi-one]
//
// The format, and the bytes a signature covers, are defined by the verifier in
// backend/crates/laysi-license-token/src/release.rs. This writes the same
// text: five body lines, then one `signature <key id> <signature>` line per
// key, each over exactly the body bytes. scripts/release-manifest-test.ts
// checks the two sides agree.
//
// The private key comes only from the environment, encoded like
// LICENSE_SIGNING_KEY: the 32-byte Ed25519 seed as 64 hex characters. Several
// seeds separated by whitespace sign together, which is how a key is rotated.
// Nothing here prints a seed or writes one to disk.
//
// `check-trusted` is the signing-key step of release.yml: every key the secret
// holds has to be in TRUSTED_KEYS, which the product compiles in and asks
// nobody about at run time. It reads the constant from release.rs as text, so
// the job that runs it needs no Rust toolchain.
//
// `keygen` creates the secret once. It refuses when the secret already exists:
// GitHub cannot hand a secret back, so replacing it would lose the only copy
// of a key every installation in the field trusts.

import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify, type KeyObject } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";

const SECRET = "RELEASE_MANIFEST_SIGNING_KEY";
const TRUSTED_SOURCE = join(import.meta.dir, "..", "backend", "crates", "laysi-license-token", "src", "release.rs");
const TRUSTED_DECLARATION = "pub const TRUSTED_KEYS: &[&str] =";
const HEADER = "laysi-release-manifest 1";
const IMAGES = ["api", "web", "backup"] as const;
// DER prefix of a PKCS#8 Ed25519 private key; the 32-byte seed follows it.
const PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

// The same grammar ReleaseVersion::parse accepts: semantic versioning with a
// `v`, no leading zeros, no build metadata.
const NUM = "(0|[1-9][0-9]*)";
const PRE_ID = "(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)";
const VERSION = new RegExp(`^v${NUM}\\.${NUM}\\.${NUM}(-${PRE_ID}(\\.${PRE_ID})*)?$`);
const DIGEST = /^sha256:[0-9a-f]{64}$/;

function fail(message: string): never {
  console.error(message);
  process.exit(1);
}

interface Key {
  privateKey: KeyObject;
  publicKey: KeyObject;
  publicHex: string;
  keyId: string;
}

function keyFromSeed(seed: Buffer): Key {
  const privateKey = createPrivateKey({ key: Buffer.concat([PKCS8_PREFIX, seed]), format: "der", type: "pkcs8" });
  const publicKey = createPublicKey(privateKey);
  const spki = publicKey.export({ format: "der", type: "spki" }) as Buffer;
  const raw = spki.subarray(spki.length - 32);
  return {
    privateKey,
    publicKey,
    publicHex: raw.toString("hex"),
    keyId: createHash("sha256").update(raw).digest("hex").slice(0, 16),
  };
}

function keysFromEnv(): Key[] {
  const text = (process.env[SECRET] ?? "").trim();
  if (text === "") fail(`${SECRET} 未設定`);
  const keys = text.split(/\s+/).map((hex, i) => {
    if (!/^[0-9a-fA-F]{64}$/.test(hex)) fail(`${SECRET} 的第 ${i + 1} 把金鑰應為 64 個十六進位字元`);
    const seed = Buffer.from(hex, "hex");
    const key = keyFromSeed(seed);
    seed.fill(0);
    return key;
  });
  if (new Set(keys.map((k) => k.keyId)).size !== keys.length) fail(`${SECRET} 含有重複的金鑰`);
  return keys;
}

function signManifest(args: string[]): void {
  const { values } = parseArgs({
    args,
    options: {
      version: { type: "string" },
      api: { type: "string" },
      web: { type: "string" },
      backup: { type: "string" },
      out: { type: "string" },
    },
    strict: true,
  });
  const version = values.version ?? fail("缺少 --version");
  if (!VERSION.test(version)) fail(`版本號格式應為 v1.2.3：${JSON.stringify(version)}`);
  const lines = [HEADER, `version ${version}`];
  for (const image of IMAGES) {
    const digest = (values[image] ?? fail(`缺少 --${image}`)).trim();
    if (!DIGEST.test(digest)) fail(`${image} 的 digest 應為 sha256: 加 64 個小寫十六進位字元：${JSON.stringify(digest)}`);
    lines.push(`image ${image} ${digest}`);
  }
  const out = values.out ?? fail("缺少 --out");

  const body = Buffer.from(lines.map((l) => `${l}\n`).join(""), "utf8");
  let text = body.toString("utf8");
  for (const key of keysFromEnv()) {
    const signature = sign(null, body, key.privateKey);
    // Read back what was just signed, as the licence server does with a
    // token: a key that loaded wrong fails here rather than on every host.
    if (!verify(null, body, key.publicKey, signature)) fail(`金鑰 ${key.keyId} 簽出的簽章驗不過`);
    text += `signature ${key.keyId} ${signature.toString("hex")}\n`;
  }
  writeFileSync(out, text);
}

function printPublicKeys(): void {
  for (const key of keysFromEnv()) console.log(key.publicHex);
}

// Every quoted 64-hex string from the line that declares the constant to the
// `];` that closes it, which rustfmt puts on one line or several depending on
// how many keys there are. A renamed or reshaped constant yields nothing, and
// the check fails rather than passing over it.
function trustedKeys(source: string): string[] {
  const lines = readFileSync(source, "utf8").split("\n");
  const start = lines.findIndex((line) => line.startsWith(TRUSTED_DECLARATION));
  if (start < 0) return [];
  const end = lines.findIndex((line, i) => i >= start && line.endsWith("];"));
  if (end < 0) return [];
  const quoted = lines.slice(start, end + 1).join("\n").match(/"[0-9a-f]{64}"/g) ?? [];
  return quoted.map((q) => q.slice(1, -1));
}

function checkTrusted(args: string[]): void {
  const { values } = parseArgs({
    args,
    options: { source: { type: "string", default: TRUSTED_SOURCE } },
    strict: true,
  });
  const source = values.source!;
  const trusted = trustedKeys(source);
  if (trusted.length === 0) fail(`${source} 找不到 TRUSTED_KEYS 的公鑰：常數改了名稱或寫法，比對無從進行`);
  const keys = keysFromEnv();
  console.log("產品信任的公鑰：");
  for (const hex of trusted) console.log(`  ${hex}`);
  console.log(`${SECRET} 推出的公鑰：`);
  for (const key of keys) console.log(`  ${key.publicHex}`);
  const untrusted = keys.filter((key) => !trusted.includes(key.publicHex));
  if (untrusted.length > 0) {
    fail(untrusted.map((key) => `${key.publicHex}（${key.keyId}）不在 TRUSTED_KEYS 裡`).join("\n"));
  }
}

function keygen(args: string[]): void {
  const { values } = parseArgs({
    args,
    options: { repo: { type: "string", default: "Laysi/laysi-one" } },
    strict: true,
  });
  const repo = values.repo!;

  const listed = Bun.spawnSync(["gh", "secret", "list", "--repo", repo, "--json", "name"], { stderr: "inherit" });
  if (listed.exitCode !== 0) fail(`無法讀取 ${repo} 的 secret 清單`);
  const names = (JSON.parse(listed.stdout.toString()) as { name: string }[]).map((s) => s.name);
  if (names.includes(SECRET)) fail(`${repo} 已經有 ${SECRET}；換金鑰要新舊兩把一起簽，不可直接覆寫`);

  const pair = generateKeyPairSync("ed25519");
  const der = pair.privateKey.export({ format: "der", type: "pkcs8" }) as Buffer;
  const seed = Buffer.from(der.subarray(der.length - 32));
  der.fill(0);
  const key = keyFromSeed(seed);
  const value = Buffer.from(seed.toString("hex"), "utf8");
  seed.fill(0);

  const set = Bun.spawnSync(["gh", "secret", "set", SECRET, "--repo", repo], {
    stdin: value,
    stdout: "inherit",
    stderr: "inherit",
  });
  value.fill(0);
  if (set.exitCode !== 0) fail(`${SECRET} 未寫入；私鑰已丟棄，可重新執行`);

  console.log(`key id      ${key.keyId}`);
  console.log(`public key  ${key.publicHex}`);
  console.log("把 public key 寫進 backend/crates/laysi-license-token/src/release.rs 的 TRUSTED_KEYS。");
}

const [mode, ...rest] = Bun.argv.slice(2);
switch (mode) {
  case "sign":
    signManifest(rest);
    break;
  case "public-key":
    printPublicKeys();
    break;
  case "check-trusted":
    checkTrusted(rest);
    break;
  case "keygen":
    keygen(rest);
    break;
  default:
    fail("用法：release-manifest.ts sign|public-key|check-trusted|keygen");
}
