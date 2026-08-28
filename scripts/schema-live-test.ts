#!/usr/bin/env bun
// Do the frontend response schemas match what the server actually sends?
//
// web/src/api/client.ts declares the shape of every operation's response as a
// zod schema, and callOp validates against it. Those declarations are written
// by hand -- the operations return serde_json::Value, so there is nothing to
// generate them from -- which means they can be wrong, and a wrong one turns
// a working screen into an error message. This calls each operation for real
// and parses the answer with the schema the screens use.
//
// Run scripts/function-test.ts first: this reuses the database it leaves
// behind, so the lists have rows in them.
//
//   bun scripts/function-test.ts && bun scripts/schema-live-test.ts

import { mismatch, mismatchAgainst, wiredOperations } from "./response-schemas";

const PG_CONTAINER = process.env.PG_CONTAINER ?? "laysi-one_postgres_1";
const PG_USER = process.env.POSTGRES_USER ?? "laysi";
const TEST_DB = process.env.TEST_DB ?? "laysi_function_test";
const PORT = Number(process.env.TEST_PORT ?? "8093");
const BASE = `http://127.0.0.1:${PORT}`;
const SERVER_BIN = new URL("../backend/target/debug/laysi-server", import.meta.url).pathname;
const ACCOUNTS: [string, string, string][] = [
  ["boss", "boss@test.local", "Function-Boss-2026"],
  ["admin", "admin", "Function-Test-2026"],
];

const PG_PASSWORD =
  (
    await new Response(
      Bun.spawn(["podman", "exec", PG_CONTAINER, "cat", "/secrets/pg_password"], {
        stdout: "pipe",
        stderr: "ignore",
      }).stdout,
    ).text()
  ).trim() ||
  process.env.POSTGRES_PASSWORD ||
  "";

async function startServer() {
  const proc = Bun.spawn([SERVER_BIN], {
    env: {
      ...process.env,
      DATABASE_URL: `postgres://${PG_USER}:${PG_PASSWORD}@localhost:5432/${TEST_DB}`,
      BIND_ADDR: `127.0.0.1:${PORT}`,
      SESSION_COOKIE_SECURE: "false",
      RUST_LOG: "warn",
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  for (let i = 0; i < 60; i += 1) {
    try {
      if ((await fetch(`${BASE}/healthz`)).ok) return proc;
    } catch {}
    await Bun.sleep(250);
  }
  proc.kill();
  throw new Error(`測試伺服器沒有起來。先執行 bun scripts/function-test.ts 建好 ${TEST_DB}。`);
}

const server = await startServer();
let failed = 0;

try {
  for (const [label, username, password] of ACCOUNTS) {
    const res = await fetch(`${BASE}/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ username, password }),
    });
    if (!res.ok) throw new Error(`登入失敗 ${username}：HTTP ${res.status}`);
    const cookie = (res.headers.get("set-cookie") ?? "").split(";")[0]!;

    // The routes outside the registry. auth.ts and the sidebar read these, and
    // a wrong shape here stops the login itself rather than one screen.
    const routes: [string, string][] = [
      ["/v1/auth/me", "authEnvelope"],
      ["/v1/meta", "meta"],
      ["/v1/ops", "opList"],
    ];
    for (const [path, schema] of routes) {
      const body = await (await fetch(`${BASE}${path}`, { headers: { cookie } })).json();
      const wrong = mismatchAgainst(schema, body);
      if (wrong) {
        failed += 1;
        console.log(`  FAIL ${path}\n       ${wrong}`);
      }
    }

    let checked = 0;
    let skipped = 0;
    for (const op of wiredOperations) {
      const r = (await (
        await fetch(`${BASE}/v1/ops/${op}`, {
          method: "POST",
          headers: { "content-type": "application/json", cookie },
          body: JSON.stringify({ input: {} }),
        })
      ).json()) as { ok: boolean; data?: unknown };
      // A refusal is not a shape: the operation needs arguments this script
      // does not invent, or this role may not call it.
      if (!r.ok) {
        skipped += 1;
        continue;
      }
      checked += 1;
      const wrong = mismatch(op, r.data);
      if (wrong) {
        failed += 1;
        console.log(`  FAIL ${op}\n       ${wrong}`);
      }
    }
    console.log(`ok   ${label}：核對 ${checked} 個操作（略過 ${skipped}）`);
  }
} finally {
  server.kill();
}

console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
