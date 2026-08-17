import type { OpName } from './generated';

export interface ApiError {
  code: string;
  message: string;
}

export class OpFailed extends Error {
  constructor(public readonly error: ApiError) {
    super(error.message);
    this.name = 'OpFailed';
  }
}

type UnauthorizedHandler = () => void;

let onUnauthorized: UnauthorizedHandler | null = null;

export function setUnauthorizedHandler(handler: UnauthorizedHandler | null): void {
  onUnauthorized = handler;
}

export const SESSION_EXPIRED = 'session_expired';

function sessionExpired(): OpFailed {
  onUnauthorized?.();
  return new OpFailed({
    code: SESSION_EXPIRED,
    message: '登入狀態已經失效，請重新登入。',
  });
}

function headers(): HeadersInit {
  return { 'content-type': 'application/json' };
}

export async function callOp<T>(name: OpName, input?: unknown, prompt?: string): Promise<T> {
  const res = await fetch(`/v1/ops/${name}`, {
    method: 'POST',
    headers: headers(),
    body: JSON.stringify({ input: input ?? {}, prompt: prompt ?? null }),
  });
  if (res.status === 401) throw sessionExpired();
  const body: unknown = await res.json().catch(() => null);

  if (body && typeof body === 'object' && 'ok' in body) {
    const envelope = body as { ok: boolean; data?: T; error?: ApiError };
    if (envelope.ok && envelope.data !== undefined) return envelope.data;
    if (envelope.error) throw new OpFailed(envelope.error);
  }
  throw new OpFailed({ code: 'network', message: `伺服器回應異常（HTTP ${res.status}）` });
}

export const MAX_UPLOAD_BYTES = 25 * 1024 * 1024;

export function readFileBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`${file.name} 讀取失敗`));
    reader.onload = () => {
      const text = String(reader.result);
      const comma = text.indexOf(',');
      resolve(comma >= 0 ? text.slice(comma + 1) : text);
    };
    reader.readAsDataURL(file);
  });
}

export function fileSizeLabel(bytes: number): string {
  const mb = bytes / (1024 * 1024);
  return mb >= 1 ? `${mb.toFixed(1)} MB` : `${Math.ceil(bytes / 1024)} KB`;
}

export interface Meta {
  company: string;
  tax_id: string;
  setup_required: boolean;
  period: string;
  period_state: string;
  actor: string;
  actor_role: string;
  employee_id: number | null;
  oauth: { enabled: boolean; provider: string | null };
  version: string;
  licence: string;
  brand_build: string;
}

export async function getMeta(): Promise<Meta> {
  const res = await fetch('/v1/meta', { headers: headers() });
  if (res.status === 401) throw sessionExpired();
  if (!res.ok) throw new Error(`讀不到系統資訊（HTTP ${res.status}）`);
  return (await res.json()) as Meta;
}

export interface Stat {
  kicker: string;
  value: string;
  note: string;
}
export interface Todo {
  count: string;
  label: string;
  due: string;
  screen: string;
}
export interface CashBar {
  month: string;
  value: string;
  label: string;
}
export interface AiRecent {
  what: string;
  time: string;
  who: string;
  outcome: string;
}
export interface Dashboard {
  stats: Stat[];
  todos: Todo[];
  cash_bars: CashBar[];
  ai_recent: AiRecent[];
}

export interface VoucherLine {
  account_code: string;
  account_name: string;
  debit: string;
  credit: string;
  memo: string | null;
}
export interface Voucher {
  no: string;
  date: string;
  memo: string;
  status: 'draft' | 'pending' | 'posted' | 'void';
  source: string;
  created_by: string;
  amount: string;
  accounts_label: string;
  lines: VoucherLine[];
}

export interface CashAccount {
  name: string;
  account_no: string;
  balance: string;
}

export interface ArInvoice {
  invoice_no: string;
  customer: string;
  issued_on: string;
  due_on: string;
  amount: string;
  settled: string;
  in_transit: string;
  outstanding: string;
  state: 'open' | 'receiving' | 'settled';
  days_overdue: number;
  memo: string;
  origin_voucher_no: string | null;
  recognised: boolean;
  receipt_voucher_no: string | null;
}

export interface AgingBucket {
  label: string;
  value: string;
  count: number;
}

export interface ArAging {
  as_of: string;
  buckets: AgingBucket[];
  total: string;
  gl_balance: string;
  difference: string;
  reconciled: boolean;
  not_yet_posted: string;
  overdue_count: number;
}

export interface ApBill {
  bill_no: string;
  vendor: string;
  memo: string;
  billed_on: string;
  due_on: string;
  amount: string;
  settled: string;
  in_transit: string;
  outstanding: string;
  state: 'open' | 'paying' | 'settled';
  days_overdue: number;
  origin_voucher_no: string | null;
  recognised: boolean;
  batch_no: string | null;
}

export interface PayablesResult {
  bills: ApBill[];
  listed_total: string;
  total: string;
  gl_balance: string;
  difference: string;
  reconciled: boolean;
  not_yet_posted: string;
  overdue_count: number;
}

export interface BankTxn {
  id: number;
  bank_account_id: number;
  bank_name: string;
  account_no: string;
  txn_date: string;
  memo: string;
  amount: string;
  kind: string;
  matched_voucher_no: string | null;
  matched_at: string | null;
  matched_by: string | null;
}

export interface MatchCandidate {
  voucher_no: string;
  date: string;
  memo: string;
  status: string;
  net_effect: string;
  amount_gap: string;
  day_gap: number;
  exact: boolean;
  reason: string;
}

export interface EInvoice {
  invoice_no: string;
  track: string;
  serial_no: number;
  buyer_tax_id: string;
  buyer: string;
  item: string;
  issued_on: string;
  net_amount: string;
  tax_amount: string;
  total_amount: string;
  vat_version: string;
  upload_status: 'queued' | 'uploaded' | 'failed' | 'void';
  status_label: string;
  upload_attempts: number;
  uploaded_at: string | null;
  last_error: string | null;
  voucher_no: string | null;
  voucher_status: string | null;
  ar_invoice_no: string | null;
  recognised: boolean;
}

export interface EInvoiceTrack {
  track: string;
  period: string;
  range_from: number;
  range_to: number;
  next_no: number;
  remaining: number;
}

export interface EInvoiceList {
  invoices: EInvoice[];
  track: EInvoiceTrack;
  track_label: string;
  issued_total: string;
  failed_count: number;
  queued_count: number;
  vat: {
    version: string;
    rate: string;
    rate_percent: string;
    effective_from: string;
    source_note: string;
  };
}

export interface VatRow {
  label: string;
  value: string;
  kind: '' | 'sub' | 'total';
}

export interface VatReturn {
  period_label: string;
  period_from: string;
  period_to: string;
  period_months: number;
  rows: VatRow[];
  sales: string;
  output_tax: string;
  input_base: string;
  input_tax: string;
  prior_credit: string;
  payable: string;
  carry_forward: string;
  expected_output_tax: string;
  output_tax_gap: string;
  reconciled: boolean;
  pending_sales: string;
  pending_output_tax: string;
  pending_input_tax: string;
  pending_vouchers: number;
  ruleset: {
    domain: string;
    version: string;
    rate: string;
    rate_percent: string;
    effective_from: string;
    source_note: string;
  };
}

export interface WithholdingRow {
  paid_on: string;
  payee: string;
  payee_id: string;
  income_code: string;
  income_name: string;
  code_label: string;
  gross_amount: string;
  withheld_amount: string;
  supplementary_amount: string;
  rate: string;
  threshold: string;
  below_threshold: boolean;
  basis: 'rate' | 'table';
  basis_label: string;
  note: string;
  memo: string;
  voucher_no: string | null;
  ruleset_version: string;
}

export interface WithholdingList {
  month: string;
  rows: WithholdingRow[];
  tax_total: string;
  supplementary_total: string;
  ruleset_versions: string[];
}

export interface FixedAsset {
  code: string;
  name: string;
  category: string;
  category_label: string;
  acquired_on: string;
  cost: string;
  accumulated: string;
  in_transit: string;
  book_value: string;
  monthly: string;
  life_years: number;
  life_label: string;
  months_left: number;
  fully_depreciated: boolean;
  status: string;
  recognised: boolean;
  origin_voucher_no: string | null;
  memo: string;
  life_ruleset_version: string;
}

export interface DepreciationRunRow {
  period: string;
  kind: 'opening' | 'monthly';
  run_on: string;
  voucher_no: string;
  voucher_status: string;
  ruleset_version: string | null;
  created_by: string;
  total: string;
}

export interface FixedAssetList {
  assets: FixedAsset[];
  cost_total: string;
  accumulated_total: string;
  book_total: string;
  gl_cost: string;
  gl_accumulated: string;
  cost_difference: string;
  accumulated_difference: string;
  reconciled: boolean;
  not_yet_posted: string;
  runs: DepreciationRunRow[];
}

export interface DepreciationLine {
  code: string;
  name: string;
  category_label: string;
  acquired_on: string;
  cost: string;
  accumulated: string;
  amount: string;
  monthly: string;
  life_years: number;
  life_label: string;
  life_ruleset_version: string;
  skip_reason: string | null;
}

export interface DepreciationPreview {
  period: string;
  period_end: string;
  assets: DepreciationLine[];
  total: string;
  count: number;
  already_run: DepreciationRunRow | null;
  ruleset_version: string;
}

export interface ReportRow {
  label: string;
  code: string | null;
  value: string | null;
  kind: 'head' | 'sub' | 'total' | '';
  hint: string | null;
}

export interface Unposted {
  count: number;
  amount: string;
}

export interface BalanceSheet {
  as_of: string;
  period_label: string;
  rows: ReportRow[];
  assets: string;
  liabilities: string;
  equity: string;
  equity_accounts: string;
  profit: string;
  difference: string;
  balanced: boolean;
  unposted: Unposted;
}

export interface IncomeStatement {
  from: string;
  to: string;
  period_label: string;
  rows: ReportRow[];
  revenue: string;
  cogs: string;
  gross_profit: string;
  operating_expense: string;
  operating_profit: string;
  profit: string;
  unposted: Unposted;
}

export interface CashFlow {
  from: string;
  to: string;
  period_label: string;
  rows: ReportRow[];
  opening: string;
  profit: string;
  operating_total: string;
  investing_total: string;
  financing_total: string;
  net_change: string;
  closing: string;
  ledger_closing: string;
  difference: string;
  cash_accounts: CashAccount[];
  cash_accounts_total: string;
  cash_accounts_difference: string;
  reconciled: boolean;
  unposted: Unposted;
}

export interface CompanySettingRow {
  code: string;
  label: string;
  value: string;
  note: string;
  editable: boolean;
  source: 'setting' | 'regulation';
  updated_at?: string;
  updated_by?: string;
  ruleset?: {
    domain: string;
    version: string;
    effective_from: string;
    source_note: string;
  };
}

export interface ChartAccount {
  code: string;
  name: string;
  kind: string;
  kind_label: string;
  normal_side: string;
  normal_side_label: string;
  is_postable: boolean;
  report_group: string;
  report_group_label: string;
  cash_flow_section: string | null;
  cash_flow_label: string;
  balance: string;
  line_count: number;
}

export interface ChartOfAccounts {
  accounts: ChartAccount[];
  total: number;
  used: number;
  as_of: string;
}

export interface DimensionValue {
  code: string;
  label: string;
  active: boolean;
}

export interface Dimension {
  code: string;
  label: string;
  note: string;
  values: DimensionValue[];
}

export interface DimensionList {
  dimensions: Dimension[];
  applied_to_entries: boolean;
}

export interface RoleRow {
  code: string;
  name: string;
  note: string;
}

export interface PermissionRow {
  code: string;
  label: string;
  note: string;
  op_names: string[];
  missing_ops: string[];
  boss_only_ops: string[];
}

export interface GrantCell {
  role_code: string;
  permission_code: string;
  level: 'full' | 'limited';
  mark: string;
  note: string;
  updated_at: string;
  updated_by: string;
}

export interface RoleMatrix {
  roles: RoleRow[];
  permissions: PermissionRow[];
  grants: GrantCell[];
  enforced: boolean;
  enforcement_note: string;
  actor_role: string;
}

export interface ApprovalStep {
  step_no: number;
  role_code: string | null;
  role_name: string | null;
  label: string;
  condition_note: string;
  min_amount: string | null;
  always: boolean;
}

export interface ApprovalRules {
  steps: ApprovalStep[];
  enforced: boolean;
  enforcement_note: string;
}

export interface SecurityRule {
  code: string;
  label: string;
  value: string;
  detail: string;
  enforced: boolean;
  source: 'policy' | 'mcp_tool_grant' | 'approval_rule';
  blocked_tools?: string[];
  risky_enabled_tools?: string[];
}

export interface SecurityPolicy {
  rules: SecurityRule[];
  enforced_count: number;
  total: number;
}

export interface ApiTokenRow {
  id: number;
  name: string;
  kind: 'api' | 'mcp';
  kind_label: string;
  scope: string;
  prefix: string;
  actor: string;
  actor_role: string;
  actor_role_name: string | null;
  created_at: string;
  created_by: string;
  last_used_at: string | null;
  rotate_days: number | null;
  rotate_due_on: string | null;
  overdue: boolean;
  expiry_note: string;
  revoked_at: string | null;
  revoked_by: string;
  live: boolean;
}

export interface ApiTokenList {
  tokens: ApiTokenRow[];
  live_count: number;
  storage_note: string;
  wiring_note: string;
}

export interface SystemHealth {
  migration: number | null;
  stats: {
    posted_vouchers: number;
    unposted_vouchers: number;
    accounts: number;
    audit_entries: number;
    audit_blocked: number;
    audit_since: string | null;
    tokens_live: number;
    tokens_revoked: number;
    rulesets: number;
    tools_enabled: number;
    tools_total: number;
  };
  rulesets: {
    domain: string;
    version: string | null;
    effective_from: string | null;
    applicable: boolean;
  }[];
  rulesets_missing: number;
  as_of: string;
  note: string;
}

export interface Healthz {
  status: string;
  brand: { ok: boolean; files: number; signed: boolean; fingerprint: string };
  db: { ok: boolean; migration: number | null };
  rustfs: { ok: boolean; endpoint: string };
  operations: number;
}

export async function getHealthz(): Promise<Healthz> {
  const res = await fetch('/healthz');
  if (!res.ok && res.status !== 503) throw new Error(`讀不到系統健康（HTTP ${res.status}）`);
  return (await res.json()) as Healthz;
}

export interface ApprovalStepState {
  step_no: number;
  label: string;
  role_code: string | null;
  role_name: string | null;
  condition_note?: string;
  min_amount: string | null;
  done?: boolean;
  reason?: string;
}

export interface ExpenseHistory {
  step_no: number;
  decision: 'approved' | 'rejected';
  actor: string;
  actor_role: string;
  note: string;
  decided_at: string;
}

export interface ExpenseClaim {
  claim_no: string;
  title: string;
  claimant: string;
  dept_code: string | null;
  dept_label: string | null;
  spent_on: string;
  amount: string;
  suggested_account_code: string | null;
  suggested_account_name: string | null;
  account_code: string | null;
  account_name: string | null;
  account_label: string | null;
  account_confirmed: boolean;
  pay_method: string;
  pay_method_label: string;
  status: 'submitted' | 'approving' | 'approved' | 'rejected';
  step_label: string;
  next_step: ApprovalStepState | null;
  required_steps: ApprovalStepState[];
  history: ExpenseHistory[];
  voucher_no: string | null;
  voucher_status: string | null;
  document_no: string | null;
  document_name: string | null;
  memo: string;
  created_at: string;
  created_by: string;
}

export interface ExpenseClaimList {
  claims: ExpenseClaim[];
  waiting_total: string;
  waiting_count: number;
  flow: ApprovalStepState[];
  thresholds_source: string;
  actor: string;
  actor_role: string;
}

export interface BudgetLine {
  account_code: string;
  account_name: string;
  budget: string;
  actual: string;
  remaining: string;
  rate: string | null;
  rate_label: string;
  alert: boolean;
  over_budget: boolean;
  line_count: number;
  note: string;
}

export interface BudgetVersionRow {
  code: string;
  label: string;
  fiscal_year: number;
  is_current: boolean;
  note: string;
}

export interface BudgetStatus {
  version: string;
  version_label: string;
  fiscal_year: number;
  versions: BudgetVersionRow[];
  from: string;
  to: string;
  rows: BudgetLine[];
  budget_total: string;
  actual_total: string;
  remaining_total: string;
  alert_percent: string;
  alert_count: number;
  actual_source: string;
}

export interface DeptPlRow {
  code: string | null;
  label: string;
  revenue: string;
  cost: string;
  profit: string;
  margin: string | null;
  margin_label: string;
  unallocated: boolean;
}

export interface DepartmentPl {
  from: string;
  to: string;
  rows: DeptPlRow[];
  total_revenue: string;
  total_cost: string;
  total_profit: string;
  statement_profit: string;
  difference: string;
  reconciled: boolean;
  unallocated_cost: string | null;
  unallocated_note: string;
}

export interface DocumentRow {
  doc_no: string;
  name: string;
  kind: string;
  kind_label: string;
  link_type: string;
  link_ref: string;
  archived: boolean;
  note: string;
  version_no: number;
  version_label: string;
  versions: number;
  file_name: string;
  object_key: string;
  byte_size: number;
  size_label: string;
  content_type: string;
  checksum: string;
  checksum_short: string;
  ocr_status: string;
  ocr_label: string;
  uploaded_at: string;
  uploaded_by: string;
  created_at: string;
  created_by: string;
}

export interface DocumentList {
  documents: DocumentRow[];
  stats: {
    documents: number;
    versions: number;
    bytes: number;
    size_label: string;
    ocr_pending: number;
    archived: number;
    vouchers_without_doc: number;
  };
  bucket: {
    endpoint: string;
    bucket: string;
    objects: number;
    bytes: number;
    size_label: string;
  } | null;
  bucket_difference: number | null;
  bucket_reconciled: boolean | null;
  storage_error: string | null;
  ocr_note: string;
  immutability_note: string;
  max_object_bytes: number;
}

export interface ImportSourceRow {
  code: string;
  label: string;
  format: string;
  cadence: string;
  supported: boolean;
  note: string;
  last_at: string | null;
  last_status: string | null;
  last_batch_no: string | null;
}

export interface ImportIssueRow {
  row_no: number;
  severity: 'error' | 'warning';
  issue: string;
  fix: string;
}

export interface ImportBatchRow {
  batch_no: string;
  source_code: string;
  source_label: string;
  file_name: string;
  row_count: number;
  ok_count: number;
  error_count: number;
  warn_count: number;
  status: 'preview' | 'committed' | 'rolled_back' | 'discarded';
  voucher_nos: string[];
  created_at: string;
  created_by: string;
  settled_at: string | null;
  settled_by: string;
  issues: ImportIssueRow[];
}

export interface IntegrationRow {
  code: string;
  label: string;
  detail: string;
  live: boolean;
  note: string;
}

export interface ImportList {
  sources: ImportSourceRow[];
  batches: ImportBatchRow[];
  integrations: IntegrationRow[];
  supported_sources: string[];
  live_integrations: number;
  csv_columns: string[];
  format_note: string;
  commit_note: string;
}

export interface ImportPreview {
  batch: ImportBatchRow;
  drafts: { date: string; memo: string; lines: unknown[] }[];
  total: string;
  can_commit: boolean;
  blocked_note: string | null;
}

export interface RulesetRef {
  domain: string;
  version: string;
  effective_from: string;
  effective_to: string | null;
  source_note: string;
}

export interface HrLink {
  label: string;
  implemented: boolean;
  partial?: boolean;
  detail: string;
  screen: string;
}

export interface HrDashboard {
  month: string;
  stats: Stat[];
  todos: Todo[];
  links: HrLink[];
  headcount: number;
  leaving: number;
  flow_kinds: string[];
  payroll_reconciled: boolean;
  payroll_not_yet_posted: string;
}

export interface EmployeeRow {
  emp_no: string;
  name: string;
  dept_code: string | null;
  dept_label: string | null;
  title: string;
  role_code: string | null;
  role_name: string | null;
  employment_type: string;
  employment_label: string;
  hired_on: string;
  left_on: string | null;
  status: string;
  status_label: string;
  service_months: number;
  service_label: string;
  annual_leave_days: number;
  annual_leave_taken: string;
  annual_leave_left: string;
  id_no_masked: string;
  bank_account: string;
  note: string;
}

export interface EmployeeList {
  employees: EmployeeRow[];
  total: number;
  active: number;
  leaving: number;
  by_dept: { label: string; count: number }[];
  avg_service_months: number;
  avg_service_label: string;
  annual_leave_ruleset: RulesetRef;
  salary_note: string;
  id_note: string;
}

export interface FlowStepRef {
  step_no: number;
  label: string;
  note: string;
}

export interface MovementStepState {
  step_no: number;
  label: string;
  note: string;
  done: boolean;
  done_by: string | null;
  done_at: string | null;
  done_note: string | null;
}

export interface MovementRow {
  movement_no: string;
  emp_no: string;
  name: string;
  dept_label: string | null;
  kind: string;
  kind_label: string;
  effective_on: string;
  from_value: string;
  to_value: string;
  reason: string;
  status: string;
  done_count: number;
  step_count: number;
  step_label: string;
  next_step: FlowStepRef | null;
  steps: MovementStepState[];
  created_at: string;
  created_by: string;
}

export interface MovementFlow {
  kind: string;
  label: string;
  steps: FlowStepRef[];
}

export interface MovementList {
  movements: MovementRow[];
  flows: MovementFlow[];
  open_count: number;
  flow_source: string;
}

export interface AttendanceRow {
  emp_no: string;
  name: string;
  dept_label: string | null;
  title: string;
  month: string;
  required_days: string;
  worked_days: string;
  leave_days: string;
  overtime_hours: string;
  overtime_pay: string;
  late_count: number;
  note: string;
}

export interface AttendanceMonth {
  month: string;
  months: string[];
  rows: AttendanceRow[];
  headcount: number;
  totals: {
    required_days: string;
    worked_days: string;
    leave_days: string;
    overtime_hours: string;
    overtime_pay: string;
    late_count: number;
  };
  overtime_recomputed: boolean;
  overtime_note: string;
}

export interface AnnualLeaveBalance {
  entitlement: string;
  taken: string;
  remaining: string;
}

export interface LeaveRequestRow {
  leave_no: string;
  emp_no: string;
  name: string;
  dept_label: string | null;
  kind: string;
  kind_label: string;
  from_on: string;
  to_on: string;
  days: string;
  reason: string;
  status: string;
  status_label: string;
  decided_by: string;
  decided_role: string;
  decided_at: string | null;
  decision_note: string;
  created_at: string;
  created_by: string;
  annual_leave: AnnualLeaveBalance | null;
}

export interface LeaveBalanceRow extends AnnualLeaveBalance {
  emp_no: string;
  name: string;
  dept_label: string | null;
}

export interface LeaveRequestList {
  requests: LeaveRequestRow[];
  pending_count: number;
  annual_leave_balances: LeaveBalanceRow[];
  annual_leave_year: number;
  annual_leave_ruleset: RulesetRef;
  annual_leave_quota_stored: boolean;
  annual_leave_note: string;
}

export interface PayrollRunRow {
  period: string;
  pay_on: string;
  headcount: number;
  gross_total: string;
  deduction_total: string;
  net_total: string;
  employer_total: string;
  voucher_no: string | null;
  voucher_status: string | null;
  rulesets: {
    labor_insurance: string;
    health_insurance: string;
    pension: string;
    withholding: string;
  };
  note: string;
  created_at: string;
  created_by: string;
}

export interface PayrollSummary {
  period: string;
  run: PayrollRunRow | null;
  runs: PayrollRunRow[];
  periods: string[];
  hr_posted_total: string;
  gl_balance: string;
  difference: string;
  reconciled: boolean;
  not_yet_posted: string;
  detail_visible: boolean;
  detail_op: string;
  detail_note: string;
  actor_role: string;
  employer_in_ledger: boolean;
  employer_note: string;
}

export interface PayslipRow {
  emp_no: string;
  name: string;
  title: string;
  dept_label: string | null;
  base_salary: string;
  overtime: string;
  allowance: string;
  gross: string;
  labor_insured_salary: string;
  health_insured_salary: string;
  pension_wage: string;
  labor_employee: string;
  health_employee: string;
  income_tax: string;
  deduction: string;
  net: string;
  labor_employer: string;
  health_employer: string;
  pension_employer: string;
  employer_total: string;
  paid_days: string;
  month_days: string;
  prorated: boolean;
  note: string;
  recompute_matches: boolean;
}

export interface PayslipList {
  period: string;
  run: PayrollRunRow;
  rows: PayslipRow[];
  recompute_all_match: boolean;
  recompute_note: string;
  periods: string[];
}

export interface PayrollRunResult {
  run: PayrollRunRow;
  voucher: Voucher;
  skipped: { emp_no: string; name: string; reason: string }[];
  warnings: { emp_no: string; name: string; issue: string }[];
  deduction_breakdown: {
    labor_employee: string;
    health_employee: string;
    income_tax: string;
  };
  withholding: {
    income_code: string;
    income_name: string;
    rate: string;
    note: string;
    ruleset: RulesetRef;
  };
  employer_in_ledger: boolean;
  employer_note: string;
}

export interface InsuranceRow {
  emp_no: string;
  name: string;
  dept_label: string | null;
  title: string;
  status: string;
  labor_insured_salary: string;
  labor_charged_on: string;
  labor_capped: boolean;
  labor_employer: string;
  labor_employee: string;
  labor_government: string;
  health_insured_salary: string;
  health_employer: string;
  health_employee: string;
  pension_wage: string;
  pension_employer: string;
  employer_total: string;
  employee_total: string;
}

export interface InsuranceNote {
  title: string;
  body: string;
  source: string;
  version: string;
}

export interface InsuranceSummary {
  as_of: string;
  rows: InsuranceRow[];
  headcount: number;
  capped_count: number;
  labor_employer: string;
  labor_employee: string;
  health_employer: string;
  health_employee: string;
  pension_employer: string;
  employer_total: string;
  employee_total: string;
  notes: InsuranceNote[];
  rulesets: { labor: RulesetRef; health: RulesetRef; pension: RulesetRef };
  posted_to_ledger: boolean;
  posting_note: string;
}

export interface InsuranceQuote {
  as_of: string;
  insured_salary: string;
  labor_charged_on: string;
  labor_capped: boolean;
  labor: {
    employer: string;
    employee: string;
    government: string;
    rate_percent: string;
    max_insured_salary: string;
    ruleset: RulesetRef;
  };
  health: {
    insured_salary: string;
    employer: string;
    employee: string;
    government: string;
    rate_percent: string;
    avg_dependents: string;
    ruleset: RulesetRef;
  };
  pension: {
    contribution_wage: string;
    employer: string;
    rate_percent: string;
    ruleset: RulesetRef;
  };
  employer_total: string;
  employee_total: string;
}

export interface ComplianceFailure {
  emp_no: string;
  name: string;
  message: string;
}

export interface ComplianceCheck {
  name: string;
  checked: boolean;
  passed: boolean | null;
  rulesets: RulesetRef[];
  failures: ComplianceFailure[];
  scope_note?: string;
  not_checked_reason?: string;
  required_domain?: string;
  evidence?: {
    employee_count: number;
    labor_employer: string;
    labor_employee: string;
    health_employer: string;
    health_employee: string;
    pension_employer: string;
  };
}

export interface HrCompliance {
  as_of: string;
  employee_count: number;
  checked_all_passed: boolean;
  coverage_complete: boolean;
  checked_count: number;
  unchecked_count: number;
  failure_count: number;
  checks: ComplianceCheck[];
  available_domains: string[];
  coverage_note: string;
}

export interface WithholdingStatementRecord {
  paid_on: string;
  gross_amount: string;
  withheld_amount: string | null;
  basis: string;
  basis_label: string;
  rate: string | null;
  amount_source: string;
  memo: string;
  voucher_no: string | null;
  ruleset_version: string;
  warning: string | null;
}

export interface WithholdingIncomeLine {
  income_code: string;
  income_name: string;
  gross_amount: string;
  withheld_amount: string | null;
  known_withheld_amount: string;
  withheld_complete: boolean;
  payment_count: number;
  bases: string[];
  basis_labels: string[];
  ruleset_versions: string[];
  records: WithholdingStatementRecord[];
}

export interface WithholdingStatement {
  payee: string;
  payee_id: string;
  person_specific: boolean;
  gross_amount: string;
  withheld_amount: string | null;
  known_withheld_amount: string;
  withheld_complete: boolean;
  payment_count: number;
  income_lines: WithholdingIncomeLine[];
  source_note: string;
}

export interface EmployeeIncomeRow {
  emp_no: string;
  name: string;
  payee_id: string;
  gross_amount: string;
  withheld_amount: string;
  payroll_months: number;
  source: string;
}

export interface WithholdingStatements {
  year: number;
  from: string;
  to: string;
  statements: WithholdingStatement[];
  statement_count: number;
  person_specific_count: number;
  gross_total: string;
  withheld_total: string | null;
  known_withheld_total: string;
  withheld_complete: boolean;
  missing_table_amounts: number;
  rulesets: RulesetRef[];
  employee_income: EmployeeIncomeRow[];
  payroll_reconciliation: {
    payroll_gross: string;
    withholding_record_table_gross: string;
    gross_difference: string;
    payroll_withheld: string;
    withholding_record_known_table_withheld: string;
    withheld_difference: string | null;
    table_withheld_complete: boolean;
    reconciled: boolean;
    note: string;
  };
  table_note: string;
  source_note: string;
}

export interface MyProfile {
  emp_no: string;
  name: string;
  dept_code: string | null;
  dept_label: string | null;
  title: string;
  role_code: string | null;
  role_name: string | null;
  employment_type: string;
  hired_on: string;
  left_on: string | null;
  status: string;
  service_months: number;
  service_label: string;
  bank_account_masked: string;
  privacy_note: string;
}

export interface MyAttendance {
  month: string;
  required_days: string;
  worked_days: string;
  leave_days: string;
  overtime_hours: string;
  overtime_pay: string;
  late_count: number;
  note: string;
  overtime_recomputed: boolean;
  overtime_note: string;
}

export interface MyAnnualLeave {
  year: number;
  entitlement: string;
  taken: string;
  remaining: string;
  ruleset: RulesetRef;
  quota_stored: boolean;
  note: string;
}

export interface MyLeaveRequest {
  leave_no: string;
  kind: string;
  kind_label: string;
  from_on: string;
  to_on: string;
  days: string;
  reason: string;
  status: string;
  status_label: string;
  decided_by: string;
  decided_role: string;
  decided_at: string | null;
  decision_note: string;
  created_at: string;
  created_by: string;
}

export interface MyPayslip {
  period: string;
  pay_on: string;
  base_salary: string;
  overtime: string;
  allowance: string;
  gross: string;
  deductions: {
    labor_insurance: string;
    health_insurance: string;
    income_tax: string;
    total: string;
  };
  net: string;
  employer_contributions: {
    labor_insurance: string;
    health_insurance: string;
    pension: string;
    total: string;
  };
  paid_days: string;
  month_days: string;
  prorated: boolean;
  rulesets: {
    labor_insurance: string;
    health_insurance: string;
    pension: string;
    withholding: string;
  };
  note: string;
}

export interface MyHome {
  profile: MyProfile;
  attendance_month: string;
  attendance: MyAttendance | null;
  attendance_note: string;
  annual_leave: MyAnnualLeave;
  submissions: {
    leave_requests: MyLeaveRequest[];
    expense_claims: ExpenseClaim[];
  };
  pending: {
    leave_requests: number;
    expense_claims: number;
  };
  identity_note: string;
}

export interface MyPayslipList {
  employee: { emp_no: string; name: string; dept_label: string | null; title: string };
  payslips: MyPayslip[];
  periods: string[];
  gross_total: string;
  net_total: string;
  scope_note: string;
}

export interface MyLeaveList {
  employee: { emp_no: string; name: string };
  requests: MyLeaveRequest[];
  leave_kinds: { kind: string; label: string }[];
  pending_count: number;
  annual_leave: MyAnnualLeave;
  scope_note: string;
}

export interface MyLeaveResult {
  request: MyLeaveRequest;
  status: string;
  days_source: string;
  scope_note: string;
}

export interface ExpenseReceipt {
  doc_no: string;
  file_name: string;
  byte_size: number;
  size_label: string;
  content_type: string;
  checksum_short: string;
  uploaded_at: string;
}

export interface MyExpenseClaim extends ExpenseClaim {
  receipts: ExpenseReceipt[];
  receipt_count: number;
  accepts_receipt: boolean;
}

export interface MyExpenseClaimList {
  employee: { emp_no: string; name: string };
  claims: MyExpenseClaim[];
  waiting_count: number;
  waiting_total: string;
  scope_note: string;
}

export interface AttachReceiptResult {
  claim_no: string;
  receipt: ExpenseReceipt;
  receipts: ExpenseReceipt[];
  scope_note: string;
}

export interface WorkSchedule {
  work_start: string | null;
  work_end: string | null;
  grace_minutes: number;
  grace_stated: boolean;
  flex_minutes: number;
  flexible: boolean;
  latest_start: string | null;
  judges_lateness: boolean;
  counts_overtime: boolean;
  source: string;
  note: string;
}

export interface PunchDay {
  date: string;
  weekday: string;
  in_at: string | null;
  out_at: string | null;
  in_source: string | null;
  out_source: string | null;
  manual: boolean;
  worked_seconds: number | null;
  worked_hours: string | null;
  open: boolean;
  late: boolean | null;
  late_seconds: number | null;
  /** When this person's day is expected to end. Under flexitime it moves
   *  with their arrival. */
  expected_end: string | null;
  overtime_seconds: number | null;
  note: string;
}

export interface PunchSummary {
  days_with_arrival: number;
  complete_days: number;
  open_days: number;
  worked_seconds: number;
  worked_hours: string;
  late_days: number | null;
  overtime_seconds: number | null;
  overtime_hours: string | null;
  manual_days: number;
  overtime_pay_computed: boolean;
  overtime_note: string;
}

export interface PunchReconciliation {
  imported: boolean;
  matches?: boolean;
  worked_days?: { imported: string; from_punches: number; difference: string };
  late_count?: { imported: number; from_punches: number | null; difference: number | null };
  overtime_hours?: {
    imported: string;
    from_punches: string | null;
    difference: string | null;
  };
  note: string;
}

export interface MyPunchList {
  employee: { emp_no: string; name: string };
  month: string;
  today: string;
  today_punches: PunchDay | null;
  next_kind: 'in' | 'out' | null;
  next_kind_label: string | null;
  days: PunchDay[];
  summary: PunchSummary;
  reconciliation: PunchReconciliation;
  schedule: WorkSchedule;
  scope_note: string;
}

export interface PunchResult {
  punch: {
    punched_on: string;
    at: string;
    kind: 'in' | 'out';
    kind_label: string;
    source: string;
  };
  today: PunchDay | null;
  schedule: WorkSchedule;
  scope_note: string;
}

export interface AttendancePunchList {
  month: string;
  months: string[];
  schedule: WorkSchedule;
  rows: {
    emp_no: string;
    name: string;
    dept_label: string | null;
    summary: PunchSummary;
    reconciliation: PunchReconciliation;
  }[];
  employee: { emp_no: string; name: string } | null;
  days: PunchDay[] | null;
  open_days: number;
  mismatched: number;
}

export interface MyMcpTools {
  usable: string[];
  blocked_for_ai: string[];
  role: string;
  note: string;
}

export interface MyApiToken {
  id: number;
  name: string;
  scope: string;
  prefix: string;
  created_at: string;
  last_used_at: string | null;
  rotate_days: number | null;
  expires_at: string | null;
  expired: boolean;
  expiry_note: string;
  revoked_at: string | null;
  live: boolean;
}

export interface MyApiTokenList {
  employee: { emp_no: string; name: string };
  tokens: MyApiToken[];
  live_count: number;
  tools: MyMcpTools;
  endpoint: string;
  storage_note: string;
  scope_note: string;
}

export interface MyApiTokenIssued {
  token: { id: number; name: string; prefix: string; scope: string; expiry_note: string };
  secret: string;
  tools: MyMcpTools;
  secret_note: string;
  scope_note: string;
}

export interface MyExpenseClaimResult {
  claim: ExpenseClaim;
  account_note: string;
  scope_note: string;
}

export const STATUS_LABEL: Record<Voucher['status'], string> = {
  draft: '待確認',
  pending: '待過帳',
  posted: '已過帳',
  void: '已作廢',
};

export function money(v: string): string {
  const neg = v.startsWith('-');
  const [whole = '0'] = (neg ? v.slice(1) : v).split('.');
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return neg ? `-${grouped}` : grouped;
}

export function reportAmount(v: string): string {
  return v.startsWith('-') ? `(${money(v.slice(1))})` : money(v);
}

export function signedMoney(v: string): string {
  return v.startsWith('-') ? money(v) : `+${money(v)}`;
}

export function trimDecimal(v: string): string {
  if (!v.includes('.')) return v;
  const t = v.replace(/0+$/, '').replace(/\.$/, '');
  return t === '' || t === '-' ? '0' : t;
}

export function isZero(v: string): boolean {
  return /^-?0*(\.0*)?$/.test(v);
}

function scaled(v: string): { digits: bigint; scale: number } | null {
  const t = v.trim();
  if (!/^-?\d*(\.\d*)?$/.test(t) || t === '' || t === '-' || t === '.') return null;
  const neg = t.startsWith('-');
  const [whole = '0', frac = ''] = (neg ? t.slice(1) : t).split('.');
  const digits = BigInt((whole || '0') + frac);
  return { digits: neg ? -digits : digits, scale: frac.length };
}

export function multiplyToDollars(a: string, b: string): string | null {
  const x = scaled(a);
  const y = scaled(b);
  if (!x || !y) return null;

  const unit = 10n ** BigInt(x.scale + y.scale);
  const product = x.digits * y.digits;
  const neg = product < 0n;
  const abs = neg ? -product : product;

  const rounded = abs / unit + (2n * (abs % unit) >= unit ? 1n : 0n);
  return `${neg && rounded !== 0n ? '-' : ''}${rounded}`;
}

export function addDecimals(a: string, b: string): string | null {
  const x = scaled(a);
  const y = scaled(b);
  if (!x || !y) return null;
  const scale = Math.max(x.scale, y.scale);
  const lift = (s: { digits: bigint; scale: number }) => s.digits * 10n ** BigInt(scale - s.scale);
  const sum = lift(x) + lift(y);
  if (scale === 0) return sum.toString();
  const neg = sum < 0n;
  const abs = (neg ? -sum : sum).toString().padStart(scale + 1, '0');
  return `${neg ? '-' : ''}${abs.slice(0, -scale)}.${abs.slice(-scale)}`;
}
