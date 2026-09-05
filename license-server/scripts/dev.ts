#!/usr/bin/env bun
/**
 * Development server: a throwaway PostgreSQL in podman, a throwaway signing
 * key, and `cargo watch` on top of the two.
 *
 *   bun scripts/dev.ts          # start the database, then the server
 *   bun scripts/dev.ts stop     # stop the database, keep its data
 *   bun scripts/dev.ts reset    # throw the database away and start over
 *
 * Nothing here reads `secrets/`. That directory holds the key this
 * installation has already signed licences with, and a development run that
 * borrowed it would mint tokens the field accepts. The key under `.dev/` is
 * generated once, git-ignored, and meant to be thrown away.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";

const ROOT = new URL("..", import.meta.url).pathname.replace(/\/$/, "");

// The database is loopback-only and lives no longer than the branch, so these
// are constants rather than settings: there is nothing here to protect, and
// nothing worth asking anybody to invent a value for.
const CONTAINER = "laysi-one-license-dev-db";
const VOLUME = "laysi-one-license-dev-pgdata";
const IMAGE = "docker.io/library/postgres:18";
const PG_USER = "laysi";
const PG_DB = "license";
const PG_PASSWORD = "dev";
// 5432 belongs to whichever laysi-one stack is already up on this machine.
const PG_PORT = 5433;
const BIND_ADDR = "127.0.0.1:8090";
const VENDOR_EMAIL = "ruisi@laysi.io";

const DATABASE_URL = `postgres://${PG_USER}:${PG_PASSWORD}@127.0.0.1:${PG_PORT}/${PG_DB}`;

function podman(args: string[]) {
  return Bun.spawnSync(["podman", ...args], { stdout: "pipe", stderr: "pipe" });
}

/** `running`, `exited`, … or `null` when no such container exists. */
function containerState(): string | null {
  const state = podman([
    "ps", "-a", "--filter", `name=^${CONTAINER}$`, "--format", "{{.State}}",
  ]).stdout.toString().trim();
  return state === "" ? null : state;
}

function startDatabase() {
  const state = containerState();
  if (state === "running") {
    console.log(`資料庫 ${CONTAINER} 已在執行。`);
    return;
  }
  if (state !== null) {
    console.log(`啟動既有的資料庫 ${CONTAINER}…`);
    run("podman", ["start", CONTAINER]);
    return;
  }
  console.log(`建立資料庫 ${CONTAINER}（連接埠 ${PG_PORT}）…`);
  // The same initdb arguments as the compose stack. Collation decides the
  // order of every listing in the console, so a development database that
  // sorts differently hides exactly the bug nobody would look for.
  run("podman", [
    "run", "-d",
    "--name", CONTAINER,
    "-e", `POSTGRES_USER=${PG_USER}`,
    "-e", `POSTGRES_PASSWORD=${PG_PASSWORD}`,
    "-e", `POSTGRES_DB=${PG_DB}`,
    "-e", "POSTGRES_INITDB_ARGS=--encoding=UTF8 --locale-provider=icu --icu-locale=zh-TW --locale=C.UTF-8",
    "-v", `${VOLUME}:/var/lib/postgresql`,
    "-p", `127.0.0.1:${PG_PORT}:5432`,
    IMAGE,
  ]);
}

async function waitForDatabase() {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    if (podman(["exec", CONTAINER, "pg_isready", "-U", PG_USER, "-d", PG_DB]).exitCode === 0) return;
    await Bun.sleep(1000);
  }
  fail(`資料庫在 60 秒內沒有就緒，請看 podman logs ${CONTAINER}`);
}

/**
 * The signing key for development runs: generated on first use and then left
 * alone, so a licence file signed yesterday still verifies today.
 */
function signingKey(): string {
  const path = `${ROOT}/.dev/license_signing_key`;
  if (existsSync(path)) return readFileSync(path, "utf8").trim();
  const seed = crypto.getRandomValues(new Uint8Array(32));
  const hex = Array.from(seed, (byte) => byte.toString(16).padStart(2, "0")).join("");
  mkdirSync(`${ROOT}/.dev`, { recursive: true });
  writeFileSync(path, `${hex}\n`, { mode: 0o600 });
  console.log("已產生開發用簽章金鑰 .dev/license_signing_key，與正式金鑰無關，刪掉再跑即換一把。");
  return hex;
}

/** Locate a tool in `~/.cargo/bin` as well as on PATH: this may run from an
 *  editor or a hook, where the interactive shell's PATH is not in effect. */
function cargoTool(name: string): string | null {
  return Bun.which(name) ?? (existsSync(`${process.env.HOME}/.cargo/bin/${name}`)
    ? `${process.env.HOME}/.cargo/bin/${name}`
    : null);
}

function serverEnv(): Record<string, string> {
  return {
    ...process.env,
    // Appended rather than prepended: `~/.cargo/bin/cargo` is the rustup shim,
    // and putting it first hands the build to rustup's default toolchain
    // instead of the one proto pins here. Two compilers sharing one `target`
    // fail with E0514 on the first crate the other one built.
    PATH: `${process.env.PATH}:${process.env.HOME}/.cargo/bin`,
    DATABASE_URL,
    LICENSE_SIGNING_KEY: signingKey(),
    BIND_ADDR,
    RUST_LOG: process.env.RUST_LOG ?? "info,laysi_one_license=debug",
  } as Record<string, string>;
}

/**
 * The first console account. `create-vendor` applies the schema on its way
 * in, so a fresh volume needs nothing else. An account that already exists is
 * left alone: re-running it would reset a password somebody may have changed.
 */
function bootstrapVendor(cargo: string, env: Record<string, string>) {
  const existing = podman([
    "exec", CONTAINER,
    "psql", "-U", PG_USER, "-d", PG_DB, "-tAc", "SELECT 1 FROM partner_user LIMIT 1",
  ]);
  if (existing.exitCode === 0 && existing.stdout.toString().trim() !== "") return;

  console.log("建立第一個後台帳號…");
  const created = Bun.spawnSync([cargo, "run", "--quiet", "--", "create-vendor", VENDOR_EMAIL], {
    cwd: ROOT,
    env,
    stdout: "pipe",
    stderr: "inherit",
  });
  if (created.exitCode !== 0) fail("create-vendor 失敗。");
  const account = created.stdout.toString().trim();
  writeFileSync(`${ROOT}/.dev/vendor.txt`, `${account}\n`, { mode: 0o600 });
  console.log(`${account}\n（同一份寫在 .dev/vendor.txt）`);
}

function run(command: string, args: string[]) {
  const done = Bun.spawnSync([command, ...args], { cwd: ROOT, stdout: "inherit", stderr: "inherit" });
  if (done.exitCode !== 0) fail(`${command} ${args.join(" ")} 失敗。`);
}

function fail(message: string): never {
  console.error(message);
  process.exit(1);
}

async function start() {
  startDatabase();
  await waitForDatabase();

  const cargo = cargoTool("cargo") ?? fail("找不到 cargo，請先安裝 Rust 工具鏈：proto install rust");
  const env = serverEnv();
  bootstrapVendor(cargo, env);

  // cargo-watch rebuilds on save. Without it this is the same server with one
  // more keystroke per change, which is not worth stopping for.
  const watching = cargoTool("cargo-watch") !== null;
  if (!watching) {
    console.log("未安裝 cargo-watch，改為一次性執行；cargo install cargo-watch 可開啟存檔即重編。");
  }
  const args = watching
    ? ["watch", "--why", "-w", "src", "-w", "schema.sql", "-x", "run"]
    : ["run"];

  console.log(`\n後台     http://${BIND_ADDR}`);
  console.log(`檢查端點 POST http://${BIND_ADDR}/v1/license/check`);
  console.log(`資料庫   ${DATABASE_URL}`);
  console.log("停止資料庫 bun scripts/dev.ts stop ／ 清空重建 bun scripts/dev.ts reset\n");

  const server = Bun.spawn([cargo, ...args], {
    cwd: ROOT,
    env,
    stdout: "inherit",
    stderr: "inherit",
    stdin: "inherit",
  });
  process.exit(await server.exited);
}

function stop() {
  if (containerState() === null) {
    console.log("沒有開發資料庫。");
    return;
  }
  run("podman", ["stop", CONTAINER]);
  console.log("資料庫已停止，資料還在。");
}

function reset() {
  if (containerState() !== null) run("podman", ["rm", "-f", CONTAINER]);
  podman(["volume", "rm", VOLUME]);
  console.log("開發資料庫已清空，下次啟動會重新建立，包含後台帳號。");
}

switch (process.argv[2]) {
  case undefined:
  case "start":
    await start();
    break;
  case "stop":
    stop();
    break;
  case "reset":
    reset();
    break;
  default:
    fail("用法：bun scripts/dev.ts [start|stop|reset]");
}
