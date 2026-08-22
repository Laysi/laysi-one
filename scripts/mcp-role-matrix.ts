#!/usr/bin/env bun
// Every role against every tool AI can see, over a real MCP client.
//
// One token per role (two, so the per-token rate limit does not cut a role's
// sweep in half), issued through issue_api_token and revoked at the end. The
// expectation for each cell comes from role_permission plus the two risk
// floors, and the call is actually made — except where a permitted call would
// write to the books, which is recorded as skipped rather than executed.
//
//   LAYSI_USER=... LAYSI_PASSWORD=... bun scripts/mcp-role-matrix.ts

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const MCP_URL = process.env.MCP_URL ?? "http://127.0.0.1:8080/mcp";
const API_URL = process.env.API_URL ?? MCP_URL.replace(/\/mcp$/, "");
const USER = process.env.LAYSI_USER;
const PASSWORD = process.env.LAYSI_PASSWORD;
const PG_CONTAINER = process.env.PG_CONTAINER ?? "laysi-one_postgres_1";
const PG_USER = process.env.POSTGRES_USER ?? "laysi";
const PG_DB = process.env.POSTGRES_DB ?? "laysi";
const TOKEN_PREFIX = "MCP 角色測試 · ";

if (!USER || !PASSWORD) {
  console.error("請設定 LAYSI_USER 與 LAYSI_PASSWORD。");
  process.exit(2);
}

async function psql(sql: string): Promise<string> {
  const proc = Bun.spawn(
    ["podman", "exec", PG_CONTAINER, "psql", "-U", PG_USER, "-d", PG_DB,
     "-v", "ON_ERROR_STOP=1", "-t", "-A", "-F", "|", "-c", sql],
    { stdout: "pipe", stderr: "pipe" },
  );
  const [out, err, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  if (code !== 0) throw new Error(`psql 失敗：${err.trim()}`);
  return out.trim();
}

async function rows(sql: string): Promise<string[][]> {
  const out = await psql(sql);
  return out ? out.split("\n").map((line) => line.split("|")) : [];
}

async function scalar(sql: string): Promise<string> {
  const r = await rows(sql);
  return r.length ? r[0][0] : "";
}

let cookie = "";

async function callOp(name: string, input: unknown = {}): Promise<any> {
  const res = await fetch(`${API_URL}/v1/ops/${name}`, {
    method: "POST",
    headers: { "content-type": "application/json", cookie },
    body: JSON.stringify({ input }),
  });
  const body = await res.json();
  if (!body.ok) throw new Error(`${name} 失敗：${body.error?.message}`);
  return body.data;
}

async function connect(secret: string): Promise<Client> {
  const transport = new StreamableHTTPClientTransport(new URL(MCP_URL), {
    requestInit: { headers: { authorization: `Bearer ${secret}` } },
  });
  const client = new Client({ name: "laysi-role-matrix", version: "1.0.0" });
  await client.connect(transport);
  return client;
}

function textOf(result: any): string {
  return String(result?.content?.[0]?.text ?? "");
}

// The three refusals the permission layers produce. Everything else — bad
// input, an unlinked employee, an empty ledger — means the call got past them.
const REFUSALS = ["未獲授權", "限負責人執行", "限系統管理員執行", "目前為關閉狀態"];

function refused(text: string): boolean {
  return REFUSALS.some((r) => text.includes(r));
}

const problems: string[] = [];

async function main() {
  const login = await fetch(`${API_URL}/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ username: USER, password: PASSWORD }),
  });
  const loginBody = await login.json();
  if (!loginBody.ok) throw new Error(`登入失敗：${JSON.stringify(loginBody)}`);
  cookie = (login.headers.getSetCookie?.() ?? []).map((c) => c.split(";")[0]).join("; ");

  const roles = (await rows("SELECT code, name FROM role ORDER BY code")).map(([code, name]) => ({ code, name }));
  const grants = (await rows("SELECT tool_name, risk FROM mcp_tool_grant WHERE enabled ORDER BY tool_name"))
    .map(([tool, risk]) => ({ tool, risk }));
  const offGrants = await rows("SELECT tool_name, risk FROM mcp_tool_grant WHERE NOT enabled");

  const permitted = new Map<string, Set<string>>();
  for (const role of roles) {
    const r = await rows(
      `SELECT unnest(p.op_names) FROM role_permission rp JOIN permission p ON p.code = rp.permission_code
       WHERE rp.role_code = $$${role.code}$$`,
    );
    permitted.set(role.code, new Set(r.map((x) => x[0])));
  }

  const auditStart = Number((await scalar("SELECT coalesce(max(id),0) FROM ai_audit_log")) || 0);

  console.log(`MCP 角色矩陣 · ${MCP_URL}`);
  console.log(`${roles.length} 個角色 × ${grants.length} 個對 AI 開著的工具\n`);

  console.log("不變量");
  const riskyOn = grants.filter((g) => g.risk === "admin_only" || g.risk === "boss_only");
  if (riskyOn.length === 0) {
    console.log(`  ok   INV1  沒有任何負責人／系統管理員底線的工具對 AI 開著（${offGrants.filter((g) => g[1] === "admin_only" || g[1] === "boss_only").length} 個全關）`);
  } else {
    problems.push(`INV1：${riskyOn.map((g) => g.tool).join("、")} 帶著身分底線卻對 AI 開著`);
    console.log(`  FAIL INV1  ${riskyOn.map((g) => g.tool).join("、")}`);
  }

  const tokens: number[] = [];
  const results: Record<string, { allowed: string[]; refused: string[]; skipped: string[]; wrong: string[] }> = {};
  let listedSignature = "";
  let sawTwoListings = false;

  try {
    for (const role of roles) {
      const a = await callOp("issue_api_token", {
        name: `${TOKEN_PREFIX}${role.name} A`, kind: "mcp", scope: "測試",
        actor: `MCP 角色測試 ${role.name}`, actor_role: role.code,
        rotate_days: 1, expiry_note: "測試結束即撤銷",
      });
      const b = await callOp("issue_api_token", {
        name: `${TOKEN_PREFIX}${role.name} B`, kind: "mcp", scope: "測試",
        actor: `MCP 角色測試 ${role.name}`, actor_role: role.code,
        rotate_days: 1, expiry_note: "測試結束即撤銷",
      });
      tokens.push(a.token.id, b.token.id);
      const actor = `MCP 角色測試 ${role.name}`;
      const clients = [await connect(a.secret), await connect(b.secret)];

      const listedNames = (await clients[0].listTools()).tools.map((t) => t.name).sort();
      if (!listedSignature) listedSignature = listedNames.join(",");
      else if (listedNames.join(",") !== listedSignature) sawTwoListings = true;

      const bucket = { allowed: [] as string[], refused: [] as string[], skipped: [] as string[], wrong: [] as string[] };
      let i = 0;
      for (const { tool, risk } of grants) {
        const hasPermission = permitted.get(role.code)!.has(tool);
        const floorOk =
          risk === "boss_only" ? role.code === "boss" : risk === "admin_only" ? role.code === "admin" : true;
        const shouldAllow = hasPermission && floorOk;

        if (shouldAllow && risk !== "read_only") {
          // A permitted write would really post to the books. Not executed.
          bucket.skipped.push(tool);
          continue;
        }

        const client = clients[i++ % clients.length];
        const r = await client.callTool({ name: tool, arguments: {} });
        const text = textOf(r);
        const wasRefused = r.isError === true && refused(text);

        if (shouldAllow && wasRefused) {
          bucket.wrong.push(`${tool}：應可執行卻被拒絕（${text}）`);
        } else if (!shouldAllow && !wasRefused) {
          bucket.wrong.push(`${tool}：應被拒絕卻執行了（isError=${r.isError}，${text.slice(0, 60)}）`);
        } else if (shouldAllow) {
          bucket.allowed.push(tool);
        } else {
          bucket.refused.push(tool);
        }
      }

      for (const c of clients) await c.close();
      results[role.code] = bucket;

      // The list must be exactly what this role can call: what the sweep
      // executed, plus the writes it deliberately did not.
      const shouldSee = [...bucket.allowed, ...bucket.skipped].sort();
      if (JSON.stringify(listedNames) !== JSON.stringify(shouldSee)) {
        const extra = listedNames.filter((n) => !shouldSee.includes(n));
        const missing = shouldSee.filter((n) => !listedNames.includes(n));
        problems.push(
          `INV2：${role.name} 的 tools/list 有 ${listedNames.length} 個，可叫用的是 ${shouldSee.length} 個` +
            (extra.length ? `；多出 ${extra.join("、")}` : "") +
            (missing.length ? `；少了 ${missing.join("、")}` : ""),
        );
      }

      // Every refusal must leave a blocked audit row under this actor.
      const blocked = Number(
        await scalar(
          `SELECT count(*) FROM ai_audit_log WHERE actor = $$${actor}$$ AND outcome = $$blocked$$ AND id > ${auditStart}`,
        ),
      );
      const auditOk = blocked === bucket.refused.length + bucket.wrong.filter((w) => w.includes("應可執行")).length;
      if (!auditOk) {
        problems.push(`INV3：${role.name} 被拒 ${bucket.refused.length} 次，稽核只有 ${blocked} 筆 blocked`);
      }
      if (bucket.wrong.length) problems.push(...bucket.wrong.map((w) => `${role.name} · ${w}`));

      const flag = bucket.wrong.length ? "FAIL" : "ok  ";
      console.log(
        `  ${flag} ${role.code.padEnd(10)} ${role.name.padEnd(6)} 清單 ${String(listedNames.length).padStart(2)}` +
          ` · 叫得動 ${String(bucket.allowed.length).padStart(2)}` +
          ` · 被拒 ${String(bucket.refused.length).padStart(2)} · 未執行的寫入類 ${String(bucket.skipped.length).padStart(2)}` +
          ` · 稽核 blocked ${String(blocked).padStart(2)}`,
      );
    }
  } finally {
    for (const id of tokens) {
      try {
        await callOp("revoke_api_token", { id });
      } catch {}
    }
  }

  console.log("\n各角色叫得動的唯讀工具");
  for (const role of roles) {
    const b = results[role.code];
    if (!b) continue;
    console.log(`  ${role.name}（${role.code}）`);
    console.log(`    執行：${b.allowed.length ? b.allowed.join("、") : "（無）"}`);
    if (b.skipped.length) console.log(`    有權限但會寫入，未執行：${b.skipped.join("、")}`);
  }

  const auditEnd = Number(await scalar("SELECT coalesce(max(id),0) FROM ai_audit_log"));
  const live = await scalar(`SELECT count(*) FROM api_token WHERE name LIKE $$${TOKEN_PREFIX}%$$ AND revoked_at IS NULL`);
  if (!sawTwoListings) {
    problems.push("INV2：七個角色的 tools/list 完全相同，角色過濾沒有生效");
  }
  console.log(`\n新增稽核紀錄 ${auditEnd - auditStart} 筆，測試 Token 未撤銷 ${live} 組`);
  if (problems.length) {
    console.log(`\n${problems.length} 項不符：`);
    for (const p of problems) console.log(`  · ${p}`);
    process.exit(1);
  }
  console.log("全部符合 role_permission 與兩道身分底線");
}

await main();
