/**
 * Backend client.
 *
 * Every operation takes the same path, `POST /v1/ops/{name}`, which is the
 * same registry and the same dispatch as the MCP `tools/call`. The frontend
 * must have no action that only the UI can perform, or "every operation is
 * available over MCP" would be only half true.
 */

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

/**
 * What to do when a session becomes invalid. Registered once by `auth.ts`
 * on mount.
 *
 * Identity comes entirely from the HttpOnly session cookie, which the
 * frontend cannot read and therefore cannot use to decide whether a session
 * is still valid. The only reliable signal is a 401 from the server, so all
 * 401 handling is centralised here: any operation hitting one returns the
 * whole app to the login screen, rather than a dozen screens each saying
 * "failed to load" and turning "you were signed out" into "it is broken".
 */
type UnauthorizedHandler = () => void;

let onUnauthorized: UnauthorizedHandler | null = null;

export function setUnauthorizedHandler(handler: UnauthorizedHandler | null): void {
  onUnauthorized = handler;
}

/** The session expired or was revoked. A screen seeing this knows to return
 *  to login without interpreting any text. */
export const SESSION_EXPIRED = 'session_expired';

function sessionExpired(): OpFailed {
  onUnauthorized?.();
  return new OpFailed({
    code: SESSION_EXPIRED,
    message: '登入狀態已經失效，請重新登入。',
  });
}

/** Every request is identical: content type only. Identity is in the cookie
 *  and cannot be set by a header. */
function headers(): HeadersInit {
  return { 'content-type': 'application/json' };
}

/**
 * Call an operation.
 *
 * `prompt` is what the user actually said and is written to the audit trail:
 * an audit needs the intent, not only "somebody called draft_voucher".
 *
 * 401 and 403 are different and must not be conflated: 401 means you are no
 * longer signed in and the login screen should return; 403
 * (`not_permitted` or `needs_boss`) means you are signed in but this is not
 * yours to do, and the backend's wording should be shown so the user knows
 * what stopped them and why.
 */
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

export interface Meta {
  company: string;
  tax_id: string;
  /** The company name is still empty, so first-run setup has not happened. */
  setup_required: boolean;
  period: string;
  period_state: string;
  /** The current user, resolved from the session cookie; a request cannot
   *  set it. */
  actor: string;
  actor_role: string;
  /** Which roster row this account links to. null means no link, and the
   *  self-service module returns an error. */
  employee_id: number | null;
  oauth: { enabled: boolean; provider: string | null };
  version: string;
  licence: string;
  attribution: string;
  brand_build: string;
}

export async function getMeta(): Promise<Meta> {
  const res = await fetch('/v1/meta', { headers: headers() });
  if (res.status === 401) throw sessionExpired();
  if (!res.ok) throw new Error(`讀不到系統資訊（HTTP ${res.status}）`);
  return (await res.json()) as Meta;
}

// Operation response types
// Amounts are always decimal strings and are never converted to number:
// floating point causes problems with money.

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

// Settlement modules
// `outstanding` always equals the ledger balance of the account.
// `in_transit` is the part whose settlement voucher exists but is unposted,
// and it is not deducted from outstanding: the ledger has not moved, so the
// subsidiary ledger must not move either.

export interface ArInvoice {
  invoice_no: string;
  customer: string;
  issued_on: string;
  due_on: string;
  amount: string;
  settled: string;
  in_transit: string;
  outstanding: string;
  /** open: unpaid; receiving: receipt voucher unposted; settled: paid. */
  state: 'open' | 'receiving' | 'settled';
  /** Days overdue; negative means not yet due. */
  days_overdue: number;
  memo: string;
  origin_voucher_no: string | null;
  /** The source voucher is posted, so this counts in the ledger and the
   *  outstanding total. */
  recognised: boolean;
  receipt_voucher_no: string | null;
}

export interface AgingBucket {
  label: string;
  value: string;
  count: number;
}

/** Ageing and its reconciliation with the ledger. A false `reconciled` must
 *  be stated on screen. */
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
  /** open: unpaid; paying: payment voucher unposted; settled: paid. */
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
  /** Positive for money in, negative for money out. */
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
  /** One line on why it matches, e.g. "amount and date both agree". */
  reason: string;
}

// E-invoices

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
  /** Which version of the VAT rules produced this tax. */
  vat_version: string;
  upload_status: 'queued' | 'uploaded' | 'failed' | 'void';
  status_label: string;
  upload_attempts: number;
  uploaded_at: string | null;
  last_error: string | null;
  voucher_no: string | null;
  voucher_status: string | null;
  ar_invoice_no: string | null;
  /** The issuing voucher is posted, so this output tax is in the ledger. */
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
  /** "Jul-Aug period of ROC 26". The period length is computed by the
   *  backend from the VAT rules; the frontend does not derive it. */
  track_label: string;
  issued_total: string;
  failed_count: number;
  queued_count: number;
  /** The rate an invoice issued now would use. The frontend must not carry
   *  one of its own. */
  vat: {
    version: string;
    rate: string;
    rate_percent: string;
    effective_from: string;
    source_note: string;
  };
}

// Tax

export interface VatRow {
  label: string;
  value: string;
  /** Empty is a plain row, sub is a tax amount, total is a conclusion. */
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
  /** Net sales times the rate should equal the period movement of 2211. */
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
    /** The rate as a percentage (`0.05` becomes `5`), derived by the backend
     *  from the rate itself. */
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
  /** rate: computed from the withholding standard; table: from the salary
   *  withholding table. */
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

// Fixed assets

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
  /** Why this asset is not charged. null means it is. */
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

// Financial statements
// Every cell is computed by the backend from voucher_line. The frontend only
// lays it out and states whether it balances; no figure here is computed in
// the frontend.

/** One statement row. `value` is signed, and how a negative is presented
 *  (accounting convention is parentheses) is the frontend's decision. */
export interface ReportRow {
  label: string;
  code: string | null;
  value: string | null;
  /** head is a section title, sub a detail, total a total, empty a plain
   *  row. */
  kind: 'head' | 'sub' | 'total' | '';
  hint: string | null;
}

/** Vouchers that are unposted and therefore absent from the statements. */
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
  /** Profit for the period, not yet closed out. Excluding it from equity
   *  breaks the identity. */
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
  /** Closing cash derived from the three activity sections. */
  closing: string;
  /** Closing cash computed directly from the cash accounts. The two must
   *  agree. */
  ledger_closing: string;
  difference: string;
  cash_accounts: CashAccount[];
  cash_accounts_total: string;
  cash_accounts_difference: string;
  reconciled: boolean;
  unposted: Unposted;
}

// Company settings

export interface CompanySettingRow {
  code: string;
  label: string;
  value: string;
  note: string;
  editable: boolean;
  /** setting is a configured value; regulation is derived from the rule
   *  library and the owner cannot change it. */
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
  /** The dimension is not attached to journal lines yet. When false the
   *  screen must say so. */
  applied_to_entries: boolean;
}

// Roles and approvals

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
  /** The corresponding operations are not implemented, so this permission
   *  currently governs nothing. */
  missing_ops: string[];
  /** These operations carry an owner-only risk floor on top of this
   *  matrix. */
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
  /** dispatch reads this matrix on every call, deny by default. The screen
   *  must display it. */
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

// Security and operations

export interface SecurityRule {
  code: string;
  label: string;
  value: string;
  detail: string;
  /** Whether this rule is actually applied. When false the screen must
   *  mark it. */
  enforced: boolean;
  /** policy comes from the table; mcp_tool_grant and approval_rule are
   *  derived. */
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
  /** First eight characters. No plaintext exists in the system, so this is
   *  the only way to recognise which token a row is. */
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

/** Response of `GET /healthz`. Only the server process knows the brand
 *  fingerprint and the RustFS state. */
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

// Expense claims
// Which steps a claim must pass is derived by the backend from its amount
// against approval_rule. The frontend must not carry its own thresholds.

export interface ApprovalStepState {
  step_no: number;
  label: string;
  role_code: string | null;
  role_name: string | null;
  condition_note?: string;
  min_amount: string | null;
  done?: boolean;
  /** Why this step applies: "all claims" or "amount above 10,000". */
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
  /** Whether the account was decided by a person (true) or suggested by AI
   *  (false). The screen must distinguish them. */
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
  /** Where the threshold comes from. The screen must state it. */
  thresholds_source: string;
  actor: string;
  actor_role: string;
}

// Budgets and departmental P&L
// Budgeted figures are stored and actuals are always read live from the
// ledger, which `actual_source` states in the response.

export interface BudgetLine {
  account_code: string;
  account_name: string;
  budget: string;
  actual: string;
  remaining: string;
  rate: string | null;
  rate_label: string;
  /** Utilisation above the alert threshold. That threshold is a
   *  company_setting, not a frontend constant. */
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
  /** The row with no department. It is never apportioned away. */
  unallocated: boolean;
}

export interface DepartmentPl {
  from: string;
  to: string;
  rows: DeptPlRow[];
  total_revenue: string;
  total_cost: string;
  total_profit: string;
  /** Profit on the income statement. The two must agree. */
  statement_profit: string;
  difference: string;
  reconciled: boolean;
  unallocated_cost: string | null;
  unallocated_note: string;
}

// Documents

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
  /** Only ever pending or not_applicable: there is no OCR engine. */
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
  /** Usage measured on the RustFS side. null means object storage is not
   *  connected. */
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

// Imports and integrations

export interface ImportSourceRow {
  code: string;
  label: string;
  format: string;
  cadence: string;
  /** Whether a parser exists. When false the screen must show "not
   *  implemented". */
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
  /** Is it actually running? When false the screen must not say
   *  "enabled". */
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

// HR: shared
// Almost every HR figure carries a ruleset version: premiums, withholding
// and leave entitlement are never a current value stored in the database
// but always "computed under the version in force on a date". The screen
// must be able to answer which version produced a figure, which is why
// `RulesetRef` appears throughout.

/** Provenance of one ruleset, attached by the backend to every derived
 *  figure. */
export interface RulesetRef {
  domain: string;
  version: string;
  effective_from: string;
  /** null means no later version has taken over yet. */
  effective_to: string | null;
  source_note: string;
}

// HR dashboard

/** One link between HR and finance. `implemented` is a field rather than
 *  copy: writing all four in the same tone would be misleading. */
export interface HrLink {
  label: string;
  implemented: boolean;
  /** Connected, but only partly (withheld items reach the ledger while
   *  clearing the payment has no operation yet). */
  partial?: boolean;
  detail: string;
  /** Destination code from the backend (`hr_pay`, `hr_ins`, `gl`, ...),
   *  not a ScreenId. */
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

// Employee roster
// The roster carries no salary and no insured brackets. That is not a
// missing column but deliberate: per-person amounts have two exits, the
// owner-only list_payslips and the filed brackets on the insurance screen.

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
  /** Leave entitlement, derived from service length and the annual_leave
   *  rules. The database does not hold this number. */
  annual_leave_days: number;
  annual_leave_taken: string;
  annual_leave_left: string;
  /** Masked. The full national ID does not exist in this system. */
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
  /** Why the roster shows no salary. The screen must display it: a blank
   *  is more easily misread than a sentence. */
  salary_note: string;
  id_note: string;
}

// Onboarding, offboarding and movements
// Which steps a request passes comes from `hr_flow_step`: the company's own
// process, neither regulation nor code.

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

// Attendance

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
  /** Supplied by the timekeeping system. The rule library has no overtime
   *  multipliers, so hours are never multiplied out here. */
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
  /** Always false: overtime pay is not recomputed here, and the screen must
   *  say so. */
  overtime_recomputed: boolean;
  overtime_note: string;
}

// Leave

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
  /** A decided request carries no balance; deciding does not need the
   *  entitlement recomputed. */
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
  /** Always false: the entitlement is derived, and the database only stores
   *  approved days taken. */
  annual_leave_quota_stored: boolean;
  annual_leave_note: string;
}

// Payroll
// Totals are visible to everyone and per-person detail is owner only, which
// is one of the rules that genuinely stops a person.

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
  /** Which four rule versions this period used. The only place a
   *  retroactive amendment can be answered from. */
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
  /** Posted gross pay total on the HR side. */
  hr_posted_total: string;
  /** Ledger balance of 6111. Two independently computed paths, where
   *  equality is a conclusion rather than an assumption. */
  gl_balance: string;
  difference: string;
  reconciled: boolean;
  not_yet_posted: string;
  /** Whether the current identity may see per-person detail. When false the
   *  screen explains rather than showing a blank. */
  detail_visible: boolean;
  detail_op: string;
  detail_note: string;
  actor_role: string;
  /** Always false: the employer's insurance and pension contributions are
   *  not in the ledger yet. */
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
  /** Recomputed from the stored rule version, to compare against the stored
   *  figures. */
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
  /** Being below the minimum wage does not block the run, because people
   *  still have to be paid, but it must be visible. */
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

// Insurance and pension
// Rates, ceilings and the average dependant count all come from
// laysi-regulation, including the numbers inside the explanatory text.

export interface InsuranceRow {
  emp_no: string;
  name: string;
  dept_label: string | null;
  title: string;
  status: string;
  /** Insured salary as filed with the bureau: a fact. */
  labor_insured_salary: string;
  /** Insured salary actually charged, after the rule ceiling: a rule. */
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
  /** Always false. State it, so nobody assumes the employer share is
   *  already booked as an expense. */
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

// HR compliance
// `checked` and `passed` are different things. What the rule library does
// not cover cannot be passed off as "fine", so an item with
// `checked: false` must look different on screen from one checked and
// passed.

export interface ComplianceFailure {
  emp_no: string;
  name: string;
  message: string;
}

export interface ComplianceCheck {
  name: string;
  /** Whether it was really checked. false means the rule library has no
   *  such rule yet. */
  checked: boolean;
  /** A boolean only when `checked` is true; null when it was not checked. */
  passed: boolean | null;
  rulesets: RulesetRef[];
  failures: ComplianceFailure[];
  /** What was checked and what was not. */
  scope_note?: string;
  /** Why it was not checked. Always present when `checked` is false. */
  not_checked_reason?: string;
  /** Which domain's rules are missing. */
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
  /** Means only that the items with usable rules currently show no
   *  discrepancy. */
  checked_all_passed: boolean;
  /** Always false: it does not mean every HR regulation is covered. */
  coverage_complete: boolean;
  checked_count: number;
  unchecked_count: number;
  failure_count: number;
  checks: ComplianceCheck[];
  available_domains: string[];
  coverage_note: string;
}

// Annual withholding statements (owner only)

export interface WithholdingStatementRecord {
  paid_on: string;
  gross_amount: string;
  /** null on a table basis when the source supplied no tax; a rate is never
   *  substituted. */
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
  ruleset_versions: string[];
  records: WithholdingStatementRecord[];
}

export interface WithholdingStatement {
  payee: string;
  payee_id: string;
  /** The source carries per-person identification. false means it may be
   *  several people combined, which the system does not pretend to split. */
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
  /** null when any table-basis tax is missing. */
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

// Employee self-service
// The inputs of this group are all empty (`request_leave` accepts only the
// leave type, dates, days and reason). The subject comes from
// `ctx.employee_id`, and no field can name anyone else. `ctx.employee_id`
// itself is resolved from the `user_account` behind the session cookie and
// cannot be set by a request header. `scope_note` and `identity_note` in
// the response are the backend stating its own boundary, and the screen
// displays them verbatim.

/** Personal details on the self-service home. No national ID, and the
 *  payroll account shows only the bank name and last digits. */
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
  /** Masked by the backend before sending, e.g. "E.SUN, ending 5508". The
   *  full account number never leaves the backend. */
  bank_account_masked: string;
  privacy_note: string;
}

/** This month's own attendance. The HR table covers the whole company;
 *  this is one row. */
export interface MyAttendance {
  month: string;
  required_days: string;
  worked_days: string;
  leave_days: string;
  overtime_hours: string;
  overtime_pay: string;
  late_count: number;
  note: string;
  /** Always false: the overtime multipliers are not in the rule library, and
   *  the amount comes from the timekeeping source. */
  overtime_recomputed: boolean;
  overtime_note: string;
}

export interface MyAnnualLeave {
  year: number;
  entitlement: string;
  taken: string;
  remaining: string;
  ruleset: RulesetRef;
  /** Always false: the entitlement is derived from service length, and the
   *  database only sums approved days taken. */
  quota_stored: boolean;
  note: string;
}

/** One's own leave requests. Compared with the HR table this drops emp_no,
 *  name and department: it is only the caller, so there is nobody to
 *  identify. */
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
  /** What the company pays on top, not deducted from pay. Shown separately
   *  so the true cost of a person is visible. */
  employer_contributions: {
    labor_insurance: string;
    health_insurance: string;
    pension: string;
    total: string;
  };
  paid_days: string;
  month_days: string;
  prorated: boolean;
  /** Which four rule versions this period used. The only place a
   *  retroactive amendment can be answered from. */
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
  /** null when there is no timekeeping data for the month, which does not
   *  mean zero leave and zero lateness. */
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
  /** The backend's own statement that the subject is decided by the
   *  employee_id of a verified session. Display it verbatim. */
  identity_note: string;
}

export interface MyPayslipList {
  employee: { emp_no: string; name: string; dept_label: string | null; title: string };
  payslips: MyPayslip[];
  periods: string[];
  gross_total: string;
  net_total: string;
  /** "Queries only ctx.employee_id from a verified session; the input has
   *  no field that can name anyone else." */
  scope_note: string;
}

export interface MyLeaveList {
  employee: { emp_no: string; name: string };
  requests: MyLeaveRequest[];
  pending_count: number;
  annual_leave: MyAnnualLeave;
  scope_note: string;
}

export interface MyLeaveResult {
  request: MyLeaveRequest;
  status: string;
  /** Why the number of days is entered manually: the system has no weekend
   *  or public holiday rules and must not pretend to derive it. */
  days_source: string;
  scope_note: string;
}

export interface MyExpenseClaimList {
  employee: { emp_no: string; name: string };
  claims: ExpenseClaim[];
  waiting_count: number;
  waiting_total: string;
  scope_note: string;
}

export const STATUS_LABEL: Record<Voucher['status'], string> = {
  draft: '待確認',
  pending: '待過帳',
  posted: '已過帳',
  void: '已作廢',
};

/** Thousands separators. Amounts arrive as strings and formatting never
 *  goes through number. */
export function money(v: string): string {
  const neg = v.startsWith('-');
  const [whole = '0'] = (neg ? v.slice(1) : v).split('.');
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return neg ? `-${grouped}` : grouped;
}

/**
 * An amount on a financial statement. Negatives take parentheses by
 * accounting convention.
 *
 * The parentheses are presentation, not data: the backend always returns a
 * signed string, because only that can be used in further arithmetic. The
 * sign is read from the first character rather than by converting to
 * number.
 */
export function reportAmount(v: string): string {
  return v.startsWith('-') ? `(${money(v.slice(1))})` : money(v);
}

/**
 * A signed amount, used for bank transactions.
 *
 * A bank statement reads as plus for money in and minus for money out, and
 * splitting that into two columns hides the direction of a row. The sign is
 * always read from the first character, never by converting to number.
 */
export function signedMoney(v: string): string {
  return v.startsWith('-') ? money(v) : `+${money(v)}`;
}

/**
 * Tidy form of a decimal string: drop trailing zeros (`7.0` becomes `7`,
 * `0.50` becomes `0.5`).
 *
 * For days and hours. The database stores numeric, so `7.0` comes back, but
 * nobody says "7.0 days of leave left". This is string manipulation and
 * never converts to number: half a day is `0.5` and that decimal must not
 * be lost in conversion. Use `money()` for amounts, which needs thousands
 * separators.
 */
export function trimDecimal(v: string): string {
  if (!v.includes('.')) return v;
  const t = v.replace(/0+$/, '').replace(/\.$/, '');
  return t === '' || t === '-' ? '0' : t;
}

/** Whether an amount is zero. String comparison, never number. */
export function isZero(v: string): boolean {
  return /^-?0*(\.0*)?$/.test(v);
}

/** Split a decimal string into (integer digits, decimal places). `null`
 *  means it is not a valid number. */
function scaled(v: string): { digits: bigint; scale: number } | null {
  const t = v.trim();
  if (!/^-?\d*(\.\d*)?$/.test(t) || t === '' || t === '-' || t === '.') return null;
  const neg = t.startsWith('-');
  const [whole = '0', frac = ''] = (neg ? t.slice(1) : t).split('.');
  const digits = BigInt((whole || '0') + frac);
  return { digits: neg ? -digits : digits, scale: frac.length };
}

/**
 * Multiply two decimal strings, rounded to whole dollars.
 *
 * BigInt throughout, never number: 0.05 is not 0.05 in floating point, and
 * this computes what a customer owes in tax. Rounding is half away from
 * zero (not banker's rounding), matching `to_dollars` in the backend.
 *
 * The rate always comes from the backend's ruleset; the frontend must not
 * write one of its own.
 */
export function multiplyToDollars(a: string, b: string): string | null {
  const x = scaled(a);
  const y = scaled(b);
  if (!x || !y) return null;

  const unit = 10n ** BigInt(x.scale + y.scale);
  const product = x.digits * y.digits;
  const neg = product < 0n;
  const abs = neg ? -product : product;
  // Round half away from zero: carry when twice the remainder reaches one
  // unit.
  const rounded = abs / unit + (2n * (abs % unit) >= unit ? 1n : 0n);
  return `${neg && rounded !== 0n ? '-' : ''}${rounded}`;
}

/** Add two decimal strings, keeping the longer scale. Never converts to
 *  number either. */
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
