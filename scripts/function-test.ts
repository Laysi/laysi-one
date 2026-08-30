#!/usr/bin/env bun
// End-to-end functional test: does the system compute the right numbers?
//
// Self contained. It drops and recreates a throwaway database, starts its own
// laysi-server against it, drives the REST operations a person drives from the
// screens, and checks the results — VAT splits, voucher balance, the report
// invariants, payroll. Nothing touches the working database.
//
//   bun scripts/function-test.ts
//
// The MCP surface is covered by scripts/mcp-test.ts and
// scripts/mcp-role-matrix.ts; both interfaces dispatch through the same
// registry, so the arithmetic is tested once, here.

import { mismatch } from "./response-schemas";

const PG_CONTAINER = process.env.PG_CONTAINER ?? "laysi-one_postgres_1";
const PG_USER = process.env.POSTGRES_USER ?? "laysi";
// The compose stack generates the password into the secrets volume, so the
// running container is the source of truth and is asked first. POSTGRES_PASSWORD
// is only the fallback: bun loads a stray .env from before that change, and a
// stale value there fails as an authentication error fifteen seconds later,
// reported as "the server did not start".
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
const TEST_DB = process.env.TEST_DB ?? "laysi_function_test";
const PORT = Number(process.env.TEST_PORT ?? "8091");
const BASE = `http://127.0.0.1:${PORT}`;
const SERVER_BIN = new URL("../backend/target/debug/laysi-server", import.meta.url).pathname;
// The books run on +08:00, so a UTC date would file today's vouchers under
// tomorrow and every report would close before them.
const TODAY =
  process.env.TEST_TODAY ?? new Date(Date.now() + 8 * 3600 * 1000).toISOString().slice(0, 10);
const ADMIN_PASSWORD = "Function-Test-2026";
const BOSS_PASSWORD = "Function-Boss-2026";

async function psql(db: string, sql: string): Promise<string> {
  const proc = Bun.spawn(
    ["podman", "exec", PG_CONTAINER, "psql", "-U", PG_USER, "-d", db,
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

let cookie = "";

type OpResult = { ok: boolean; data?: any; error?: { code: string; message: string } };

async function op(name: string, input: unknown = {}): Promise<OpResult> {
  const res = await fetch(`${BASE}/v1/ops/${name}`, {
    method: "POST",
    headers: { "content-type": "application/json", cookie },
    body: JSON.stringify({ input }),
  });
  return (await res.json()) as OpResult;
}

async function must(name: string, input: unknown = {}): Promise<any> {
  const r = await op(name, input);
  if (!r.ok) throw new Error(`${name}：${r.error?.message}`);
  // The screens parse this response with a zod schema before showing it, so a
  // shape they got wrong stops here rather than on the screen. Write
  // operations are only reachable with real arguments, which is what this
  // file spends its length building.
  const wrong = mismatch(name, r.data);
  if (wrong) throw new Error(`${name} 的回應與前端 schema 不符 — ${wrong}`);
  return r.data;
}

async function login(username: string, password: string) {
  const res = await fetch(`${BASE}/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  const body = await res.json();
  if (!body.ok) throw new Error(`登入失敗：${JSON.stringify(body)}`);
  cookie = (res.headers.getSetCookie?.() ?? []).map((c) => c.split(";")[0]).join("; ");
  return body.user;
}

// Money arrives as decimal strings and must never become a float on the way
// to a comparison.
function money(value: unknown): number {
  if (typeof value !== "string") throw new Error(`金額應為字串，收到 ${typeof value}：${JSON.stringify(value)}`);
  return Math.round(Number(value) * 100);
}

function sum(values: unknown[]): number {
  return values.reduce<number>((total, v) => total + money(v), 0);
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

function expect(condition: unknown, message: string) {
  if (!condition) throw new Error(message);
}

function expectEq(got: unknown, want: unknown, what: string) {
  if (got !== want) throw new Error(`${what}：得到 ${JSON.stringify(got)}，預期 ${JSON.stringify(want)}`);
}

async function startServer(): Promise<ReturnType<typeof Bun.spawn>> {
  const proc = Bun.spawn([SERVER_BIN], {
    env: {
      ...process.env,
      DATABASE_URL: `postgres://${PG_USER}:${PG_PASSWORD}@localhost:5432/${TEST_DB}`,
      BIND_ADDR: `127.0.0.1:${PORT}`,
      RUSTFS_ENDPOINT: process.env.RUSTFS_ENDPOINT ?? "http://localhost:9000",
      RUSTFS_ACCESS_KEY: process.env.RUSTFS_ACCESS_KEY ?? "laysi",
      RUSTFS_SECRET_KEY: process.env.RUSTFS_SECRET_KEY ?? "devsecret-change-me",
      RUSTFS_BUCKET: `${TEST_DB}-documents`,
      SETUP_ALLOW_WEB: "true",
      SESSION_COOKIE_SECURE: "false",
      RUST_LOG: "warn",
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  for (let i = 0; i < 60; i++) {
    try {
      const res = await fetch(`${BASE}/healthz`);
      if (res.ok) return proc;
    } catch {}
    await Bun.sleep(250);
  }
  proc.kill();
  throw new Error("測試伺服器沒有在 15 秒內起來");
}

async function main() {
  await psql("postgres", `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '${TEST_DB}'`);
  await psql("postgres", `DROP DATABASE IF EXISTS ${TEST_DB}`);
  await psql(
    "postgres",
    `CREATE DATABASE ${TEST_DB} WITH TEMPLATE template0 ENCODING 'UTF8'
       LOCALE_PROVIDER icu ICU_LOCALE 'zh-TW' LOCALE 'C.UTF-8'`,
  );
  const server = await startServer();
  console.log(`功能測試 · ${BASE} · 資料庫 ${TEST_DB} · 帳務日 ${TODAY}\n`);

  try {
    console.log("S 首次設定");
    await check("S1", "網頁初始化建出的是安裝本身的帳號", async () => {
      const res = await fetch(`${BASE}/v1/auth/bootstrap`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ password: ADMIN_PASSWORD }),
      });
      const body = await res.json();
      expect(body.ok, JSON.stringify(body));
      expectEq(body.user.username, "admin", "登入名稱");
      expectEq(body.user.role, "admin", "角色");
      expectEq(body.user.email, "", "電子郵件應留空");
      expectEq(body.user.must_change_password, false, "密碼由本人設定，不該要求變更");
      cookie = (res.headers.getSetCookie?.() ?? []).map((c) => c.split(";")[0]).join("; ");
      return `${body.user.display_name}／${body.user.username}`;
    });

    await check("S2", "初始化只會成功一次", async () => {
      const res = await fetch(`${BASE}/v1/auth/bootstrap`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ password: "another-one" }),
      });
      expect(res.status !== 200, `HTTP ${res.status}`);
      const count = await psql(TEST_DB, "SELECT count(*) FROM user_account");
      expectEq(count, "1", "帳號數");
      return `HTTP ${res.status}`;
    });

    await check("S3", "公司設定寫得進去", async () => {
      const data = await must("update_company_setting", { code: "company_name", value: "功能測試股份有限公司" });
      expectEq(data.setting.value, "功能測試股份有限公司", "公司名稱");
      // Work hours decide whether punch-derived overtime is checked at all, and
      // the responses that answer it change shape once they are set. Leaving
      // them empty keeps that branch out of the live schema check, which is how
      // get_hr_compliance shipped a null where the screen expects a string.
      await must("update_company_setting", { code: "work_start", value: "09:00" });
      await must("update_company_setting", { code: "work_end", value: "18:00" });
      const meta = await fetch(`${BASE}/v1/meta`, { headers: { cookie } }).then((r) => r.json());
      expectEq(meta.setup_required, false, "填了公司名之後不該再要求首次設定");
      return meta.period;
    });

    await check("S4", "建立資金帳戶", async () => {
      const data = await must("create_bank_account", {
        name: "台灣銀行活存", account_no: "123-456-789", gl_code: "1102",
      });
      expectEq(data.bank_account.gl_code, "1102", "總帳科目");
      return `${data.bank_account.name}（id ${data.bank_account.id}）`;
    });

    let bossEmployeeNo = "";
    await check("S5", "建立負責人帳號時一定連著一筆員工主檔", async () => {
      const data = await must("create_user_account", {
        email: "boss@test.local", display_name: "測試負責人", role_code: "boss",
      });
      expect(typeof data.temporary_password === "string" && data.temporary_password.length > 0, "沒有臨時密碼");
      expectEq(data.must_change_password, true, "代建的帳號應強制首次變更密碼");
      expect(data.user.employee_no, "沒有連結員工主檔");
      bossEmployeeNo = data.user.employee_no;
      await login("boss@test.local", data.temporary_password);
      const res = await fetch(`${BASE}/v1/auth/change-password`, {
        method: "POST",
        headers: { "content-type": "application/json", cookie },
        body: JSON.stringify({ current_password: data.temporary_password, new_password: BOSS_PASSWORD }),
      });
      expect(res.status === 200, `變更密碼 HTTP ${res.status}`);
      const set = (res.headers.getSetCookie?.() ?? []).map((c) => c.split(";")[0]).join("; ");
      if (set) cookie = set;
      return `員工編號 ${bossEmployeeNo}`;
    });

    console.log("\nT 營業稅拆分");
    await check("T1", "只給含稅金額，未稅與稅額由稅率算出", async () => {
      const data = await must("create_ar_invoice", {
        invoice_no: "AR-001", customer: "甲客戶", issued_on: TODAY, due_on: "2026-09-30",
        amount: "10500", revenue_account_code: "4111",
      });
      expectEq(money(data.net_amount), 1000000, "未稅金額");
      expectEq(money(data.tax_amount), 50000, "稅額");
      expectEq(money(data.invoice.amount), 1050000, "含稅金額");
      return `10,500 = 10,000 + 500（${data.vat_version}）`;
    });

    await check("T2", "只給未稅金額，含稅與稅額由稅率算出", async () => {
      const data = await must("create_ar_invoice", {
        invoice_no: "AR-002", customer: "乙客戶", issued_on: TODAY, due_on: "2026-09-30",
        net_amount: "20000", revenue_account_code: "4111",
      });
      expectEq(money(data.invoice.amount), 2100000, "含稅金額");
      expectEq(money(data.tax_amount), 100000, "稅額");
      return "20,000 → 21,000";
    });

    await check("T3", "零稅率明確寫 0，不是靠旗標", async () => {
      const data = await must("create_ar_invoice", {
        invoice_no: "AR-003", customer: "丙客戶（外銷）", issued_on: TODAY, due_on: "2026-09-30",
        amount: "30000", tax_amount: "0", revenue_account_code: "4111",
      });
      expectEq(money(data.net_amount), 3000000, "未稅金額應等於含稅金額");
      expectEq(money(data.tax_amount), 0, "稅額");
      return "30,000 全額未稅";
    });

    await check("T4", "應收單的傳票把稅額拆到 2211", async () => {
      const data = await must("create_ar_invoice", {
        invoice_no: "AR-004", customer: "丁客戶", issued_on: TODAY, due_on: "2026-09-30",
        amount: "10500", revenue_account_code: "4111",
      });
      const lines = data.voucher?.lines ?? [];
      const receivable = lines.find((l: any) => l.account_code === "1131");
      const revenue = lines.find((l: any) => l.account_code === "4111");
      const tax = lines.find((l: any) => l.account_code === "2211");
      expect(receivable && revenue && tax, `分錄科目為 ${lines.map((l: any) => l.account_code).join("、")}`);
      expectEq(money(receivable.debit), 1050000, "借應收帳款");
      expectEq(money(revenue.credit), 1000000, "貸銷貨收入");
      expectEq(money(tax.credit), 50000, "貸代收營業稅");
      return `${data.invoice.voucher_no}：借 1131 10,500 / 貸 4111 10,000 · 2211 500`;
    });

    console.log("\nV 傳票不變量");
    await check("V1", "借貸不相等的傳票建不起來", async () => {
      const r = await op("create_voucher", {
        date: TODAY, memo: "不平的傳票",
        lines: [
          { account_code: "1102", debit: "100", credit: "0" },
          { account_code: "4111", debit: "0", credit: "90" },
        ],
      });
      expect(!r.ok, "竟然建起來了");
      return r.error!.message;
    });

    await check("V2", "只有一行的傳票建不起來", async () => {
      const r = await op("create_voucher", {
        date: TODAY, memo: "只有一行",
        lines: [{ account_code: "1102", debit: "100", credit: "0" }],
      });
      expect(!r.ok, "竟然建起來了");
      return r.error!.message;
    });

    await check("V3", "同一行不能同時有借有貸", async () => {
      const r = await op("create_voucher", {
        date: TODAY, memo: "一行兩邊",
        lines: [
          { account_code: "1102", debit: "100", credit: "100" },
          { account_code: "4111", debit: "0", credit: "0" },
        ],
      });
      expect(!r.ok, "竟然建起來了");
      return r.error!.message;
    });

    let manualVoucher = "";
    await check("V4", "手工傳票建立後是待過帳，編號帶年月", async () => {
      const data = await must("create_voucher", {
        date: TODAY, memo: "收現金",
        lines: [
          { account_code: "1101", debit: "5000", credit: "0" },
          { account_code: "4111", debit: "0", credit: "5000" },
        ],
      });
      manualVoucher = data.voucher.no;
      expectEq(data.voucher.status, "pending", "狀態");
      const yy = TODAY.slice(2, 4), mm = TODAY.slice(5, 7);
      expect(new RegExp(`^V${yy}${mm}-\\d{3}$`).test(manualVoucher), `編號格式 ${manualVoucher}`);
      expect(typeof data.voucher.amount === "string", "金額應為字串");
      return manualVoucher;
    });

    await check("V5", "過帳把狀態改成已過帳", async () => {
      const data = await must("post_voucher", { no: manualVoucher });
      expectEq(data.voucher.status, "posted", "狀態");
      return manualVoucher;
    });

    await check("V6", "已過帳的傳票不能再過帳一次", async () => {
      const r = await op("post_voucher", { no: manualVoucher });
      expect(!r.ok, "竟然可以重複過帳");
      return r.error!.message;
    });

    console.log("\nR 報表不變量");
    // The invoices posted above make the reports non-trivial.
    const pending = await must("search_vouchers", { status: "pending" });
    for (const v of pending.vouchers ?? pending.rows ?? []) {
      await must("post_voucher", { no: v.no });
    }

    let statementProfit = 0;
    await check("R1", "試算表借貸合計相等", async () => {
      const tb = await must("get_trial_balance", { as_of: TODAY });
      expectEq(tb.balanced, true, "balanced 旗標");
      expectEq(money(tb.total_debit), money(tb.total_credit), "借貸合計");
      expect(money(tb.total_debit) > 0, "試算表是空的，後面的報表檢查沒有意義");
      return `借貸各 ${tb.total_debit}`;
    });

    await check("R2", "資產等於負債加權益", async () => {
      const bs = await must("get_balance_sheet", { as_of: TODAY });
      expect(money(bs.assets) > 0, "資產負債表是空的，這個檢查沒有意義");
      expectEq(money(bs.assets), money(bs.liabilities) + money(bs.equity), "資產 vs 負債＋權益");
      expectEq(money(bs.difference), 0, "差額");
      expectEq(bs.balanced, true, "balanced 旗標");
      return `資產 ${bs.assets} = 負債 ${bs.liabilities} + 權益 ${bs.equity}`;
    });

    await check("R3", "兩張表的本期損益一致", async () => {
      const from = TODAY.slice(0, 8) + "01";
      const is = await must("get_income_statement", { from, to: TODAY });
      const bs = await must("get_balance_sheet", { as_of: TODAY });
      statementProfit = money(is.profit);
      expect(statementProfit !== 0, "本期損益為 0，這個檢查沒有意義");
      expectEq(statementProfit, money(bs.profit), "本期損益");
      expectEq(money(is.revenue) - money(is.cogs) - money(is.operating_expense), statementProfit, "收入減成本費用");
      return `${is.profit}`;
    });

    await check("R4", "現金流量表的期末現金等於現金科目餘額", async () => {
      const from = TODAY.slice(0, 8) + "01";
      const cf = await must("get_cash_flow", { from, to: TODAY });
      expect(money(cf.closing) !== 0, "期末現金為 0，這個檢查沒有意義");
      expectEq(money(cf.closing), money(cf.ledger_closing), "期末現金 vs 總帳現金餘額");
      expectEq(money(cf.opening) + money(cf.net_change), money(cf.closing), "期初＋淨變動 vs 期末");
      expectEq(cf.reconciled, true, "reconciled 旗標");
      // Cards plus the not-yet-attributed remainder must be the whole balance.
      expectEq(
        money(cf.cash_accounts_assigned) + money(cf.cash_accounts_unattributed),
        money(cf.ledger_closing),
        "資金帳戶卡片合計 vs 總帳現金餘額",
      );
      return `期末 ${cf.closing}`;
    });

    await check("R5", "401 的銷項稅額等於 2211 的期間變動", async () => {
      const vat = await must("get_vat_return", { as_of: TODAY });
      const output = money(vat.output_tax ?? vat.sales_tax ?? vat.output);
      expect(output > 0, "銷項稅額為 0，這個檢查沒有意義");
      const gl = await psql(
        TEST_DB,
        `SELECT coalesce(sum(l.credit - l.debit), 0)::text FROM voucher_line l
           JOIN voucher v ON v.id = l.voucher_id
          WHERE l.account_code = '2211' AND v.status = 'posted'`,
      );
      expectEq(output, Math.round(Number(gl) * 100), "銷項稅額 vs 2211 總帳");
      return `${vat.output_tax ?? vat.sales_tax ?? vat.output}`;
    });

    await check("R6", "報表說得出它漏掉了什麼", async () => {
      const is = await must("get_income_statement", { from: TODAY.slice(0, 8) + "01", to: TODAY });
      expect(is.unposted !== undefined, "損益表沒有回報未列入的傳票");
      return JSON.stringify(is.unposted).slice(0, 80);
    });

    console.log("\nH 人資與薪資");
    await check("H1", "底薪 0 表示尚未設定，人先建得起來", async () => {
      const data = await must("create_employee", {
        name: "測試員工", hired_on: "2026-01-01", employment_type: "full_time",
      });
      expectEq(money(data.employee.base_salary), 0, "底薪");
      return data.employee.emp_no;
    });

    await check("H2", "沒有底薪就算不出薪資，錯誤要說是哪裡沒填", async () => {
      const r = await op("run_payroll", { period: TODAY.slice(0, 7), pay_on: TODAY });
      expect(!r.ok, "竟然發得出薪資單");
      expect(r.error!.message.includes("底薪"), r.error!.message);
      return r.error!.message;
    });

    let insured = 0;
    await check("H3", "勞健保勞退由法規規則庫算，不是常數", async () => {
      const data = await must("compute_insurance", { insured_salary: "36300", as_of: TODAY });
      const labor = money(data.labor_employee ?? data.labor?.employee);
      const health = money(data.health_employee ?? data.health?.employee);
      const pension = money(data.pension_employer ?? data.pension?.employer);
      expect(labor > 0 && health > 0 && pension > 0, JSON.stringify(data).slice(0, 200));
      insured = labor + health;
      return `勞保自付 ${data.labor_employee ?? data.labor?.employee}、健保自付 ${data.health_employee ?? data.health?.employee}、勞退提繳 ${data.pension_employer ?? data.pension?.employer}`;
    });

    await check("H4", "沒有適用規則的日期回錯誤，不回退到預設值", async () => {
      const r = await op("compute_insurance", { insured_salary: "36300", as_of: "1990-01-01" });
      expect(!r.ok, "竟然算得出 1990 年的保費");
      return r.error!.message;
    });

    await check("H5", "設定底薪之後發得出薪資單", async () => {
      for (const [empNo, name] of [["E001", "測試負責人"], ["E002", "測試員工"]]) {
        await must("update_employee", {
          emp_no: empNo, name, hired_on: "2026-01-01",
          base_salary: "50000", labor_insured_salary: "45800",
          health_insured_salary: "45800", pension_wage: "45800",
        });
      }
      const data = await must("run_payroll", { period: TODAY.slice(0, 7), pay_on: TODAY });
      const run = data.run;
      expectEq(run.headcount, 2, "計薪人數");
      expectEq(money(run.gross_total), 10000000, "應發合計");
      expectEq(money(run.net_total), money(run.gross_total) - money(run.deduction_total), "實發 = 應發 − 代扣");
      const parts = data.deduction_breakdown;
      expect(money(parts.labor_employee) > 0 && money(parts.health_employee) > 0, JSON.stringify(parts));
      expectEq(
        money(parts.labor_employee) + money(parts.health_employee) + money(parts.income_tax),
        money(run.deduction_total),
        "代扣明細合計",
      );
      expectEq(run.voucher_status, "pending", "薪資傳票狀態");
      expectEq(data.employer_in_ledger, false, "公司負擔的保費還沒有接進總帳，回報值要照實");
      return `${run.headcount} 人，應發 ${run.gross_total}，代扣 ${run.deduction_total}，實發 ${run.net_total}`;
    });

    await check("H6", "薪資產生的傳票借貸相等，借薪資支出、貸代扣款與存款", async () => {
      const pendingNow = await must("search_vouchers", { status: "pending" });
      const v = (pendingNow.vouchers ?? []).find((x: any) =>
        (x.lines ?? []).some((l: any) => l.account_code === "6111"),
      );
      expect(v, "找不到薪資傳票");
      const lines = v.lines ?? [];
      const debit = sum(lines.map((l: any) => l.debit));
      const credit = sum(lines.map((l: any) => l.credit));
      expectEq(debit, credit, "借貸合計");
      expect(lines.some((l: any) => l.account_code === "2117"), "沒有貸代扣款");
      await must("post_voucher", { no: v.no });
      return `${v.no}：借貸各 ${v.amount}`;
    });

    await check("H7", "薪資同時寫下扣繳紀錄，所得代號是薪資所得", async () => {
      const rows = await psql(
        TEST_DB,
        "SELECT income_code, count(*)::text FROM withholding_record GROUP BY income_code ORDER BY income_code",
      );
      expect(rows.length > 0, "沒有任何扣繳紀錄");
      expect(rows.startsWith("50|"), `所得代號為 ${rows}`);
      return rows.replaceAll("\n", " / ");
    });

    await check("H8", "月中才到職的人，同一期補算得回來", async () => {
      const period = TODAY.slice(0, 7);
      const late = await must("create_employee", {
        name: "測試遲到職", hired_on: `${period}-15`, employment_type: "full_time",
      });
      await must("update_employee", {
        emp_no: late.employee.emp_no, name: "測試遲到職", hired_on: `${period}-15`,
        base_salary: "50000",
        labor_insured_salary: "45800", health_insured_salary: "45800", pension_wage: "45800",
      });
      // The first run of this period is already posted, so voiding it is
      // refused and a second run is the only way this person gets paid.
      const refused = await op("void_payroll_run", { period, reason: "測試" });
      expect(!refused.ok, "已過帳的期間竟然作廢得了");
      const data = await must("run_payroll", { period, pay_on: TODAY });
      expectEq(data.run.headcount, 1, "補算人數");
      expect(
        (data.skipped ?? []).filter((s: any) => s.reason === "本期已計薪").length === 2,
        "已計薪的兩個人應被跳過",
      );
      const summary = await must("get_payroll_summary", { period });
      expectEq(summary.run.run_count, 2, "本期計算次數");
      expectEq(summary.run.headcount, 3, "本期累計人數");
      expectEq(summary.period_runs.length, 2, "本期傳票數");
      expect(
        summary.periods.filter((p: string) => p === period).length === 1,
        "期別選單重複列出同一期",
      );
      return `補算 ${data.run.headcount} 人 · ${data.run.voucher_no} · 本期共 ${summary.run.headcount} 人`;
    });

    console.log("\nA 固定資產");
    let assetVoucher = "";
    await check("A1", "取得成本填未稅，營業稅另拆到進項稅額", async () => {
      const list = await must("list_fixed_assets", {});
      const category = list.categories[0].code;
      const data = await must("create_fixed_asset", {
        name: "測試設備", category, cost: "100000", acquired_on: TODAY,
        gl_code: "1601", accum_gl_code: "1602", paid_from_account_code: "1102",
      });
      expectEq(money(data.asset.cost), 10000000, "登記成本應為未稅");
      expectEq(money(data.tax_amount), 500000, "營業稅");
      // A registered asset with no voucher would claim a cost the ledger has
      // never seen.
      expect(data.asset.origin_voucher_no, "資產沒有指向任何傳票");
      assetVoucher = data.asset.origin_voucher_no;
      const found = await must("search_vouchers", { q: assetVoucher });
      const lines = (found.vouchers ?? []).find((v: any) => v.no === assetVoucher)?.lines ?? [];
      const asset = lines.find((l: any) => l.account_code === "1601");
      const inputTax = lines.find((l: any) => l.account_code === "1284");
      const paid = lines.find((l: any) => l.account_code === "1102");
      expect(asset && inputTax && paid, `分錄科目為 ${lines.map((l: any) => l.account_code).join("、")}`);
      expectEq(money(asset.debit), 10000000, "借資產科目");
      expectEq(money(inputTax.debit), 500000, "借進項稅額");
      expectEq(money(paid.credit), 10500000, "貸付款科目");
      return `${assetVoucher}：借 1601 100,000 · 1284 5,000 / 貸 1102 105,000`;
    });

    await check("A2", "登記簿的成本合計與總帳對得上", async () => {
      await must("post_voucher", { no: assetVoucher });
      const list = await must("list_fixed_assets", {});
      expectEq(money(list.cost_total), money(list.gl_cost), "登記簿成本 vs 總帳");
      expectEq(money(list.cost_difference), 0, "差額");
      expectEq(list.reconciled, true, "reconciled 旗標");
      return `成本合計 ${list.cost_total}`;
    });

    console.log("\nN 公告與規章");
    let policyId = 0;
    await check("N1", "公告發布後員工看得到，記為第 1 版", async () => {
      const data = await must("create_notice", {
        kind: "announcement", title: "中秋節放假一日", body: "9/25 全日休假。", effective_on: TODAY,
      });
      expectEq(data.notice.version_no, 1, "版次");
      expectEq(data.notice.status_label, "公告中", "狀態");
      const mine = await must("list_my_notices");
      expect(mine.notices.some((n: any) => n.title === "中秋節放假一日"), "員工端看不到這則公告");
      return `第 ${data.notice.version_no} 版 · ${data.notice.status_label}`;
    });

    await check("N2", "規章發布後在員工端讀得到全文，且標為未讀", async () => {
      const data = await must("create_notice", {
        kind: "policy", title: "出勤管理辦法", body: "上班 09:00，下班 18:00。", effective_on: TODAY,
      });
      policyId = data.notice.id;
      const mine = await must("list_my_notices");
      const row = mine.notices.find((n: any) => n.id === policyId);
      expectEq(row.body, "上班 09:00，下班 18:00。", "內文");
      expectEq(row.versions, 1, "版本數");
      expectEq(row.unread, true, "沒看過的應標為未讀");
      return `${row.kind_label}第 ${row.version_no} 版`;
    });

    await check("N3", "改動內容會發布新的一版，舊版仍讀得到原文", async () => {
      const data = await must("update_notice", { id: policyId, body: "上班 09:30，下班 18:30。" });
      expectEq(data.revised, true, "應判定為改版");
      expectEq(data.notice.version_no, 2, "版次");
      const history = await must("list_notices", { id: policyId });
      expectEq(history.versions.length, 2, "版本數");
      expectEq(history.versions[0].version_no, 2, "第一列應為最新版");
      expectEq(history.versions[1].body, "上班 09:00，下班 18:00。", "第 1 版的原文");
      return `共 ${history.versions.length} 版，第 1 版原文仍在`;
    });

    await check("N4", "只改廢止日不算改版", async () => {
      const later = new Date(Date.parse(`${TODAY}T00:00:00Z`) + 30 * 86400000).toISOString().slice(0, 10);
      const data = await must("update_notice", { id: policyId, expires_on: later });
      expectEq(data.revised, false, "設定廢止日不該進版");
      expectEq(data.notice.version_no, 2, "版次應不變");
      expectEq(data.notice.expires_on, later, "廢止日");
      return `仍為第 ${data.notice.version_no} 版`;
    });

    await check("N5", "員工讀到的修訂紀錄與人資是同一份", async () => {
      const mine = await must("list_my_notices", { id: policyId });
      const hr = await must("list_notices", { id: policyId });
      expectEq(JSON.stringify(mine.versions), JSON.stringify(hr.versions), "兩邊的版本清單");
      expect(mine.versions.every((v: any) => v.published_by), "每一版都要記得住是誰發布的");
      return `${mine.versions.length} 版，兩邊相同`;
    });

    await check("N6", "標為已看過之後未讀歸零，之後的修訂又是未讀", async () => {
      const before = await must("list_my_notices");
      expect(before.unread_count > 0, "標記之前應該有未讀");
      await must("mark_my_notices_read");
      const after = await must("list_my_notices");
      expectEq(after.unread_count, 0, "標記之後的未讀數");
      expect(after.notices.every((n: any) => n.unread === false), "還有標成未讀的");
      await must("update_notice", { id: policyId, body: "改過的內容，應重新變成未讀。" });
      const again = await must("list_my_notices");
      expectEq(again.unread_count, 1, "修訂之後的未讀數");
      expectEq(again.notices.find((n: any) => n.id === policyId).unread, true, "被修訂的那一則");
      return `${before.unread_count} → 0 → 1`;
    });

    await check("N7", "廢止之後員工端看不到，人資仍查得到全部版本", async () => {
      const yesterday = new Date(Date.parse(`${TODAY}T00:00:00Z`) - 86400000).toISOString().slice(0, 10);
      await must("update_notice", { id: policyId, effective_on: yesterday, expires_on: yesterday });
      const mine = await must("list_my_notices");
      expect(!mine.notices.some((n: any) => n.id === policyId), "已廢止的規章還留在員工端");
      const visible = await must("list_notices");
      expect(!visible.notices.some((n: any) => n.id === policyId), "預設清單不該列出已廢止的");
      const all = await must("list_notices", { include_ended: true });
      const row = all.notices.find((n: any) => n.id === policyId);
      expectEq(row.status_label, "已廢止", "狀態");
      const history = await must("list_notices", { id: policyId });
      expectEq(history.versions.length, 4, "廢止不刪任何版本");
      return `${row.status_label}，仍存 ${history.versions.length} 版`;
    });

    console.log("\nZ 收尾");
    await check("Z1", "所有寫入之後試算表仍然平衡", async () => {
      const tb = await must("get_trial_balance", { as_of: TODAY });
      expectEq(tb.balanced, true, "balanced 旗標");
      expectEq(money(tb.total_debit), money(tb.total_credit), "借貸合計");
      const bs = await must("get_balance_sheet", { as_of: TODAY });
      expectEq(money(bs.assets), money(bs.liabilities) + money(bs.equity), "資產 vs 負債＋權益");
      return `借貸各 ${tb.total_debit}`;
    });
  } finally {
    server.kill();
    await server.exited;
  }

  const failed = cases.filter((c) => !c.ok);
  console.log(`\n${cases.length - failed.length} / ${cases.length} 通過`);
  if (failed.length) {
    console.log("\n未通過：");
    for (const c of failed) console.log(`  ${c.id}  ${c.name}\n      ${c.note}`);
    process.exit(1);
  }
}

await main();
