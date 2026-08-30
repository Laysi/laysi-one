#!/usr/bin/env bun
// End-to-end MCP test against a running laysi-server, driven by the official
// MCP client SDK. The cases and their reasons are in
// docs/mcp-test-checklist.md; the ids below match that file.
//
// Nothing is faked. The tokens come from the real issue_api_token operation
// over an authenticated session, the protocol runs through the same client
// library an AI connector uses, and revocation goes through revoke_api_token.
// The one exception is B5, which needs a token older than its rotation
// period: there is no way to wait a day, so it moves created_at in the
// database and says so.
//
//   cd scripts && bun install
//   LAYSI_USER=... LAYSI_PASSWORD=... bun scripts/mcp-test.ts

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const MCP_URL = process.env.MCP_URL ?? "http://127.0.0.1:8080/mcp";
const API_URL = process.env.API_URL ?? MCP_URL.replace(/\/mcp$/, "");
const USER = process.env.LAYSI_USER;
const PASSWORD = process.env.LAYSI_PASSWORD;
const PG_CONTAINER = process.env.PG_CONTAINER ?? "laysi-one_postgres_1";
const PG_USER = process.env.POSTGRES_USER ?? "laysi";
const PG_DB = process.env.POSTGRES_DB ?? "laysi";
const RATE_LIMIT = Number(process.env.MCP_RATE_LIMIT_PER_MINUTE ?? "60");
const TOKEN_PREFIX = "MCP 測試 · ";

if (!USER || !PASSWORD) {
  console.error("請設定 LAYSI_USER 與 LAYSI_PASSWORD；核發 Token 需要系統管理員的登入身分。");
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

type Rpc = { status: number; text: string; json: any };

// The low level cases below cannot go through the SDK: a client library will
// not send a malformed body, omit a method, or forge an identity header.
async function rpc(
  token: string | null,
  body: unknown,
  extraHeaders: Record<string, string> = {},
): Promise<Rpc> {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
    ...extraHeaders,
  };
  if (token) headers.authorization = `Bearer ${token}`;
  const res = await fetch(MCP_URL, {
    method: "POST",
    headers,
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
  const text = await res.text();
  let json: any = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {}
  return { status: res.status, text, json };
}

async function connect(secret: string): Promise<Client> {
  const transport = new StreamableHTTPClientTransport(new URL(MCP_URL), {
    requestInit: { headers: { authorization: `Bearer ${secret}` } },
  });
  const client = new Client({ name: "laysi-mcp-test", version: "1.0.0" });
  await client.connect(transport);
  return client;
}

function textOf(result: any): string {
  return String(result?.content?.[0]?.text ?? "");
}

type Case = { id: string; name: string; ok: boolean; note: string };
const cases: Case[] = [];

async function check(id: string, name: string, fn: () => Promise<string>) {
  try {
    const note = await fn();
    cases.push({ id, name, ok: true, note });
    console.log(`  ok   ${id}  ${name}${note ? `  — ${note}` : ""}`);
  } catch (error) {
    const note = error instanceof Error ? error.message : String(error);
    cases.push({ id, name, ok: false, note });
    console.log(`  FAIL ${id}  ${name}\n         ${note}`);
  }
}

function skip(id: string, name: string, why: string) {
  cases.push({ id, name, ok: true, note: `略過：${why}` });
  console.log(`  skip ${id}  ${name}  — ${why}`);
}

function expect(condition: unknown, message: string) {
  if (!condition) throw new Error(message);
}

type Token = { id: number; secret: string; actor: string };

async function issue(label: string, actor: string, role: string, kind = "mcp", rotateDays?: number): Promise<Token> {
  const data = await callOp("issue_api_token", {
    name: `${TOKEN_PREFIX}${label}`,
    kind,
    scope: "測試",
    actor,
    actor_role: role,
    rotate_days: rotateDays ?? null,
    expiry_note: "測試結束即撤銷",
  });
  return { id: data.token.id, secret: data.secret, actor };
}

async function lastAudit(tool: string, actor: string) {
  const r = await rows(
    `SELECT id, actor, actor_role, outcome, channel, coalesce(prompt,''), result_summary, risk, params::text
     FROM ai_audit_log WHERE tool_name = $$${tool}$$ AND actor = $$${actor}$$
     ORDER BY id DESC LIMIT 1`,
  );
  expect(r.length === 1, `找不到 ${tool} / ${actor} 的稽核紀錄`);
  const [id, auditActor, role, outcome, channel, prompt, summary, risk, params] = r[0];
  return { id: Number(id), actor: auditActor, role, outcome, channel, prompt, summary, risk, params };
}

async function main() {
  const enabledTools = Number(await scalar("SELECT count(*) FROM mcp_tool_grant WHERE enabled"));
  const disabledTools = Number(await scalar("SELECT count(*) FROM mcp_tool_grant WHERE NOT enabled"));
  const auditStart = Number((await scalar("SELECT coalesce(max(id),0) FROM ai_audit_log")) || 0);

  console.log(`MCP 端對端測試 · ${MCP_URL}`);
  console.log(`工具開關 ${enabledTools} 開 / ${disabledTools} 關；註冊表的筆數由 E5 核對\n`);

  console.log("L 前置：真實登入與真實核發");
  await check("L1", "帳號密碼登入取得 session", async () => {
    const res = await fetch(`${API_URL}/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ username: USER, password: PASSWORD }),
    });
    const body = await res.json();
    expect(res.status === 200 && body.ok, JSON.stringify(body));
    cookie = (res.headers.getSetCookie?.() ?? []).map((c) => c.split(";")[0]).join("; ");
    expect(cookie.length > 0, "沒有拿到 session cookie");
    expect(body.user.role === "admin", `登入身分為 ${body.user.role_name}，核發 Token 需要系統管理員`);
    return body.user.display_name;
  });

  const tokens: Token[] = [];
  let admin!: Token, staff!: Token, apiKind!: Token, revoked!: Token, expired!: Token, touch!: Token, rate!: Token;

  await check("L2", "issue_api_token 核發 MCP Token（畫面通道不受工具開關影響）", async () => {
    const off = await scalar("SELECT enabled FROM mcp_tool_grant WHERE tool_name = $$issue_api_token$$");
    expect(off === "f", "此案例預期 issue_api_token 對 AI 是關閉的");
    admin = await issue("系統管理員", "MCP 測試管理員", "admin");
    staff = await issue("一般職員", "MCP 測試職員", "staff");
    apiKind = await issue("API 類型", "MCP 測試 API", "admin", "api");
    revoked = await issue("待撤銷", "MCP 測試已撤銷", "admin");
    expired = await issue("待過期", "MCP 測試已過期", "admin", "mcp", 1);
    touch = await issue("最後使用時間", "MCP 測試最後使用", "admin");
    rate = await issue("速率限制", "MCP 測試速率", "admin");
    tokens.push(admin, staff, apiKind, revoked, expired, touch, rate);
    return `${tokens.length} 組，明文只在本次回應出現`;
  });

  await check("L3", "核發的明文不進資料庫，也不進稽核", async () => {
    const stored = await scalar(`SELECT token_hash FROM api_token WHERE id = ${admin.id}`);
    expect(stored !== admin.secret, "資料庫存了明文");
    expect(stored.length === 64, `token_hash 長度 ${stored.length}`);
    const leaked = await scalar(
      `SELECT count(*) FROM ai_audit_log WHERE params::text LIKE $$%${admin.secret}%$$
                                            OR result_summary LIKE $$%${admin.secret}%$$`,
    );
    expect(leaked === "0", `稽核裡出現了 ${leaked} 次明文`);
    return "";
  });

  let client!: Client;

  try {
    console.log("\nA 協定（官方 MCP 用戶端 SDK）");
    await check("A1", "SDK 連線握手成功並取得伺服器資訊", async () => {
      client = await connect(admin.secret);
      const info = client.getServerVersion();
      expect(info?.name === "laysi-one", `serverInfo.name = ${info?.name}`);
      expect(client.getServerCapabilities()?.tools?.listChanged === false, "capabilities.tools 不正確");
      const instructions = client.getInstructions() ?? "";
      expect(instructions.length > 0, "缺少 instructions");
      return `${info?.name} ${info?.version}`;
    });

    await check("A2", "協定版本協商為 2025-06-18", async () => {
      const r = await rpc(admin.secret, { jsonrpc: "2.0", id: 1, method: "initialize" });
      expect(r.json?.result?.protocolVersion === "2025-06-18", `${r.json?.result?.protocolVersion}`);
      return r.json.result.protocolVersion;
    });

    await check("A3", "SDK 的 ping 有回應", async () => {
      await client.ping();
      return "";
    });

    await check("A4", "未知 method 回 -32601", async () => {
      const r = await rpc(admin.secret, { jsonrpc: "2.0", id: 3, method: "resources/list" });
      expect(r.json?.error?.code === -32601, JSON.stringify(r.json));
      return r.json.error.message;
    });

    await check("A5", "缺少 method 回 -32600", async () => {
      const r = await rpc(admin.secret, { jsonrpc: "2.0", id: 4 });
      expect(r.json?.error?.code === -32600, JSON.stringify(r.json));
      return r.json.error.message;
    });

    await check("A6", "body 不是 JSON 回 400", async () => {
      const r = await rpc(admin.secret, "{ 這不是 JSON");
      expect(r.status === 400, `HTTP ${r.status}`);
      expect(r.json?.error?.code === -32600, JSON.stringify(r.json));
      return "";
    });

    await check("A7", "回應原樣帶回請求的 id", async () => {
      const r = await rpc(admin.secret, { jsonrpc: "2.0", id: "abc-1", method: "ping" });
      expect(r.json?.id === "abc-1", `id = ${JSON.stringify(r.json?.id)}`);
      const nullId = await rpc(admin.secret, { jsonrpc: "2.0", id: null, method: "ping" });
      expect(nullId.json?.id === null, `null id = ${JSON.stringify(nullId.json?.id)}`);
      return "";
    });

    console.log("\nB 認證");
    await check("B1", "沒有 Authorization 標頭回 401", async () => {
      const r = await rpc(null, { jsonrpc: "2.0", id: 1, method: "initialize" });
      expect(r.status === 401, `HTTP ${r.status}`);
      return "";
    });

    await check("B2", "錯誤的 Bearer 回 401，SDK 連不上", async () => {
      const r = await rpc("lys_" + "0".repeat(64), { jsonrpc: "2.0", id: 1, method: "ping" });
      expect(r.status === 401, `HTTP ${r.status}`);
      let failed = false;
      try {
        await connect("lys_" + "0".repeat(64));
      } catch {
        failed = true;
      }
      expect(failed, "SDK 竟然連得上");
      return "";
    });

    await check("B3", "kind = api 的 Token 不能用於 MCP", async () => {
      const r = await rpc(apiKind.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
      expect(r.status === 401, `HTTP ${r.status}`);
      return "";
    });

    await check("B4", "revoke_api_token 撤銷後立刻失效並寫稽核", async () => {
      const before = await rpc(revoked.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
      expect(before.status === 200, `撤銷前應可用，HTTP ${before.status}`);
      await callOp("revoke_api_token", { id: revoked.id });
      const after = await rpc(revoked.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
      expect(after.status === 401, `撤銷後 HTTP ${after.status}`);
      const audit = await lastAudit("mcp_auth", revoked.actor);
      expect(audit.outcome === "blocked", `outcome = ${audit.outcome}`);
      expect(audit.summary.includes("已撤銷"), audit.summary);
      return audit.summary;
    });

    await check("B5", "超過輪替期限的 Token 回 401，且 revoked_at 仍為空", async () => {
      // No way to wait out a rotation period inside a test, so the row's
      // created_at moves back. Everything else about the token is real.
      await psql(`UPDATE api_token SET created_at = now() - interval '2 days' WHERE id = ${expired.id}`);
      const r = await rpc(expired.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
      expect(r.status === 401, `HTTP ${r.status}`);
      const audit = await lastAudit("mcp_auth", expired.actor);
      expect(audit.summary.includes("已過期"), audit.summary);
      const stillNull = await scalar(`SELECT revoked_at IS NULL FROM api_token WHERE id = ${expired.id}`);
      expect(stillNull === "t", "過期不應寫入 revoked_at");
      return `${audit.summary}（created_at 由測試移動）`;
    });

    await check("B6", "撤銷與過期對外都是同一句 401", async () => {
      const a = await rpc(revoked.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
      const b = await rpc(expired.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
      expect(a.status === b.status && a.text === b.text, `${a.text} vs ${b.text}`);
      return a.json?.error?.message ?? "";
    });

    await check("B7", "偽造的身分標頭無效，身分只來自 Token 資料列", async () => {
      const forged = await rpc(
        admin.secret,
        { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "get_system_health", arguments: {} } },
        { "x-laysi-actor": "forged-boss", "x-laysi-role": "boss" },
      );
      expect(forged.json?.result?.isError === false, forged.text.slice(0, 200));
      const audit = await lastAudit("get_system_health", admin.actor);
      expect(audit.role === "admin", `actor_role = ${audit.role}`);
      const forgedRows = await scalar(`SELECT count(*) FROM ai_audit_log WHERE actor = $$forged-boss$$`);
      expect(forgedRows === "0", `偽造身分留下了 ${forgedRows} 筆稽核`);
      return "稽核記的是 Token 上的身分";
    });

    await check("B8", "認證先於 body 解析：壞 Token 加壞 body 回 401 不是 400", async () => {
      const r = await rpc("lys_bad", "{ 這不是 JSON");
      expect(r.status === 401, `HTTP ${r.status}`);
      return "";
    });

    await check("B9", "成功呼叫後更新 last_used_at", async () => {
      const before = await scalar(`SELECT last_used_at IS NULL FROM api_token WHERE id = ${touch.id}`);
      expect(before === "t", "測試 Token 一開始就有 last_used_at");
      const c = await connect(touch.secret);
      await c.ping();
      await c.close();
      const after = await scalar(`SELECT last_used_at IS NOT NULL FROM api_token WHERE id = ${touch.id}`);
      expect(after === "t", "last_used_at 未更新");
      return "";
    });

    console.log("\nC Origin");
    await check("C1", "沒有帶 Origin 的請求放行", async () => {
      const r = await rpc(admin.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
      expect(r.status === 200, `HTTP ${r.status}`);
      return "";
    });

    await check("C2", "帶 Origin 而白名單為空時回 403", async () => {
      const r = await rpc(admin.secret, { jsonrpc: "2.0", id: 1, method: "ping" }, { origin: "https://evil.example" });
      expect(r.status === 403, `HTTP ${r.status}`);
      expect(String(r.json?.error?.message).includes("Origin"), r.text.slice(0, 200));
      return r.json.error.message;
    });

    await check("C3", "Origin 檢查先於認證", async () => {
      const r = await rpc(null, { jsonrpc: "2.0", id: 1, method: "ping" }, { origin: "https://evil.example" });
      expect(r.status === 403, `HTTP ${r.status}（未帶 Token 也應先被 Origin 擋下）`);
      return "";
    });

    console.log("\nD 速率限制");
    if (RATE_LIMIT === 0) {
      skip("D1", "速率限制", "MCP_RATE_LIMIT_PER_MINUTE = 0");
      skip("D2", "速率限制的稽核", "MCP_RATE_LIMIT_PER_MINUTE = 0");
    } else {
      await check("D1", `第 ${RATE_LIMIT + 1} 次呼叫回 429 與 -32029`, async () => {
        // A window boundary mid-run would reset the counter and hide the
        // limit, so wait for a fresh minute when this one is nearly over.
        const secondsLeft = 60 - (Math.floor(Date.now() / 1000) % 60);
        if (secondsLeft < 15) await Bun.sleep((secondsLeft + 1) * 1000);
        let blocked: Rpc | null = null;
        for (let i = 0; i < RATE_LIMIT + 1; i++) {
          const r = await rpc(rate.secret, { jsonrpc: "2.0", id: i, method: "ping" });
          if (r.status !== 200) {
            blocked = r;
            expect(i === RATE_LIMIT, `第 ${i + 1} 次就被擋下，預期第 ${RATE_LIMIT + 1} 次`);
            break;
          }
        }
        expect(blocked !== null, `${RATE_LIMIT + 1} 次呼叫都通過了`);
        expect(blocked!.status === 429, `HTTP ${blocked!.status}`);
        expect(blocked!.json?.error?.code === -32029, JSON.stringify(blocked!.json));
        return blocked!.json.error.message;
      });

      await check("D2", "被速率限制擋下時寫稽核", async () => {
        const audit = await lastAudit("mcp_rate_limit", rate.actor);
        expect(audit.outcome === "blocked", `outcome = ${audit.outcome}`);
        expect(audit.channel === "mcp", `channel = ${audit.channel}`);
        return audit.summary;
      });
    }

    console.log("\nE tools/list（SDK）");
    const listed = await client.listTools();
    const tools = listed.tools;

    // What this token can actually call: the tool switch, the risk floor and
    // the role's permissions, the same three questions dispatch asks.
    const callableSql = (role: string) => `
      SELECT g.tool_name FROM mcp_tool_grant g
       WHERE g.enabled
         AND (g.risk <> 'boss_only' OR $$${role}$$ = 'boss')
         AND (g.risk <> 'admin_only' OR $$${role}$$ = 'admin')
         AND g.tool_name IN (
           SELECT unnest(p.op_names) FROM role_permission rp
             JOIN permission p ON p.code = rp.permission_code
            WHERE rp.role_code = $$${role}$$)
       ORDER BY g.tool_name`;

    await check("E1", "只列出這組 Token 真的叫得動的工具", async () => {
      const callable = (await rows(callableSql("admin"))).map((r) => r[0]);
      const names = tools.map((t) => t.name).sort();
      expect(
        JSON.stringify(names) === JSON.stringify(callable.sort()),
        `列出 ${names.length} 個，可叫用的是 ${callable.length} 個：${names.filter((n) => !callable.includes(n)).join("、") || "（缺）" }`,
      );
      expect(names.length < enabledTools, `開著的有 ${enabledTools} 個，角色過濾應該更少`);
      return `${names.length} 個（開著的共 ${enabledTools} 個）`;
    });

    await check("E6", "換一個角色就換一份清單", async () => {
      const c = await connect(staff.secret);
      const staffTools = (await c.listTools()).tools.map((t) => t.name).sort();
      await c.close();
      const callable = (await rows(callableSql("staff"))).map((r) => r[0]).sort();
      expect(
        JSON.stringify(staffTools) === JSON.stringify(callable),
        `職員看到 ${staffTools.length} 個，可叫用的是 ${callable.length} 個`,
      );
      expect(!staffTools.includes("get_system_health"), "職員不該看到系統現況");
      expect(staffTools.length !== tools.length, "兩個角色看到同一份清單，角色過濾沒有生效");
      return `職員 ${staffTools.length} 個 vs 系統管理員 ${tools.length} 個`;
    });

    await check("E2", "關掉的工具不出現在清單上", async () => {
      const off = await rows("SELECT tool_name FROM mcp_tool_grant WHERE NOT enabled");
      const names = new Set(tools.map((t) => t.name));
      const leaked = off.map((r) => r[0]).filter((n) => names.has(n));
      expect(leaked.length === 0, `外洩：${leaked.join("、")}`);
      return `${off.length} 個關閉的工具都沒有列出`;
    });

    await check("E3", "每個工具的說明都帶風險標籤", async () => {
      const bad = tools.filter((t) => !/^\[[^\]]+\]\s/.test(t.description ?? ""));
      expect(bad.length === 0, `缺標籤：${bad.map((t) => t.name).join("、")}`);
      return [...new Set(tools.map((t) => t.description!.match(/^\[([^\]]+)\]/)![1]))].join("、");
    });

    await check("E4", "SDK 收得下每個工具的 inputSchema", async () => {
      const bad = tools.filter((t) => !t.inputSchema || t.inputSchema.type !== "object");
      expect(bad.length === 0, `schema 不是 object：${bad.map((t) => t.name).join("、")}`);
      return "";
    });

    await check("E5", "工具開關名稱集合等於註冊表", async () => {
      // The registry is only reachable over an authenticated /v1/ops, so the
      // count is read here rather than up front: /healthz answers whether the
      // process is serving, not what it serves.
      const registry: string[] = await fetch(`${API_URL}/v1/ops`, { headers: { cookie } })
        .then((r) => r.json())
        .then((d: any) => d.operations.map((o: any) => o.name));
      expect(
        enabledTools + disabledTools === registry.length,
        `開關 ${enabledTools + disabledTools} 筆，註冊表 ${registry.length} 個操作`,
      );
      const orphan = await rows(
        `SELECT tool_name FROM mcp_tool_grant WHERE tool_name NOT IN (
           SELECT jsonb_array_elements_text($$${JSON.stringify(registry)}$$::jsonb))`,
      );
      expect(orphan.length === 0, `開關裡有註冊表沒有的名稱：${orphan.map((r) => r[0]).join("、")}`);
      return `${registry.length} 個操作`;
    });

    console.log("\nF tools/call（SDK）");
    await check("F1", "開著又有權限的唯讀工具可以呼叫", async () => {
      const r = await client.callTool({ name: "get_system_health", arguments: {} });
      expect(r.isError === false, textOf(r));
      const data = JSON.parse(textOf(r));
      expect(data.migration !== undefined, "回傳內容不像系統現況");
      return `migration ${data.migration}`;
    });

    await check("F2", "工具開關關著時擋下，訊息指向「MCP 與 AI」", async () => {
      const r = await client.callTool({ name: "post_voucher", arguments: { voucher_no: "V2608-001" } });
      expect(r.isError === true, "應被擋下");
      expect(textOf(r).includes("MCP 與 AI"), textOf(r));
      const audit = await lastAudit("post_voucher", admin.actor);
      expect(audit.outcome === "blocked", `outcome = ${audit.outcome}`);
      return textOf(r);
    });

    await check("F3", "順序：工具開關先於角色權限", async () => {
      // create_user_account is off for AI and staff has no permission for it
      // either. The message must be the switch, proving which gate ran first.
      const c = await connect(staff.secret);
      const r = await c.callTool({
        name: "create_user_account",
        arguments: { email: "x@example.com", role_code: "staff" },
      });
      await c.close();
      expect(r.isError === true, "應被擋下");
      expect(textOf(r).includes("目前為關閉狀態"), textOf(r));
      return textOf(r);
    });

    await check("F4", "角色沒有的權限一律拒絕", async () => {
      const r = await client.callTool({ name: "get_dashboard", arguments: {} });
      expect(r.isError === true, "系統管理員不該讀得到財務總覽");
      expect(textOf(r).includes("未獲授權"), textOf(r));
      const audit = await lastAudit("get_dashboard", admin.actor);
      expect(audit.outcome === "blocked", `outcome = ${audit.outcome}`);
      return textOf(r);
    });

    await check("F5", "職員叫不動高風險的系統工具", async () => {
      const c = await connect(staff.secret);
      const r = await c.callTool({ name: "revoke_api_token", arguments: { id: 999999 } });
      await c.close();
      expect(r.isError === true, "應被拒絕");
      expect(textOf(r).includes("未獲授權"), textOf(r));
      return textOf(r);
    });

    await check("F6", "錯誤訊息不出現操作代號與角色代號", async () => {
      const r = await client.callTool({ name: "get_dashboard", arguments: {} });
      const text = textOf(r);
      expect(!text.includes("get_dashboard"), `訊息含操作代號：${text}`);
      expect(!/\b(admin|boss|staff|acct|cashier|manager)\b/.test(text), `訊息含角色代號：${text}`);
      return text;
    });

    await check("F7", "不存在的工具名回錯誤且不寫稽核", async () => {
      const before = await scalar("SELECT coalesce(max(id),0) FROM ai_audit_log");
      const r = await client.callTool({ name: "no_such_tool", arguments: {} });
      expect(r.isError === true, JSON.stringify(r));
      expect(textOf(r).includes("查無此操作"), textOf(r));
      const after = await scalar("SELECT coalesce(max(id),0) FROM ai_audit_log");
      expect(before === after, "未知工具不應留下稽核");
      return textOf(r);
    });

    await check("F8", "缺少工具名稱回 -32600", async () => {
      const r = await rpc(admin.secret, { jsonrpc: "2.0", id: 1, method: "tools/call", params: { arguments: {} } });
      expect(r.json?.error?.code === -32600, JSON.stringify(r.json));
      return r.json.error.message;
    });

    await check("F9", "輸入型別錯誤回 isError 而不是協定層錯誤", async () => {
      const r = await client.callTool({ name: "list_ai_audit", arguments: { limit: "很多" } as any });
      expect(r.isError === true, JSON.stringify(r));
      return textOf(r);
    });

    await check("F10", "沒有 arguments 時視為全部使用預設值", async () => {
      const r = await client.callTool({ name: "list_ai_audit" });
      expect(r.isError === false, textOf(r));
      return "";
    });

    await check("F11", "未連結員工的 Token 叫不動自助工具", async () => {
      const c = await connect(staff.secret);
      const r = await c.callTool({ name: "get_my_home", arguments: {} });
      await c.close();
      expect(r.isError === true, "應被拒絕");
      expect(textOf(r).includes("尚未連結員工編號"), textOf(r));
      return textOf(r);
    });

    console.log("\nG 稽核");
    await check("G1", "成功的呼叫寫下 channel = mcp 的稽核", async () => {
      await client.callTool({ name: "get_company_settings", arguments: {} });
      const audit = await lastAudit("get_company_settings", admin.actor);
      expect(audit.outcome === "ok", `outcome = ${audit.outcome}`);
      expect(audit.channel === "mcp", `channel = ${audit.channel}`);
      expect(audit.actor === admin.actor, `actor = ${audit.actor}`);
      expect(audit.risk === "read_only", `risk = ${audit.risk}`);
      return audit.summary;
    });

    await check("G2", "_meta.prompt 記進稽核", async () => {
      const prompt = "測試用提問：現在的公司設定是什麼";
      await rpc(admin.secret, {
        jsonrpc: "2.0", id: 1, method: "tools/call",
        params: { name: "list_roles", arguments: {}, _meta: { prompt } },
      });
      const audit = await lastAudit("list_roles", admin.actor);
      expect(audit.prompt === prompt, `prompt = ${audit.prompt}`);
      return "";
    });

    await check("G3", "超過 512 位元組的字串不進稽核", async () => {
      await client.callTool({ name: "search_vouchers", arguments: { q: "A".repeat(600) } });
      const audit = await lastAudit("search_vouchers", admin.actor);
      expect(!audit.params.includes("AAAAAAAAAA"), "長字串原文寫進了稽核");
      expect(audit.params.includes("未存入稽核紀錄"), audit.params.slice(0, 200));
      return "";
    });

    await check("G4", "同一組 Token 的每一次呼叫都記在同一個身分下", async () => {
      const count = await scalar(
        `SELECT count(*) FROM ai_audit_log WHERE actor = $$${admin.actor}$$ AND channel = $$mcp$$`,
      );
      expect(Number(count) > 0, "沒有任何稽核");
      const others = await scalar(
        `SELECT count(*) FROM ai_audit_log WHERE actor = $$${admin.actor}$$ AND actor_role <> $$admin$$`,
      );
      expect(others === "0", `出現了 ${others} 筆不同角色的紀錄`);
      return `${count} 筆`;
    });
  } finally {
    try {
      await client?.close();
    } catch {}
    // Teardown goes through the same operation a person would use.
    for (const token of tokens) {
      try {
        await callOp("revoke_api_token", { id: token.id });
      } catch {}
    }
  }

  await check("Z1", "測試用的 Token 全部撤銷", async () => {
    const live = await scalar(
      `SELECT count(*) FROM api_token WHERE name LIKE $$${TOKEN_PREFIX}%$$ AND revoked_at IS NULL`,
    );
    expect(live === "0", `還有 ${live} 組未撤銷`);
    const r = await rpc(admin.secret, { jsonrpc: "2.0", id: 1, method: "ping" });
    expect(r.status === 401, `撤銷後仍可用，HTTP ${r.status}`);
    return "撤銷後立刻失效";
  });

  const auditEnd = Number(await scalar("SELECT coalesce(max(id),0) FROM ai_audit_log"));
  const failed = cases.filter((c) => !c.ok);
  console.log(`\n${cases.length - failed.length} / ${cases.length} 通過`);
  console.log(`本次新增稽核紀錄 ${auditEnd - auditStart} 筆（id ${auditStart + 1} 起）`);
  console.log(`測試 Token 已由 revoke_api_token 撤銷，資料列保留供稽核`);
  if (failed.length) {
    console.log("\n未通過：");
    for (const c of failed) console.log(`  ${c.id}  ${c.name}\n      ${c.note}`);
    process.exit(1);
  }
}

await main();
