#!/usr/bin/env bun
// The response schemas at the API boundary, checked without a browser.
//
// callOp validates against these before a screen sees the data, so a backend
// field that is renamed, dropped or changed type stops at the boundary with a
// message instead of surfacing as undefined somewhere in a table.
//
//   bun scripts/schema-test.ts

import { employeeList, healthz } from "../web/src/api/client";

let failed = 0;

function accepts(label: string, schema: { safeParse: (v: unknown) => { success: boolean } }, value: unknown): void {
  const ok = schema.safeParse(value).success;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}`);
  if (!ok) failed += 1;
}

function rejects(label: string, schema: { safeParse: (v: unknown) => { success: boolean } }, value: unknown): void {
  const ok = !schema.safeParse(value).success;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}`);
  if (!ok) failed += 1;
}

const row = {
  emp_no: "E001",
  name: "王小明",
  dept_code: null,
  dept_label: null,
  title: "工程師",
  employment_type: "full_time",
  employment_label: "正職",
  pay_basis: "monthly",
  pay_basis_label: "月薪",
  insured_as: "employee",
  insured_as_label: "受僱者",
  birthday: "1994-05-02",
  hired_on: "2024-03-01",
  left_on: null,
  status: "active",
  status_label: "在職",
  has_login: true,
  service_months: 29,
  service_label: "2 年 5 個月",
  annual_leave_days: 10,
  annual_leave_taken: "3",
  annual_leave_left: "7",
  id_no: "A123456789",
  base_salary: "80000",
  labor_insured_salary: "45800",
  health_insured_salary: "45800",
  pension_wage: "45800",
  bank_accounts: [],
  dependents: [
    {
      name: "王小寶",
      relation: "child",
      relation_label: "子女",
      id_no: "",
      birthday: "2026-03-01",
      health_enrolled_from: "2026-03-01",
      health_enrolled_until: null,
    },
  ],
  note: "",
};

const list = {
  employees: [row],
  total: 1,
  active: 1,
  leaving: 0,
  by_dept: [{ label: "未指定", count: 1 }],
  departments: [{ code: "ADM", label: "行政部", active: true }],
  relations: [{ code: "child", label: "子女" }],
  avg_service_months: 29,
  avg_service_label: "2 年 5 個月",
  annual_leave_ruleset: {
    domain: "annual_leave",
    version: "2026.1",
    effective_from: "2026-01-01",
    effective_to: null,
    source_note: "勞動基準法第 38 條",
  },
  insured_suggestion: {
    floor: "29500",
    labor_ceiling: "45800",
    note: "",
  },
  salary_note: "",
};

accepts("完整的名冊回應", employeeList, list);

// A field the backend later adds must not break a screen that predates it.
accepts("多出來的欄位不影響解析", employeeList, {
  ...list,
  employees: [{ ...row, probation_ends_on: "2024-06-01" }],
});

// The two ways a backend change goes wrong quietly.
rejects("少一個欄位", employeeList, {
  ...list,
  employees: [{ ...row, annual_leave_days: undefined }],
});

// Money is a decimal string on the wire. A number here would already have
// been through a float.
rejects("金額變成數字", employeeList, {
  ...list,
  employees: [{ ...row, base_salary: 80000 }],
});

// What an unauthenticated caller sees: liveness without the deployment's
// internals.
accepts("未登入時的 /healthz", healthz, {
  status: "ok",
  db: { ok: true },
  rustfs: { ok: true },
});

console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
