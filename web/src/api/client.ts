import { z } from 'zod';
import { RISKS, type OpName } from './generated';

export const apiError = z.object({
  code: z.string(),
  message: z.string(),
});
export type ApiError = z.infer<typeof apiError>;

export const sessionUser = z.object({
  id: z.number(),
  username: z.string(),
  display_name: z.string(),
  email: z.string(),
  role: z.string(),
  role_name: z.string(),
  permissions: z.array(z.string()),
  employee_id: z.number().nullable(),
  must_change_password: z.boolean(),
});
export type SessionUser = z.infer<typeof sessionUser>;

export const setupState = z.object({
  bootstrap_available: z.boolean(),
  token_required: z.boolean(),
});
export type SetupState = z.infer<typeof setupState>;

export const oauthState = z.object({
  enabled: z.boolean(),
  label: z.string(),
  domain: z.string().nullable(),
  creates_accounts: z.boolean(),
  password_login_disabled: z.boolean(),
});
export type OauthState = z.infer<typeof oauthState>;

export const authEnvelope = z.object({
  ok: z.boolean().optional(),
  error: apiError.optional(),
  user: sessionUser.optional(),
  oauth: oauthState.partial().optional(),
  setup: setupState.optional(),
});
export type AuthEnvelope = z.infer<typeof authEnvelope>;

export class OpFailed extends Error {
  constructor(
    public readonly error: ApiError,
    cause?: unknown,
  ) {
    super(error.message, { cause });
    this.name = 'OpFailed';
  }
}

export const SHAPE_MISMATCH: ApiError = {
  code: 'shape_mismatch',
  message: '伺服器回應的欄位與本畫面預期的不相符，請聯絡系統管理員。',
};

function parsed<S extends z.ZodType>(schema: S, data: unknown): z.infer<S> {
  const result = schema.safeParse(data);
  if (result.success) return result.data;
  throw new OpFailed(SHAPE_MISMATCH, result.error);
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

export async function callOp<S extends z.ZodType>(
  name: OpName,
  input: unknown,
  prompt: string | undefined,
  schema: S,
): Promise<z.infer<S>>;
export async function callOp<T>(name: OpName, input?: unknown, prompt?: string): Promise<T>;
export async function callOp(
  name: OpName,
  input?: unknown,
  prompt?: string,
  schema?: z.ZodType,
): Promise<unknown> {
  const res = await fetch(`/v1/ops/${name}`, {
    method: 'POST',
    headers: headers(),
    body: JSON.stringify({ input: input ?? {}, prompt: prompt ?? null }),
  });
  noteServerTime(res);
  if (res.status === 401) throw sessionExpired();
  const body: unknown = await res.json().catch(() => null);

  if (body && typeof body === 'object' && 'ok' in body) {
    const envelope = body as { ok: boolean; data?: unknown; error?: ApiError };
    if (envelope.ok && envelope.data !== undefined) {
      return schema ? parsed(schema, envelope.data) : envelope.data;
    }
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

export const meta = z.object({
  company: z.string(),
  tax_id: z.string(),
  setup_required: z.boolean(),
  period: z.string(),
  period_state: z.string(),
  actor: z.string(),
  actor_role: z.string(),
  employee_id: z.number().nullable(),
  oauth: z.object({
    enabled: z.boolean(),
    label: z.string().optional(),
    domain: z.string().nullable().optional(),
    creates_accounts: z.boolean().optional(),
  }),
  utc_offset: z.string(),
  version: z.string(),
  licence: z.string(),
});
export type Meta = z.infer<typeof meta>;

export async function getMeta(): Promise<Meta> {
  const res = await fetch('/v1/meta', { headers: headers() });
  noteServerTime(res);
  if (res.status === 401) throw sessionExpired();
  if (!res.ok) throw new Error(`讀不到系統資訊（HTTP ${res.status}）`);
  const info = parsed(meta, await res.json());
  setBusinessOffset(info.utc_offset);
  return info;
}

export const stat = z.object({
  kicker: z.string(),
  value: z.string(),
  note: z.string(),
});
export type Stat = z.infer<typeof stat>;
export const todo = z.object({
  count: z.string(),
  label: z.string(),
  due: z.string(),
  screen: z.string(),
});
export type Todo = z.infer<typeof todo>;
export const cashBar = z.object({
  month: z.string(),
  value: z.string(),
  label: z.string(),
});
export type CashBar = z.infer<typeof cashBar>;
export const recentAction = z.object({
  what: z.string(),
  time: z.string(),
  who: z.string(),
  channel: z.enum(['ui', 'mcp']),
  channel_label: z.string(),
  outcome: z.string(),
});
export type RecentAction = z.infer<typeof recentAction>;
export const dashboard = z.object({
  stats: z.array(stat),
  todos: z.array(todo),
  cash_bars: z.array(cashBar),
  recent: z.array(recentAction),
  ledger_started: z.boolean(),
});
export type Dashboard = z.infer<typeof dashboard>;

export const voucherLine = z.object({
  account_code: z.string(),
  account_name: z.string(),
  debit: z.string(),
  credit: z.string(),
  memo: z.string().nullable(),
});
export type VoucherLine = z.infer<typeof voucherLine>;
export const voucher = z.object({
  no: z.string(),
  date: z.string(),
  memo: z.string(),
  status: z.enum(['draft', 'pending', 'posted', 'void']),
  source: z.string(),
  created_by: z.string(),
  amount: z.string(),
  accounts_label: z.string(),
  lines: z.array(voucherLine),
});
export type Voucher = z.infer<typeof voucher>;

export const voucherList = z.object({ vouchers: z.array(voucher) });
export type VoucherList = z.infer<typeof voucherList>;

export const voucherResult = z.object({ voucher });
export type VoucherResult = z.infer<typeof voucherResult>;

export const reversedVoucher = z.object({ voucher, original: voucher });
export type ReversedVoucher = z.infer<typeof reversedVoucher>;

export const cashAccount = z.object({
  name: z.string(),
  account_no: z.string(),
  balance: z.string(),
});
export type CashAccount = z.infer<typeof cashAccount>;

export const cashPosition = z.object({
  accounts: z.array(cashAccount),
  assigned_total: z.string(),
  unattributed: z.string(),
  total: z.string(),
});
export type CashPosition = z.infer<typeof cashPosition>;

export const arInvoice = z.object({
  invoice_no: z.string(),
  customer: z.string(),
  issued_on: z.string(),
  due_on: z.string(),
  amount: z.string(),
  settled: z.string(),
  in_transit: z.string(),
  outstanding: z.string(),
  state: z.enum(['open', 'receiving', 'settled']),
  days_overdue: z.number(),
  memo: z.string(),
  origin_voucher_no: z.string().nullable(),
  recognised: z.boolean(),
  receipt_voucher_no: z.string().nullable(),
});
export type ArInvoice = z.infer<typeof arInvoice>;

export const agingBucket = z.object({
  label: z.string(),
  value: z.string(),
  count: z.number(),
});
export type AgingBucket = z.infer<typeof agingBucket>;

export const arAging = z.object({
  as_of: z.string(),
  buckets: z.array(agingBucket),
  total: z.string(),
  gl_balance: z.string(),
  difference: z.string(),
  reconciled: z.boolean(),
  not_yet_posted: z.string(),
  overdue_count: z.number(),
});
export type ArAging = z.infer<typeof arAging>;

export const apBill = z.object({
  bill_no: z.string(),
  vendor: z.string(),
  memo: z.string(),
  billed_on: z.string(),
  due_on: z.string(),
  amount: z.string(),
  settled: z.string(),
  in_transit: z.string(),
  outstanding: z.string(),
  state: z.enum(['open', 'paying', 'settled']),
  days_overdue: z.number(),
  origin_voucher_no: z.string().nullable(),
  recognised: z.boolean(),
  batch_no: z.string().nullable(),
});
export type ApBill = z.infer<typeof apBill>;

export const newApBill = z.object({
  bill: z.object({
    bill_no: z.string(),
    vendor: z.string(),
    amount: z.string(),
    voucher_no: z.string(),
  }),
});
export type NewApBill = z.infer<typeof newApBill>;

export const newPaymentBatch = z.object({
  batch: z.object({ batch_no: z.string(), voucher_no: z.string(), total: z.string() }),
});
export type NewPaymentBatch = z.infer<typeof newPaymentBatch>;

export const vatInfo = z.object({
  version: z.string(),
  rate: z.string(),
  rate_percent: z.string(),
  effective_from: z.string(),
  source_note: z.string(),
});
export type VatInfo = z.infer<typeof vatInfo>;

export const receivableList = z.object({
  invoices: z.array(arInvoice),
  total: z.string(),
  overdue_count: z.number(),
  vat: vatInfo,
});
export type ReceivableList = z.infer<typeof receivableList>;

export const newArInvoice = z.object({
  invoice: z.object({
    invoice_no: z.string(),
    customer: z.string(),
    amount: z.string(),
    voucher_no: z.string(),
  }),
  net_amount: z.string(),
  tax_amount: z.string(),
  vat_version: z.string(),
});
export type NewArInvoice = z.infer<typeof newArInvoice>;

export const newReceipt = z.object({
  receipt: z.object({ voucher_no: z.string(), amount: z.string() }),
});
export type NewReceipt = z.infer<typeof newReceipt>;

export const accountOption = z.object({
  code: z.string(),
  name: z.string(),
});
export type AccountOption = z.infer<typeof accountOption>;

export const payablesResult = z.object({
  bills: z.array(apBill),
  listed_total: z.string(),
  total: z.string(),
  gl_balance: z.string(),
  difference: z.string(),
  reconciled: z.boolean(),
  not_yet_posted: z.string(),
  overdue_count: z.number(),
  vat: vatInfo,
  debit_accounts: z.array(accountOption),
});
export type PayablesResult = z.infer<typeof payablesResult>;

export const bankTxn = z.object({
  id: z.number(),
  bank_account_id: z.number(),
  bank_name: z.string(),
  account_no: z.string(),
  txn_date: z.string(),
  memo: z.string(),
  amount: z.string(),
  kind: z.string(),
  matched_voucher_no: z.string().nullable(),
  matched_at: z.string().nullable(),
  matched_by: z.string().nullable(),
});
export type BankTxn = z.infer<typeof bankTxn>;

export const bankTransactions = z.object({
  transactions: z.array(bankTxn),
  unmatched_count: z.number(),
  unmatched_net: z.string(),
});
export type BankTransactions = z.infer<typeof bankTransactions>;

export const matchedBankTransaction = z.object({ transaction: bankTxn });
export type MatchedBankTransaction = z.infer<typeof matchedBankTransaction>;

export const matchCandidate = z.object({
  voucher_no: z.string(),
  date: z.string(),
  memo: z.string(),
  status: z.string(),
  net_effect: z.string(),
  amount_gap: z.string(),
  day_gap: z.number(),
  exact: z.boolean(),
  reason: z.string(),
});
export type MatchCandidate = z.infer<typeof matchCandidate>;

export const bankMatchSuggestions = z.object({
  transaction: bankTxn,
  candidates: z.array(matchCandidate),
});
export type BankMatchSuggestions = z.infer<typeof bankMatchSuggestions>;

export const vatRow = z.object({
  label: z.string(),
  value: z.string(),
  kind: z.enum(['', 'sub', 'total']),
});
export type VatRow = z.infer<typeof vatRow>;

export const vatReturn = z.object({
  period_label: z.string(),
  period_from: z.string(),
  period_to: z.string(),
  period_months: z.number(),
  rows: z.array(vatRow),
  sales: z.string(),
  output_tax: z.string(),
  input_base: z.string(),
  input_tax: z.string(),
  prior_credit: z.string(),
  payable: z.string(),
  carry_forward: z.string(),
  expected_output_tax: z.string(),
  output_tax_gap: z.string(),
  reconciled: z.boolean(),
  pending_sales: z.string(),
  pending_output_tax: z.string(),
  pending_input_tax: z.string(),
  pending_vouchers: z.number(),
  ruleset: z.object({
    domain: z.string(),
    version: z.string(),
    rate: z.string(),
    rate_percent: z.string(),
    effective_from: z.string(),
    source_note: z.string(),
  }),
});
export type VatReturn = z.infer<typeof vatReturn>;

export const withholdingRow = z.object({
  paid_on: z.string(),
  payee: z.string(),
  payee_id: z.string(),
  income_code: z.string(),
  income_name: z.string(),
  code_label: z.string(),
  gross_amount: z.string(),
  withheld_amount: z.string(),
  supplementary_amount: z.string(),
  rate: z.string(),
  threshold: z.string(),
  below_threshold: z.boolean(),
  basis: z.enum(['rate', 'table']),
  basis_label: z.string(),
  note: z.string(),
  memo: z.string(),
  voucher_no: z.string().nullable(),
  ruleset_version: z.string(),
});
export type WithholdingRow = z.infer<typeof withholdingRow>;

export const withholdingList = z.object({
  month: z.string(),
  rows: z.array(withholdingRow),
  tax_total: z.string(),
  supplementary_total: z.string(),
  ruleset_versions: z.array(z.string()),
});
export type WithholdingList = z.infer<typeof withholdingList>;

export const fixedAsset = z.object({
  code: z.string(),
  name: z.string(),
  category: z.string(),
  category_label: z.string(),
  acquired_on: z.string(),
  cost: z.string(),
  accumulated: z.string(),
  in_transit: z.string(),
  book_value: z.string(),
  monthly: z.string(),
  life_years: z.number(),
  life_label: z.string(),
  months_left: z.number(),
  fully_depreciated: z.boolean(),
  status: z.string(),
  recognised: z.boolean(),
  origin_voucher_no: z.string().nullable(),
  memo: z.string(),
  life_ruleset_version: z.string(),
});
export type FixedAsset = z.infer<typeof fixedAsset>;

export const depreciationRunRow = z.object({
  period: z.string(),
  kind: z.enum(['opening', 'monthly']),
  run_on: z.string(),
  voucher_no: z.string(),
  voucher_status: z.string(),
  ruleset_version: z.string().nullable(),
  created_by: z.string(),
  total: z.string(),
});
export type DepreciationRunRow = z.infer<typeof depreciationRunRow>;

export const assetCategory = z.object({
  code: z.string(),
  label: z.string(),
  life_years: z.string(),
  life_label: z.string(),
});
export type AssetCategory = z.infer<typeof assetCategory>;

export const fixedAssetList = z.object({
  assets: z.array(fixedAsset),
  cost_total: z.string(),
  accumulated_total: z.string(),
  book_total: z.string(),
  gl_cost: z.string(),
  gl_accumulated: z.string(),
  cost_difference: z.string(),
  accumulated_difference: z.string(),
  reconciled: z.boolean(),
  cost_accounts_label: z.string(),
  accum_accounts_label: z.string(),
  not_yet_posted: z.string(),
  runs: z.array(depreciationRunRow),
  categories: z.array(assetCategory),
  vat: vatInfo,
});
export type FixedAssetList = z.infer<typeof fixedAssetList>;

export const newFixedAsset = z.object({
  asset: z.object({
    code: z.string(),
    name: z.string(),
    cost: z.string(),
    origin_voucher_no: z.string().nullable(),
  }),
  life_years: z.number(),
  monthly: z.string(),
});
export type NewFixedAsset = z.infer<typeof newFixedAsset>;

export const depreciationRunResult = z.object({
  run: z.object({
    voucher_no: z.string(),
    total: z.string(),
    ruleset_version: z.string().nullable(),
  }),
});
export type DepreciationRunResult = z.infer<typeof depreciationRunResult>;

export const depreciationLine = z.object({
  code: z.string(),
  name: z.string(),
  category_label: z.string(),
  acquired_on: z.string(),
  cost: z.string(),
  accumulated: z.string(),
  amount: z.string(),
  monthly: z.string(),
  life_years: z.number(),
  life_label: z.string(),
  life_ruleset_version: z.string(),
  skip_reason: z.string().nullable(),
});
export type DepreciationLine = z.infer<typeof depreciationLine>;

export const depreciationPreview = z.object({
  period: z.string(),
  period_end: z.string(),
  assets: z.array(depreciationLine),
  total: z.string(),
  count: z.number(),
  already_run: depreciationRunRow.nullable(),
  ruleset_version: z.string(),
});
export type DepreciationPreview = z.infer<typeof depreciationPreview>;

export const reportRow = z.object({
  label: z.string(),
  code: z.string().nullable(),
  value: z.string().nullable(),
  kind: z.enum(['head', 'sub', 'total', '']),
  hint: z.string().nullable(),
});
export type ReportRow = z.infer<typeof reportRow>;

export const unposted = z.object({
  count: z.number(),
  amount: z.string(),
  after_period: z.object({ count: z.number(), amount: z.string(), through: z.string() }),
});
export type Unposted = z.infer<typeof unposted>;

export const balanceSheet = z.object({
  as_of: z.string(),
  period_label: z.string(),
  rows: z.array(reportRow),
  assets: z.string(),
  liabilities: z.string(),
  equity: z.string(),
  equity_accounts: z.string(),
  profit: z.string(),
  difference: z.string(),
  balanced: z.boolean(),
  unposted: unposted,
});
export type BalanceSheet = z.infer<typeof balanceSheet>;

export const incomeStatement = z.object({
  from: z.string(),
  to: z.string(),
  period_label: z.string(),
  rows: z.array(reportRow),
  revenue: z.string(),
  cogs: z.string(),
  gross_profit: z.string(),
  operating_expense: z.string(),
  operating_profit: z.string(),
  profit: z.string(),
  unposted: unposted,
});
export type IncomeStatement = z.infer<typeof incomeStatement>;

export const cashFlow = z.object({
  from: z.string(),
  to: z.string(),
  period_label: z.string(),
  rows: z.array(reportRow),
  opening: z.string(),
  profit: z.string(),
  operating_total: z.string(),
  investing_total: z.string(),
  financing_total: z.string(),
  net_change: z.string(),
  closing: z.string(),
  ledger_closing: z.string(),
  difference: z.string(),
  cash_accounts: z.array(cashAccount),
  cash_accounts_total: z.string(),
  cash_accounts_assigned: z.string(),
  cash_accounts_unattributed: z.string(),
  cash_accounts_difference: z.string(),
  reconciled: z.boolean(),
  unposted: unposted,
});
export type CashFlow = z.infer<typeof cashFlow>;

export const workCalendarDay = z.object({
  on_date: z.string(),
  weekday: z.string(),
  kind: z.string(),
  kind_label: z.string(),
  name: z.string().nullable(),
  source: z.enum(['government', 'company']).nullable(),
  default: z.boolean(),
});
export type WorkCalendarDay = z.infer<typeof workCalendarDay>;

export const workCalendarYear = z.object({
  year: z.number(),
  months: z.array(z.object({ month: z.number(), days: z.array(workCalendarDay) })),
  workdays: z.number(),
  holidays: z.number(),
  kinds: z.array(z.object({ code: z.string(), label: z.string() })),
});
export type WorkCalendarYear = z.infer<typeof workCalendarYear>;

export const workCalendarRefresh = z.object({
  year: z.number(),
  source: z.string(),
  read: z.number(),
  written: z.number(),
  kept: z.number(),
});
export type WorkCalendarRefresh = z.infer<typeof workCalendarRefresh>;

export const regulationField = z.object({
  path: z.string(),
  label: z.string(),
  value: z.string().optional(),
  editable: z.boolean().optional(),
  get children() {
    return z.array(regulationField).optional();
  },
});
export type RegulationField = z.infer<typeof regulationField>;

export const regulationVersion = z.object({
  version: z.string(),
  effective_from: z.string(),
  effective_to: z.string().nullable(),
  source_note: z.string(),
  in_force: z.boolean(),
  payload: z.record(z.string(), z.unknown()),
  fields: z.array(regulationField),
});
export type RegulationVersion = z.infer<typeof regulationVersion>;

export const regulationDomain = z.object({
  domain: z.string(),
  label: z.string(),
  applicable: z.boolean(),
  version: z.string().nullable(),
  versions: z.array(regulationVersion),
});
export type RegulationDomain = z.infer<typeof regulationDomain>;

export const regulationList = z.object({
  as_of: z.string(),
  domains: z.array(regulationDomain),
});
export type RegulationList = z.infer<typeof regulationList>;

export const companySettingRow = z.object({
  code: z.string(),
  label: z.string(),
  value: z.string(),
  note: z.string(),
  editable: z.boolean(),
  options: z.array(z.string()).optional(),
  source: z.enum(['setting', 'derived', 'regulation']),
  updated_at: z.string().optional(),
  updated_by: z.string().optional(),
  ruleset: z.object({
    domain: z.string(),
    version: z.string(),
    effective_from: z.string(),
    source_note: z.string(),
  }).optional(),
});
export type CompanySettingRow = z.infer<typeof companySettingRow>;

export const companySettings = z.object({
  settings: z.array(companySettingRow),
  editable_count: z.number(),
});
export type CompanySettings = z.infer<typeof companySettings>;

export const account = z.object({
  code: z.string(),
  name: z.string(),
  kind: z.string(),
  normal_side: z.string(),
  is_postable: z.boolean(),
  is_active: z.boolean(),
  parent_code: z.string().nullable(),
  sort_order: z.number(),
  report_group: z.string(),
  cash_flow_section: z.string().nullable(),
});
export type Account = z.infer<typeof account>;

export const accountList = z.object({ accounts: z.array(account) });
export type AccountList = z.infer<typeof accountList>;

export const newBankAccount = z.object({
  bank_account: z.object({
    id: z.number(),
    name: z.string(),
    account_no: z.string(),
    gl_code: z.string(),
    sort_order: z.number(),
  }),
});
export type NewBankAccount = z.infer<typeof newBankAccount>;

export const updatedMcpToolGrant = z.object({
  grant: z.object({
    tool_name: z.string(),
    enabled: z.boolean(),
    risk: z.string(),
    note: z.string(),
    updated_at: z.string(),
  }),
});
export type UpdatedMcpToolGrant = z.infer<typeof updatedMcpToolGrant>;

export const chartAccount = z.object({
  code: z.string(),
  name: z.string(),
  kind: z.string(),
  kind_label: z.string(),
  normal_side: z.string(),
  normal_side_label: z.string(),
  is_postable: z.boolean(),
  report_group: z.string(),
  report_group_label: z.string(),
  cash_flow_section: z.string().nullable(),
  cash_flow_label: z.string(),
  balance: z.string(),
  line_count: z.number(),
});
export type ChartAccount = z.infer<typeof chartAccount>;

export const chartOfAccounts = z.object({
  accounts: z.array(chartAccount),
  total: z.number(),
  used: z.number(),
  as_of: z.string(),
});
export type ChartOfAccounts = z.infer<typeof chartOfAccounts>;

export const dimensionValue = z.object({
  code: z.string(),
  label: z.string(),
  active: z.boolean(),
});
export type DimensionValue = z.infer<typeof dimensionValue>;

export const dimension = z.object({
  code: z.string(),
  label: z.string(),
  note: z.string(),
  values: z.array(dimensionValue),
});
export type Dimension = z.infer<typeof dimension>;

export const dimensionList = z.object({
  dimensions: z.array(dimension),
});
export type DimensionList = z.infer<typeof dimensionList>;

export const roleRow = z.object({
  code: z.string(),
  name: z.string(),
  note: z.string(),
});
export type RoleRow = z.infer<typeof roleRow>;

export const permissionRow = z.object({
  code: z.string(),
  label: z.string(),
  note: z.string(),
  op_names: z.array(z.string()),
  missing_ops: z.array(z.string()),
  boss_only_ops: z.array(z.string()),
  admin_only_ops: z.array(z.string()),
});
export type PermissionRow = z.infer<typeof permissionRow>;

export const grantCell = z.object({
  role_code: z.string(),
  permission_code: z.string(),
  level: z.enum(['full', 'limited']),
  mark: z.string(),
  note: z.string(),
  updated_at: z.string(),
  updated_by: z.string(),
});
export type GrantCell = z.infer<typeof grantCell>;

export const roleMatrix = z.object({
  roles: z.array(roleRow),
  permissions: z.array(permissionRow),
  grants: z.array(grantCell),
  actor_role: z.string(),
});
export type RoleMatrix = z.infer<typeof roleMatrix>;

export const newUserAccount = z.object({
  user: z.object({
    id: z.number(),
    username: z.string(),
    display_name: z.string(),
    email: z.string(),
    role_code: z.string(),
    role_name: z.string(),
    employee_no: z.string(),
    employee_name: z.string(),
  }),
  temporary_password: z.string().nullable(),
  must_change_password: z.boolean(),
});
export type NewUserAccount = z.infer<typeof newUserAccount>;

export const approvalStep = z.object({
  step_no: z.number(),
  role_code: z.string().nullable(),
  role_name: z.string().nullable(),
  label: z.string(),
  condition_note: z.string(),
  min_amount: z.string().nullable(),
  always: z.boolean(),
});
export type ApprovalStep = z.infer<typeof approvalStep>;

export const approvalRules = z.object({
  steps: z.array(approvalStep),
  enforcement_note: z.string(),
});
export type ApprovalRules = z.infer<typeof approvalRules>;

export const userAccountRow = z.object({
  username: z.string(),
  display_name: z.string(),
  email: z.string(),
  role_code: z.string(),
  role_name: z.string(),
  employee_no: z.string().nullable(),
  employee_name: z.string().nullable(),
  employee_status: z.string().nullable(),
  status: z.string(),
  must_change_password: z.boolean(),
  has_password: z.boolean(),
  identity_label: z.string().nullable(),
  identity_email: z.string().nullable(),
  identity_linked_at: z.string().nullable(),
  created_at: z.string(),
  created_by: z.string(),
  last_login_at: z.string().nullable(),
});
export type UserAccountRow = z.infer<typeof userAccountRow>;

export const userAccountList = z.object({
  accounts: z.array(userAccountRow),
});
export type UserAccountList = z.infer<typeof userAccountList>;

export const oidcSetting = z.object({
  enabled: z.boolean(),
  issuer: z.string(),
  provider_label: z.string(),
  client_id: z.string(),
  has_client_secret: z.boolean(),
  redirect_url: z.string(),
  allowed_domain: z.string(),
  provision_role: z.string(),
  provision_role_name: z.string().nullable(),
  password_login_disabled: z.boolean(),
  updated_at: z.string(),
  updated_by: z.string(),
  verified_at: z.string().nullable(),
  verify_error: z.string(),
  roles: z.array(roleRow),
});
export type OidcSetting = z.infer<typeof oidcSetting>;

export const backupSetting = z.object({
  enabled: z.boolean(),
  endpoint: z.string(),
  region: z.string(),
  bucket: z.string(),
  prefix: z.string(),
  access_key: z.string(),
  has_secret_key: z.boolean(),
  keep_days: z.number(),
  age_recipient: z.string(),
  restore_key_to_save: z.string(),
  blocked_reason: z.string().nullable(),
  last_run_at: z.string().nullable(),
  last_error: z.string(),
  last_bytes: z.number().nullable(),
  updated_at: z.string(),
  updated_by: z.string(),
});
export type BackupSetting = z.infer<typeof backupSetting>;

export const updatedBackupSetting = z.object({
  bucket: z.string(),
  enabled: z.boolean(),
  blocked_reason: z.string().nullable(),
});
export type UpdatedBackupSetting = z.infer<typeof updatedBackupSetting>;

export const updatedOidcSetting = z.object({
  changed: z.array(z.string()),
  enabled: z.boolean(),
});
export type UpdatedOidcSetting = z.infer<typeof updatedOidcSetting>;

export const unlinkedUserIdentity = z.object({
  user: z.object({ username: z.string(), display_name: z.string() }),
  removed: z.number(),
  revoked_sessions: z.number(),
});
export type UnlinkedUserIdentity = z.infer<typeof unlinkedUserIdentity>;

export const userPasswordReset = z.object({
  user: z.object({
    id: z.number(),
    username: z.string(),
    display_name: z.string(),
    role_code: z.string(),
    role_name: z.string(),
  }),
  temporary_password: z.string(),
  must_change_password: z.boolean(),
  revoked_sessions: z.number(),
});
export type UserPasswordReset = z.infer<typeof userPasswordReset>;

export const changedUserRole = z.object({
  user: z.object({
    id: z.number(),
    username: z.string(),
    display_name: z.string(),
    role_code: z.string(),
    role_name: z.string(),
    previous_role_name: z.string(),
    status: z.string(),
  }),
  revoked_sessions: z.number(),
  revoked_tokens: z.number(),
});
export type ChangedUserRole = z.infer<typeof changedUserRole>;

export const deletedUserAccount = z.object({
  user: z.object({
    username: z.string(),
    display_name: z.string(),
    role_code: z.string(),
    role_name: z.string(),
  }),
  remaining: z.number(),
});
export type DeletedUserAccount = z.infer<typeof deletedUserAccount>;

export const updatedUserAccount = z.object({
  user: z.object({
    id: z.number(),
    username: z.string(),
    display_name: z.string(),
    email: z.string(),
    role_code: z.string(),
    role_name: z.string(),
    status: z.string(),
  }),
  changed: z.array(z.string()),
});
export type UpdatedUserAccount = z.infer<typeof updatedUserAccount>;

export const apiTokenRow = z.object({
  id: z.number(),
  name: z.string(),
  kind: z.enum(['api', 'mcp']),
  kind_label: z.string(),
  scope: z.string(),
  prefix: z.string(),
  actor: z.string(),
  actor_role: z.string(),
  actor_role_name: z.string().nullable(),
  created_at: z.string(),
  created_by: z.string(),
  last_used_at: z.string().nullable(),
  rotate_days: z.number().nullable(),
  rotate_due_on: z.string().nullable(),
  overdue: z.boolean(),
  expiry_note: z.string(),
  revoked_at: z.string().nullable(),
  revoked_by: z.string(),
  live: z.boolean(),
});
export type ApiTokenRow = z.infer<typeof apiTokenRow>;

export const issuedApiToken = z.object({
  token: z.object({ name: z.string() }),
  secret: z.string(),
  secret_note: z.string(),
});
export type IssuedApiToken = z.infer<typeof issuedApiToken>;

export const auditEntry = z.object({
  at: z.string(),
  actor: z.string(),
  actor_role_name: z.string(),
  tool_name: z.string(),
  risk: z.string(),
  prompt: z.string().nullable(),
  channel: z.string(),
  result_summary: z.string(),
  outcome: z.string(),
  client_ip: z.string().nullable(),
});
export type AuditEntry = z.infer<typeof auditEntry>;

export const aiAuditList = z.object({ entries: z.array(auditEntry) });
export type AiAuditList = z.infer<typeof aiAuditList>;

export const apiTokenList = z.object({
  tokens: z.array(apiTokenRow),
  live_count: z.number(),
  wiring_note: z.string(),
});
export type ApiTokenList = z.infer<typeof apiTokenList>;

export const systemHealth = z.object({
  migration: z.number().nullable(),
  stats: z.object({
    posted_vouchers: z.number(),
    unposted_vouchers: z.number(),
    accounts: z.number(),
    audit_entries: z.number(),
    audit_blocked: z.number(),
    audit_errors: z.number(),
    audit_since: z.string().nullable(),
    tokens_live: z.number(),
    tokens_revoked: z.number(),
    rulesets: z.number(),
    tools_enabled: z.number(),
    tools_total: z.number(),
  }),
  rulesets: z.array(z.object({
    domain: z.string(),
    version: z.string().nullable(),
    effective_from: z.string().nullable(),
    applicable: z.boolean(),
  })),
  rulesets_missing: z.number(),
  as_of: z.string(),
  note: z.string(),
});
export type SystemHealth = z.infer<typeof systemHealth>;

export const healthz = z.object({
  status: z.string(),
  db: z.object({ ok: z.boolean(), migration: z.number().nullish() }),
  rustfs: z.object({ ok: z.boolean(), endpoint: z.string().optional() }),
  operations: z.number().optional(),
});
export type Healthz = z.infer<typeof healthz>;

export async function getHealthz(): Promise<Healthz> {
  const res = await fetch('/healthz');
  if (!res.ok && res.status !== 503) throw new Error(`讀不到系統健康（HTTP ${res.status}）`);
  return parsed(healthz, await res.json());
}

export const opInfo = z.object({
  name: z.string(),
  risk: z.enum(RISKS),
  risk_label: z.string(),
  summary: z.string(),
  enabled: z.boolean(),
});
export type OpInfo = z.infer<typeof opInfo>;

export const opList = z.object({ operations: z.array(opInfo) });

export async function getOps(): Promise<OpInfo[]> {
  const res = await fetch('/v1/ops', { headers: headers() });
  noteServerTime(res);
  if (res.status === 401) throw sessionExpired();
  if (!res.ok) throw new Error(`讀不到操作清單（HTTP ${res.status}）`);
  return parsed(opList, await res.json()).operations;
}

export const approvalStepState = z.object({
  step_no: z.number(),
  label: z.string(),
  role_code: z.string().nullable(),
  role_name: z.string().nullable(),
  condition_note: z.string().optional(),
  min_amount: z.string().nullable(),
  done: z.boolean().optional(),
  reason: z.string().optional(),
});
export type ApprovalStepState = z.infer<typeof approvalStepState>;

export const expenseHistory = z.object({
  step_no: z.number(),
  decision: z.enum(['approved', 'rejected']),
  actor: z.string(),
  actor_role: z.string(),
  note: z.string(),
  decided_at: z.string(),
});
export type ExpenseHistory = z.infer<typeof expenseHistory>;

export const expenseClaim = z.object({
  claim_no: z.string(),
  title: z.string(),
  claimant: z.string(),
  dept_code: z.string().nullable(),
  dept_label: z.string().nullable(),
  spent_on: z.string(),
  amount: z.string(),
  suggested_account_code: z.string().nullable(),
  suggested_account_name: z.string().nullable(),
  account_code: z.string().nullable(),
  account_name: z.string().nullable(),
  account_label: z.string().nullable(),
  account_confirmed: z.boolean(),
  pay_method: z.string(),
  pay_method_label: z.string(),
  status: z.enum(['submitted', 'approving', 'approved', 'rejected']),
  step_label: z.string(),
  next_step: approvalStepState.nullable(),
  required_steps: z.array(approvalStepState),
  history: z.array(expenseHistory),
  voucher_no: z.string().nullable(),
  voucher_status: z.string().nullable(),
  document_no: z.string().nullable(),
  document_name: z.string().nullable(),
  memo: z.string(),
  created_at: z.string(),
  created_by: z.string(),
  can_approve: z.boolean().optional(),
  approve_blocked_reason: z.string().nullable().optional(),
});
export type ExpenseClaim = z.infer<typeof expenseClaim>;

export const expenseClaimResult = z.object({ claim: expenseClaim });
export type ExpenseClaimResult = z.infer<typeof expenseClaimResult>;

export const expenseApproval = z.object({
  claim: expenseClaim,
  voucher: z.object({ no: z.string() }).nullable(),
  step_taken: z.number(),
  finished: z.boolean(),
});
export type ExpenseApproval = z.infer<typeof expenseApproval>;

export const expenseClaimList = z.object({
  claims: z.array(expenseClaim),
  waiting_total: z.string(),
  waiting_count: z.number(),
  flow: z.array(approvalStepState),
  thresholds_source: z.string(),
  actor: z.string(),
  actor_role: z.string(),
});
export type ExpenseClaimList = z.infer<typeof expenseClaimList>;

export const budgetLine = z.object({
  account_code: z.string(),
  account_name: z.string(),
  budget: z.string(),
  actual: z.string(),
  remaining: z.string(),
  rate: z.string().nullable(),
  rate_label: z.string(),
  alert: z.boolean(),
  over_budget: z.boolean(),
  line_count: z.number(),
  note: z.string(),
});
export type BudgetLine = z.infer<typeof budgetLine>;

export const budgetVersionRow = z.object({
  code: z.string(),
  label: z.string(),
  fiscal_year: z.number(),
  is_current: z.boolean(),
  note: z.string(),
});
export type BudgetVersionRow = z.infer<typeof budgetVersionRow>;

export const newBudgetVersion = z.object({
  version: z.object({ code: z.string(), label: z.string() }),
});
export type NewBudgetVersion = z.infer<typeof newBudgetVersion>;

export const budgetStatus = z.object({
  version: z.string().nullable(),
  version_label: z.string(),
  fiscal_year: z.number(),
  versions: z.array(budgetVersionRow),
  from: z.string(),
  to: z.string(),
  rows: z.array(budgetLine),
  budget_total: z.string(),
  actual_total: z.string(),
  remaining_total: z.string(),
  alert_percent: z.string().nullable(),
  alert_count: z.number(),
  actual_source: z.string(),
});
export type BudgetStatus = z.infer<typeof budgetStatus>;

export const deptPlRow = z.object({
  code: z.string().nullable(),
  label: z.string(),
  revenue: z.string(),
  cost: z.string(),
  profit: z.string(),
  margin: z.string().nullable(),
  margin_label: z.string(),
  unallocated: z.boolean(),
});
export type DeptPlRow = z.infer<typeof deptPlRow>;

export const departmentPl = z.object({
  from: z.string(),
  to: z.string(),
  rows: z.array(deptPlRow),
  total_revenue: z.string(),
  total_cost: z.string(),
  total_profit: z.string(),
  statement_profit: z.string(),
  difference: z.string(),
  reconciled: z.boolean(),
  unallocated_cost: z.string().nullable(),
  unallocated_note: z.string(),
});
export type DepartmentPl = z.infer<typeof departmentPl>;

export const documentRow = z.object({
  doc_no: z.string(),
  name: z.string(),
  kind: z.string(),
  kind_label: z.string(),
  link_type: z.string(),
  link_ref: z.string(),
  archived: z.boolean(),
  note: z.string(),
  version_no: z.number(),
  version_label: z.string(),
  versions: z.number(),
  file_name: z.string(),
  object_key: z.string(),
  byte_size: z.number(),
  size_label: z.string(),
  content_type: z.string(),
  checksum: z.string(),
  checksum_short: z.string(),
  uploaded_at: z.string(),
  uploaded_by: z.string(),
  created_at: z.string(),
  created_by: z.string(),
});
export type DocumentRow = z.infer<typeof documentRow>;

export const uploadedDocument = z.object({ document: documentRow, new_version: z.boolean() });
export type UploadedDocument = z.infer<typeof uploadedDocument>;

export const verifiedDocument = z.object({
  document: documentRow,
  verified: z.boolean(),
  checksum_recomputed: z.string(),
  byte_size_actual: z.number(),
});
export type VerifiedDocument = z.infer<typeof verifiedDocument>;

export const documentList = z.object({
  documents: z.array(documentRow),
  stats: z.object({
    documents: z.number(),
    versions: z.number(),
    bytes: z.number(),
    size_label: z.string(),
    archived: z.number(),
    vouchers_without_doc: z.number(),
  }),
  bucket: z.object({
    endpoint: z.string(),
    bucket: z.string(),
    objects: z.number(),
    bytes: z.number(),
    size_label: z.string(),
  }).nullable(),
  bucket_difference: z.number().nullable(),
  bucket_reconciled: z.boolean().nullable(),
  storage_error: z.string().nullable(),
  immutability_note: z.string(),
  max_object_bytes: z.number(),
});
export type DocumentList = z.infer<typeof documentList>;

export const importSourceRow = z.object({
  code: z.string(),
  label: z.string(),
  format: z.string(),
  cadence: z.string(),
  supported: z.boolean(),
  note: z.string(),
  last_at: z.string().nullable(),
  last_status: z.string().nullable(),
  last_batch_no: z.string().nullable(),
});
export type ImportSourceRow = z.infer<typeof importSourceRow>;

export const importIssueRow = z.object({
  row_no: z.number(),
  severity: z.enum(['error', 'warning']),
  issue: z.string(),
  fix: z.string(),
});
export type ImportIssueRow = z.infer<typeof importIssueRow>;

export const importBatchRow = z.object({
  batch_no: z.string(),
  source_code: z.string(),
  source_label: z.string(),
  file_name: z.string(),
  row_count: z.number(),
  ok_count: z.number(),
  error_count: z.number(),
  warn_count: z.number(),
  status: z.enum(['preview', 'committed', 'rolled_back', 'discarded']),
  voucher_nos: z.array(z.string()),
  created_at: z.string(),
  created_by: z.string(),
  settled_at: z.string().nullable(),
  settled_by: z.string(),
  issues: z.array(importIssueRow),
});
export type ImportBatchRow = z.infer<typeof importBatchRow>;

export const importCommit = z.object({ batch: importBatchRow, total: z.string() });
export type ImportCommit = z.infer<typeof importCommit>;

export const importRollback = z.object({
  batch: importBatchRow,
  removed: z.array(z.string()),
});
export type ImportRollback = z.infer<typeof importRollback>;

export const importList = z.object({
  sources: z.array(importSourceRow),
  batches: z.array(importBatchRow),
  supported_sources: z.array(z.string()),
  csv_columns: z.array(z.string()),
  format_note: z.string(),
  commit_note: z.string(),
});
export type ImportList = z.infer<typeof importList>;

export const importPreview = z.object({
  batch: importBatchRow,
  drafts: z.array(z.object({ date: z.string(), memo: z.string(), lines: z.array(z.unknown()) })),
  total: z.string(),
  can_commit: z.boolean(),
  blocked_note: z.string().nullable(),
});
export type ImportPreview = z.infer<typeof importPreview>;

export const rulesetRef = z.object({
  domain: z.string(),
  version: z.string(),
  effective_from: z.string(),
  effective_to: z.string().nullable(),
  source_note: z.string(),
});
export type RulesetRef = z.infer<typeof rulesetRef>;

export const hrLink = z.object({
  label: z.string(),
  implemented: z.boolean(),
  partial: z.boolean().optional(),
  detail: z.string(),
  screen: z.string(),
});
export type HrLink = z.infer<typeof hrLink>;

export const hrDashboard = z.object({
  month: z.string(),
  stats: z.array(stat),
  todos: z.array(todo),
  links: z.array(hrLink),
  headcount: z.number(),
  leaving: z.number(),
  payroll_reconciled: z.boolean(),
  payroll_not_yet_posted: z.string(),
  payroll_posted_total: z.string(),
  payroll_gl_total: z.string(),
});
export type HrDashboard = z.infer<typeof hrDashboard>;

export const employeeBankAccountRow = z.object({
  id: z.number(),
  bank_name: z.string(),
  account_no: z.string(),
  is_payroll: z.boolean(),
  note: z.string(),
});
export type EmployeeBankAccountRow = z.infer<typeof employeeBankAccountRow>;

export const employeeRow = z.object({
  emp_no: z.string(),
  name: z.string(),
  dept_code: z.string().nullable(),
  dept_label: z.string().nullable(),
  title: z.string(),
  employment_type: z.string(),
  employment_label: z.string(),
  hired_on: z.string(),
  left_on: z.string().nullable(),
  status: z.string(),
  status_label: z.string(),
  service_months: z.number(),
  service_label: z.string(),
  annual_leave_days: z.number(),
  annual_leave_taken: z.string(),
  annual_leave_left: z.string(),
  id_no: z.string(),
  base_salary: z.string(),
  labor_insured_salary: z.string(),
  health_insured_salary: z.string(),
  pension_wage: z.string(),
  bank_accounts: z.array(employeeBankAccountRow),
  note: z.string(),
});
export type EmployeeRow = z.infer<typeof employeeRow>;

export const employeeList = z.object({
  employees: z.array(employeeRow),
  total: z.number(),
  active: z.number(),
  leaving: z.number(),
  by_dept: z.array(z.object({ label: z.string(), count: z.number() })),
  avg_service_months: z.number(),
  avg_service_label: z.string(),
  annual_leave_ruleset: rulesetRef,
  salary_note: z.string(),
});
export type EmployeeList = z.infer<typeof employeeList>;

export const closedAccess = z.object({
  logins: z.array(z.string()),
  sessions: z.number(),
  tokens: z.number(),
});
export type ClosedAccess = z.infer<typeof closedAccess>;

export const updatedEmployee = z.object({
  employee: z.object({ emp_no: z.string(), name: z.string() }),
  closed_access: closedAccess.optional(),
});
export type UpdatedEmployee = z.infer<typeof updatedEmployee>;

export const annualLeaveBalance = z.object({
  entitlement: z.string(),
  taken: z.string(),
  remaining: z.string(),
});
export type AnnualLeaveBalance = z.infer<typeof annualLeaveBalance>;

export const leaveRequestRow = z.object({
  leave_no: z.string(),
  emp_no: z.string(),
  name: z.string(),
  dept_label: z.string().nullable(),
  kind: z.string(),
  kind_label: z.string(),
  from_at: z.string(),
  to_at: z.string(),
  from_on: z.string(),
  to_on: z.string(),
  hours: z.string(),
  days: z.string(),
  reason: z.string(),
  status: z.string(),
  status_label: z.string(),
  decided_by: z.string(),
  decided_role: z.string(),
  decided_at: z.string().nullable(),
  decision_note: z.string(),
  created_at: z.string(),
  created_by: z.string(),
  annual_leave: annualLeaveBalance.nullable(),
  can_decide: z.boolean().optional(),
  decide_blocked_reason: z.string().nullable().optional(),
});
export type LeaveRequestRow = z.infer<typeof leaveRequestRow>;

export const leaveBalanceRow = annualLeaveBalance.extend({
  emp_no: z.string(),
  name: z.string(),
  dept_label: z.string().nullable(),
});
export type LeaveBalanceRow = z.infer<typeof leaveBalanceRow>;

export const leaveRequestList = z.object({
  requests: z.array(leaveRequestRow),
  pending_count: z.number(),
  annual_leave_balances: z.array(leaveBalanceRow),
  annual_leave_year: z.number(),
  annual_leave_ruleset: rulesetRef,
  annual_leave_quota_stored: z.boolean(),
  annual_leave_note: z.string(),
});
export type LeaveRequestList = z.infer<typeof leaveRequestList>;

export const payrollRunRow = z.object({
  period: z.string(),
  pay_on: z.string(),
  headcount: z.number(),
  gross_total: z.string(),
  deduction_total: z.string(),
  net_total: z.string(),
  employer_total: z.string(),
  voucher_no: z.string().nullable(),
  voucher_status: z.string().nullable(),
  rulesets: z.object({
    labor_insurance: z.string(),
    health_insurance: z.string(),
    pension: z.string(),
    withholding: z.string(),
  }),
  note: z.string(),
  created_at: z.string(),
  created_by: z.string(),
});
export type PayrollRunRow = z.infer<typeof payrollRunRow>;

export const payrollSummary = z.object({
  period: z.string(),
  run: payrollRunRow.nullable(),
  runs: z.array(payrollRunRow),
  periods: z.array(z.string()),
  hr_posted_total: z.string(),
  gl_balance: z.string(),
  difference: z.string(),
  reconciled: z.boolean(),
  not_yet_posted: z.string(),
  detail_visible: z.boolean(),
  detail_op: z.string(),
  detail_note: z.string(),
  actor_role: z.string(),
  next_period: z.string().nullable(),
  suggested_pay_on: z.string(),
  employer_in_ledger: z.boolean(),
  employer_note: z.string(),
});
export type PayrollSummary = z.infer<typeof payrollSummary>;

export const payslipRow = z.object({
  emp_no: z.string(),
  name: z.string(),
  title: z.string(),
  dept_label: z.string().nullable(),
  base_salary: z.string(),
  overtime: z.string(),
  allowance: z.string(),
  gross: z.string(),
  labor_insured_salary: z.string(),
  health_insured_salary: z.string(),
  pension_wage: z.string(),
  labor_employee: z.string(),
  health_employee: z.string(),
  income_tax: z.string(),
  deduction: z.string(),
  net: z.string(),
  labor_employer: z.string(),
  health_employer: z.string(),
  pension_employer: z.string(),
  employer_total: z.string(),
  paid_days: z.string(),
  month_days: z.string(),
  prorated: z.boolean(),
  note: z.string(),
  recompute_matches: z.boolean(),
});
export type PayslipRow = z.infer<typeof payslipRow>;

export const payslipList = z.object({
  period: z.string(),
  run: payrollRunRow,
  rows: z.array(payslipRow),
  recompute_all_match: z.boolean(),
  recompute_note: z.string(),
  periods: z.array(z.string()),
});
export type PayslipList = z.infer<typeof payslipList>;

export const payrollRunResult = z.object({
  run: payrollRunRow,
  voucher: voucher,
  skipped: z.array(z.object({ emp_no: z.string(), name: z.string(), reason: z.string() })),
  warnings: z.array(z.object({ emp_no: z.string(), name: z.string(), issue: z.string() })),
  deduction_breakdown: z.object({ labor_employee: z.string(), health_employee: z.string(), income_tax: z.string() }),
  withholding: z.object({
    income_code: z.string(),
    income_name: z.string(),
    rate: z.string(),
    rate_percent: z.string(),
    note: z.string(),
    ruleset: rulesetRef,
  }),
  employer_in_ledger: z.boolean(),
  employer_note: z.string(),
});
export type PayrollRunResult = z.infer<typeof payrollRunResult>;

export const insuranceRow = z.object({
  emp_no: z.string(),
  name: z.string(),
  dept_label: z.string().nullable(),
  title: z.string(),
  status: z.string(),
  labor_insured_salary: z.string(),
  labor_charged_on: z.string(),
  labor_capped: z.boolean(),
  labor_employer: z.string(),
  labor_employee: z.string(),
  labor_government: z.string(),
  health_insured_salary: z.string(),
  health_employer: z.string(),
  health_employee: z.string(),
  pension_wage: z.string(),
  pension_employer: z.string(),
  employer_total: z.string(),
  employee_total: z.string(),
  labor_bracket_missing: z.boolean(),
  health_bracket_missing: z.boolean(),
  pension_wage_missing: z.boolean(),
  brackets_missing: z.boolean(),
});
export type InsuranceRow = z.infer<typeof insuranceRow>;

export const missingBracket = z.object({
  emp_no: z.string(),
  name: z.string(),
  missing: z.array(z.string()),
});
export type MissingBracket = z.infer<typeof missingBracket>;

export const insuranceNote = z.object({
  title: z.string(),
  body: z.string(),
  source: z.string(),
  version: z.string(),
});
export type InsuranceNote = z.infer<typeof insuranceNote>;

export const insuranceSummary = z.object({
  as_of: z.string(),
  rows: z.array(insuranceRow),
  headcount: z.number(),
  capped_count: z.number(),
  labor_employer: z.string(),
  labor_employee: z.string(),
  health_employer: z.string(),
  health_employee: z.string(),
  pension_employer: z.string(),
  employer_total: z.string(),
  employee_total: z.string(),
  missing_bracket_count: z.number(),
  missing_brackets: z.array(missingBracket),
  missing_bracket_note: z.string().nullable(),
  notes: z.array(insuranceNote),
  rulesets: z.object({ labor: rulesetRef, health: rulesetRef, pension: rulesetRef }),
  posted_to_ledger: z.boolean(),
  posting_note: z.string(),
});
export type InsuranceSummary = z.infer<typeof insuranceSummary>;

export const insuranceQuote = z.object({
  as_of: z.string(),
  insured_salary: z.string(),
  labor_charged_on: z.string(),
  labor_capped: z.boolean(),
  labor: z.object({
    employer: z.string(),
    employee: z.string(),
    government: z.string(),
    rate_percent: z.string(),
    max_insured_salary: z.string(),
    ruleset: rulesetRef,
  }),
  health: z.object({
    insured_salary: z.string(),
    employer: z.string(),
    employee: z.string(),
    government: z.string(),
    rate_percent: z.string(),
    avg_dependents: z.string(),
    ruleset: rulesetRef,
  }),
  pension: z.object({
    contribution_wage: z.string(),
    employer: z.string(),
    rate_percent: z.string(),
    ruleset: rulesetRef,
  }),
  employer_total: z.string(),
  employee_total: z.string(),
});
export type InsuranceQuote = z.infer<typeof insuranceQuote>;

export const complianceFailure = z.object({
  emp_no: z.string(),
  name: z.string(),
  message: z.string(),
});
export type ComplianceFailure = z.infer<typeof complianceFailure>;

export const complianceCheck = z.object({
  name: z.string(),
  checked: z.boolean(),
  passed: z.boolean().nullable(),
  rulesets: z.array(rulesetRef),
  failures: z.array(complianceFailure),
  scope_note: z.string().optional(),
  not_checked_reason: z.string().optional(),
  required_domain: z.string().optional(),
  evidence: z.object({
    employee_count: z.number(),
    labor_employer: z.string(),
    labor_employee: z.string(),
    health_employer: z.string(),
    health_employee: z.string(),
    pension_employer: z.string(),
  }).optional(),
});
export type ComplianceCheck = z.infer<typeof complianceCheck>;

export const hrCompliance = z.object({
  as_of: z.string(),
  employee_count: z.number(),
  checked_all_passed: z.boolean(),
  coverage_complete: z.boolean(),
  checked_count: z.number(),
  unchecked_count: z.number(),
  failure_count: z.number(),
  checks: z.array(complianceCheck),
  available_domains: z.array(z.string()),
  coverage_note: z.string(),
  missing_bracket_count: z.number(),
});
export type HrCompliance = z.infer<typeof hrCompliance>;

export const withholdingStatementRecord = z.object({
  paid_on: z.string(),
  gross_amount: z.string(),
  withheld_amount: z.string().nullable(),
  basis: z.string(),
  basis_label: z.string(),
  rate: z.string().nullable(),
  amount_source: z.string(),
  memo: z.string(),
  voucher_no: z.string().nullable(),
  ruleset_version: z.string(),
  warning: z.string().nullable(),
});
export type WithholdingStatementRecord = z.infer<typeof withholdingStatementRecord>;

export const withholdingIncomeLine = z.object({
  income_code: z.string(),
  income_name: z.string(),
  gross_amount: z.string(),
  withheld_amount: z.string().nullable(),
  known_withheld_amount: z.string(),
  withheld_complete: z.boolean(),
  payment_count: z.number(),
  bases: z.array(z.string()),
  basis_labels: z.array(z.string()),
  ruleset_versions: z.array(z.string()),
  records: z.array(withholdingStatementRecord),
});
export type WithholdingIncomeLine = z.infer<typeof withholdingIncomeLine>;

export const withholdingStatement = z.object({
  payee: z.string(),
  payee_id: z.string(),
  person_specific: z.boolean(),
  gross_amount: z.string(),
  withheld_amount: z.string().nullable(),
  known_withheld_amount: z.string(),
  withheld_complete: z.boolean(),
  payment_count: z.number(),
  income_lines: z.array(withholdingIncomeLine),
  source_note: z.string(),
});
export type WithholdingStatement = z.infer<typeof withholdingStatement>;

export const employeeIncomeRow = z.object({
  emp_no: z.string(),
  name: z.string(),
  payee_id: z.string(),
  gross_amount: z.string(),
  withheld_amount: z.string(),
  payroll_months: z.number(),
  source: z.string(),
});
export type EmployeeIncomeRow = z.infer<typeof employeeIncomeRow>;

export const withholdingStatements = z.object({
  year: z.number(),
  from: z.string(),
  to: z.string(),
  statements: z.array(withholdingStatement),
  statement_count: z.number(),
  person_specific_count: z.number(),
  gross_total: z.string(),
  withheld_total: z.string().nullable(),
  known_withheld_total: z.string(),
  withheld_complete: z.boolean(),
  missing_table_amounts: z.number(),
  rulesets: z.array(rulesetRef),
  employee_income: z.array(employeeIncomeRow),
  payroll_reconciliation: z.object({
    payroll_gross: z.string(),
    withholding_record_salary_gross: z.string(),
    gross_difference: z.string(),
    payroll_withheld: z.string(),
    withholding_record_known_salary_withheld: z.string(),
    withheld_difference: z.string().nullable(),
    salary_withheld_complete: z.boolean(),
    reconciled: z.boolean(),
    note: z.string(),
  }),
  table_note: z.string(),
  source_note: z.string(),
});
export type WithholdingStatements = z.infer<typeof withholdingStatements>;

export const myProfile = z.object({
  emp_no: z.string(),
  name: z.string(),
  dept_code: z.string().nullable(),
  dept_label: z.string().nullable(),
  title: z.string(),
  employment_type: z.string(),
  employment_label: z.string(),
  hired_on: z.string(),
  left_on: z.string().nullable(),
  status: z.string(),
  status_label: z.string(),
  service_months: z.number(),
  service_label: z.string(),
  bank_accounts: z.array(z.object({
    bank_name: z.string(),
    account_masked: z.string(),
    is_payroll: z.boolean(),
    note: z.string(),
  })),
  privacy_note: z.string(),
});
export type MyProfile = z.infer<typeof myProfile>;

export const myAnnualLeave = z.object({
  year: z.number(),
  entitlement: z.string(),
  taken: z.string(),
  remaining: z.string(),
  ruleset: rulesetRef,
  quota_stored: z.boolean(),
  note: z.string(),
});
export type MyAnnualLeave = z.infer<typeof myAnnualLeave>;

export const myLeaveRequest = z.object({
  leave_no: z.string(),
  kind: z.string(),
  kind_label: z.string(),
  from_at: z.string(),
  to_at: z.string(),
  from_on: z.string(),
  to_on: z.string(),
  hours: z.string(),
  days: z.string(),
  reason: z.string(),
  status: z.string(),
  status_label: z.string(),
  decided_by: z.string(),
  decided_role: z.string(),
  decided_at: z.string().nullable(),
  decision_note: z.string(),
  created_at: z.string(),
  created_by: z.string(),
});
export type MyLeaveRequest = z.infer<typeof myLeaveRequest>;

export const myPayslip = z.object({
  period: z.string(),
  pay_on: z.string(),
  base_salary: z.string(),
  overtime: z.string(),
  allowance: z.string(),
  gross: z.string(),
  deductions: z.object({
    labor_insurance: z.string(),
    health_insurance: z.string(),
    income_tax: z.string(),
    total: z.string(),
  }),
  net: z.string(),
  employer_contributions: z.object({
    labor_insurance: z.string(),
    health_insurance: z.string(),
    pension: z.string(),
    total: z.string(),
  }),
  paid_days: z.string(),
  month_days: z.string(),
  prorated: z.boolean(),
  rulesets: z.object({
    labor_insurance: z.string(),
    health_insurance: z.string(),
    pension: z.string(),
    withholding: z.string(),
  }),
  note: z.string(),
});
export type MyPayslip = z.infer<typeof myPayslip>;

export const myHome = z.object({
  profile: myProfile,
  annual_leave: myAnnualLeave,
  submissions: z.object({ leave_requests: z.array(myLeaveRequest), expense_claims: z.array(expenseClaim) }),
  pending: z.object({ leave_requests: z.number(), expense_claims: z.number() }),
  identity_note: z.string(),
});
export type MyHome = z.infer<typeof myHome>;

export const myPayslipList = z.object({
  employee: z.object({
    emp_no: z.string(),
    name: z.string(),
    dept_label: z.string().nullable(),
    title: z.string(),
  }),
  payslips: z.array(myPayslip),
  periods: z.array(z.string()),
  gross_total: z.string(),
  net_total: z.string(),
  scope_note: z.string(),
});
export type MyPayslipList = z.infer<typeof myPayslipList>;

export const workSchedule = z.object({
  work_start: z.string().nullable(),
  work_end: z.string().nullable(),
  break_start: z.string().nullable(),
  break_end: z.string().nullable(),
  grace_minutes: z.number(),
  grace_stated: z.boolean(),
  flex_minutes: z.number(),
  flexible: z.boolean(),
  latest_start: z.string().nullable(),
  judges_lateness: z.boolean(),
  counts_overtime: z.boolean(),
  source: z.string(),
  note: z.string(),
});
export type WorkSchedule = z.infer<typeof workSchedule>;

export const myLeaveList = z.object({
  employee: z.object({ emp_no: z.string(), name: z.string() }),
  requests: z.array(myLeaveRequest),
  leave_kinds: z.array(z.object({ kind: z.string(), label: z.string() })),
  schedule: workSchedule,
  pending_count: z.number(),
  annual_leave: myAnnualLeave,
  scope_note: z.string(),
});
export type MyLeaveList = z.infer<typeof myLeaveList>;

export const overtimeDayType = z.object({
  code: z.string(),
  label: z.string(),
  max_hours: z.string(),
});
export type OvertimeDayType = z.infer<typeof overtimeDayType>;

export const overtimeRequestRow = z.object({
  request_no: z.string(),
  emp_no: z.string(),
  name: z.string(),
  dept_label: z.string().nullable(),
  day_type: z.string(),
  day_type_label: z.string(),
  from_at: z.string(),
  to_at: z.string(),
  worked_on: z.string(),
  hours: z.string(),
  pay: z.string().nullable(),
  pay_blocked_reason: z.string().nullable(),
  reason: z.string(),
  status: z.string(),
  status_label: z.string(),
  decided_by: z.string(),
  decided_role: z.string(),
  decided_at: z.string().nullable(),
  decision_note: z.string(),
  created_at: z.string(),
  created_by: z.string(),
  can_decide: z.boolean().optional(),
  decide_blocked_reason: z.string().nullable().optional(),
});
export type OvertimeRequestRow = z.infer<typeof overtimeRequestRow>;

export const noticeRow = z.object({
  id: z.number(),
  kind: z.string(),
  kind_label: z.string(),
  title: z.string(),
  body: z.string(),
  effective_on: z.string(),
  expires_on: z.string().nullable(),
  status: z.string(),
  status_label: z.string(),
  version_no: z.number(),
  versions: z.number(),
  published_at: z.string(),
  published_by: z.string(),
  unread: z.boolean().optional(),
});
export type NoticeRow = z.infer<typeof noticeRow>;

export const noticeRevision = z.object({ revised: z.boolean(), notice: noticeRow });
export type NoticeRevision = z.infer<typeof noticeRevision>;

export const noticeVersionRow = z.object({
  version_no: z.number(),
  title: z.string(),
  body: z.string(),
  effective_on: z.string(),
  published_at: z.string(),
  published_by: z.string(),
});
export type NoticeVersionRow = z.infer<typeof noticeVersionRow>;

export const noticeList = z.object({
  notices: z.array(noticeRow),
  kinds: z.array(z.object({ kind: z.string(), label: z.string() })),
  announcement_count: z.number(),
  policy_count: z.number(),
});
export type NoticeList = z.infer<typeof noticeList>;

export const myNoticeList = z.object({
  notices: z.array(noticeRow),
  unread_count: z.number(),
  read_at: z.string().nullable(),
  scope_note: z.string(),
});
export type MyNoticeList = z.infer<typeof myNoticeList>;

export const noticeHistory = z.object({
  notice: noticeRow,
  versions: z.array(noticeVersionRow),
});
export type NoticeHistory = z.infer<typeof noticeHistory>;

export const myOvertimeList = z.object({
  employee: z.object({ emp_no: z.string(), name: z.string() }),
  requests: z.array(overtimeRequestRow),
  pending_count: z.number(),
  month: z.string(),
  month_approved_hours: z.string(),
  monthly_max_hours: z.string(),
  month_hours_left: z.string(),
  day_types: z.array(overtimeDayType),
  schedule: workSchedule,
  ruleset: rulesetRef,
  scope_note: z.string(),
});
export type MyOvertimeList = z.infer<typeof myOvertimeList>;

export const myOvertimeResult = z.object({
  request: overtimeRequestRow,
  status: z.string(),
  month_hours_after: z.string(),
  monthly_max_hours: z.string(),
  ruleset: rulesetRef,
  scope_note: z.string(),
});
export type MyOvertimeResult = z.infer<typeof myOvertimeResult>;

export const overtimeMonthTotal = z.object({
  emp_no: z.string(),
  name: z.string(),
  dept_label: z.string().nullable(),
  hours: z.string(),
  pay: z.string(),
  over_ceiling: z.boolean(),
});
export type OvertimeMonthTotal = z.infer<typeof overtimeMonthTotal>;

export const overtimeRequestList = z.object({
  month: z.string(),
  requests: z.array(overtimeRequestRow),
  pending_count: z.number(),
  other_month_pending: z.number(),
  month_totals: z.array(overtimeMonthTotal),
  monthly_max_hours: z.string(),
  day_types: z.array(overtimeDayType),
  ruleset: rulesetRef,
});
export type OvertimeRequestList = z.infer<typeof overtimeRequestList>;

export const myLeaveResult = z.object({
  request: myLeaveRequest,
  status: z.string(),
  schedule: workSchedule,
  hours_source: z.string(),
  scope_note: z.string(),
});
export type MyLeaveResult = z.infer<typeof myLeaveResult>;

export const expenseReceipt = z.object({
  doc_no: z.string(),
  file_name: z.string(),
  byte_size: z.number(),
  size_label: z.string(),
  content_type: z.string(),
  checksum_short: z.string(),
  uploaded_at: z.string(),
});
export type ExpenseReceipt = z.infer<typeof expenseReceipt>;

export const myExpenseClaim = expenseClaim.extend({
  receipts: z.array(expenseReceipt),
  receipt_count: z.number(),
  accepts_receipt: z.boolean(),
});
export type MyExpenseClaim = z.infer<typeof myExpenseClaim>;

export const myExpenseClaimList = z.object({
  employee: z.object({ emp_no: z.string(), name: z.string() }),
  claims: z.array(myExpenseClaim),
  waiting_count: z.number(),
  waiting_total: z.string(),
  scope_note: z.string(),
});
export type MyExpenseClaimList = z.infer<typeof myExpenseClaimList>;

export const attachReceiptResult = z.object({
  claim_no: z.string(),
  receipt: expenseReceipt,
  receipts: z.array(expenseReceipt),
  scope_note: z.string(),
});
export type AttachReceiptResult = z.infer<typeof attachReceiptResult>;

export const punchDay = z.object({
  date: z.string(),
  weekday: z.string(),
  in_at: z.string().nullable(),
  out_at: z.string().nullable(),
  in_source: z.string().nullable(),
  out_source: z.string().nullable(),
  manual: z.boolean(),
  worked_seconds: z.number().nullable(),
  worked_hours: z.string().nullable(),
  open: z.boolean(),
  late: z.boolean().nullable(),
  late_seconds: z.number().nullable(),
  expected_end: z.string().nullable(),
  overtime_seconds: z.number().nullable(),
  day_type_label: z.string().nullable(),
  overtime_limit_hours: z.string().nullable(),
  over_overtime_limit: z.boolean(),
  note: z.string(),
});
export type PunchDay = z.infer<typeof punchDay>;

export const punchSummary = z.object({
  days_with_arrival: z.number(),
  complete_days: z.number(),
  open_days: z.number(),
  worked_seconds: z.number(),
  worked_hours: z.string(),
  late_days: z.number().nullable(),
  overtime_seconds: z.number().nullable(),
  overtime_hours: z.string().nullable(),
  manual_days: z.number(),
  over_limit_days: z.number(),
  over_monthly_limit: z.boolean(),
  monthly_max_hours: z.string(),
  overtime_pay_computed: z.boolean(),
  overtime_note: z.string(),
});
export type PunchSummary = z.infer<typeof punchSummary>;

export const myPunchList = z.object({
  employee: z.object({ emp_no: z.string(), name: z.string() }),
  month: z.string(),
  today: z.string(),
  today_punches: punchDay.nullable(),
  next_kind: z.enum(['in', 'out']).nullable(),
  next_kind_label: z.string().nullable(),
  days: z.array(punchDay),
  summary: punchSummary,
  schedule: workSchedule,
  scope_note: z.string(),
});
export type MyPunchList = z.infer<typeof myPunchList>;

export const punchResult = z.object({
  punch: z.object({
    punched_on: z.string(),
    at: z.string(),
    kind: z.enum(['in', 'out']),
    kind_label: z.string(),
    source: z.string(),
  }),
  today: punchDay.nullable(),
  schedule: workSchedule,
  scope_note: z.string(),
});
export type PunchResult = z.infer<typeof punchResult>;

export const punchCorrection = z.object({
  id: z.number(),
  emp_no: z.string(),
  name: z.string(),
  dept_label: z.string().nullable(),
  punched_on: z.string(),
  kind: z.enum(['in', 'out']),
  kind_label: z.string(),
  at: z.string(),
  reason: z.string(),
  status: z.enum(['submitted', 'approved', 'rejected']),
  status_label: z.string(),
  decided_by: z.string(),
  decided_role: z.string(),
  decided_at: z.string().nullable(),
  decision_note: z.string(),
  replaced_at: z.string().nullable(),
  hr_entered: z.boolean(),
  created_by: z.string(),
  created_at: z.string(),
  can_decide: z.boolean().optional(),
  decide_blocked_reason: z.string().nullable().optional(),
});
export type PunchCorrection = z.infer<typeof punchCorrection>;

export const myPunchCorrectionList = z.object({
  corrections: z.array(punchCorrection),
  waiting_count: z.number(),
  auto_approve: z.boolean(),
  scope_note: z.string(),
});
export type MyPunchCorrectionList = z.infer<typeof myPunchCorrectionList>;

export const punchCorrectionList = z.object({
  corrections: z.array(punchCorrection),
  waiting_count: z.number(),
  auto_approve: z.boolean(),
  policy_note: z.string(),
});
export type PunchCorrectionList = z.infer<typeof punchCorrectionList>;

export const punchCorrectionResult = z.object({
  correction: punchCorrection,
  punch_written: z.boolean(),
  next_note: z.string(),
});
export type PunchCorrectionResult = z.infer<typeof punchCorrectionResult>;

export const attendancePunchList = z.object({
  month: z.string(),
  months: z.array(z.string()),
  schedule: workSchedule,
  rows: z.array(z.object({
    emp_no: z.string(),
    name: z.string(),
    dept_label: z.string().nullable(),
    summary: punchSummary,
  })),
  employee: z.object({ emp_no: z.string(), name: z.string() }).nullable(),
  days: z.array(punchDay).nullable(),
  open_days: z.number(),
});
export type AttendancePunchList = z.infer<typeof attendancePunchList>;

export const myMcpTools = z.object({
  usable: z.array(z.string()),
  blocked_for_ai: z.array(z.string()),
  role: z.string(),
  role_name: z.string(),
  note: z.string(),
});
export type MyMcpTools = z.infer<typeof myMcpTools>;

export const myApiToken = z.object({
  id: z.number(),
  name: z.string(),
  scope: z.string(),
  prefix: z.string(),
  created_at: z.string(),
  last_used_at: z.string().nullable(),
  rotate_days: z.number().nullable(),
  expires_at: z.string().nullable(),
  expired: z.boolean(),
  expiry_note: z.string(),
  revoked_at: z.string().nullable(),
  live: z.boolean(),
});
export type MyApiToken = z.infer<typeof myApiToken>;

export const myApiTokenList = z.object({
  employee: z.object({ emp_no: z.string(), name: z.string() }),
  tokens: z.array(myApiToken),
  live_count: z.number(),
  tools: myMcpTools,
  endpoint: z.string(),
  scope_note: z.string(),
});
export type MyApiTokenList = z.infer<typeof myApiTokenList>;

export const myApiTokenIssued = z.object({
  token: z.object({
    id: z.number(),
    name: z.string(),
    prefix: z.string(),
    scope: z.string(),
    expiry_note: z.string(),
  }),
  secret: z.string(),
  tools: myMcpTools,
  secret_note: z.string(),
  scope_note: z.string(),
});
export type MyApiTokenIssued = z.infer<typeof myApiTokenIssued>;

export const myExpenseClaimResult = z.object({
  claim: expenseClaim,
  account_note: z.string(),
  scope_note: z.string(),
});
export type MyExpenseClaimResult = z.infer<typeof myExpenseClaimResult>;

export const STATUS_LABEL: Record<Voucher['status'], string> = {
  draft: '待確認',
  pending: '待過帳',
  posted: '已過帳',
  void: '已作廢',
};

export function maskId(value: string): string {
  if (value.length < 6) return value;
  return `${value.slice(0, 3)}${'*'.repeat(value.length - 5)}${value.slice(-2)}`;
}

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

const DEFAULT_UTC_OFFSET_MINUTES = 8 * 60;

let businessOffsetMinutes = DEFAULT_UTC_OFFSET_MINUTES;

export function setBusinessOffset(v: string | null | undefined): void {
  if (!v) return;
  const m = /^([+-])(\d{2}):(\d{2})$/.exec(v);
  if (!m) return;
  const minutes = Number(m[2]) * 60 + Number(m[3]);
  businessOffsetMinutes = m[1] === '-' ? -minutes : minutes;
}

let serverSkewMs = 0;

export function noteServerTime(res: Response): void {
  const header = res.headers.get('date');
  if (!header) return;
  const stated = Date.parse(header);
  if (Number.isNaN(stated)) return;
  const skew = stated + 500 - Date.now();
  if (Math.abs(skew - serverSkewMs) > 2000) serverSkewMs = skew;
}

export function serverNow(): Date {
  return new Date(Date.now() + serverSkewMs);
}

export function serverClockGapMs(): number {
  return serverSkewMs;
}

function atBusinessOffset(at: Date): Date {
  return new Date(at.getTime() + businessOffsetMinutes * 60_000);
}

function shiftToBusiness(v: string): Date | null {
  const t = new Date(v);
  if (Number.isNaN(t.getTime())) return null;
  return atBusinessOffset(t);
}

const two = (n: number) => String(n).padStart(2, '0');

export function businessClock(): string {
  const at = atBusinessOffset(serverNow());
  return `${two(at.getUTCHours())}:${two(at.getUTCMinutes())}:${two(at.getUTCSeconds())}`;
}

export function businessToday(): string {
  const at = atBusinessOffset(serverNow());
  return `${at.getUTCFullYear()}-${two(at.getUTCMonth() + 1)}-${two(at.getUTCDate())}`;
}

export function businessTime(v: string | null | undefined, fallback = '—'): string {
  if (!v) return fallback;
  const shifted = shiftToBusiness(v);
  if (!shifted) return v;
  return `${two(shifted.getUTCMonth() + 1)}/${two(shifted.getUTCDate())} ${two(
    shifted.getUTCHours(),
  )}:${two(shifted.getUTCMinutes())}`;
}

export function docNumberHint(prefix: string): string {
  const now = atBusinessOffset(serverNow());
  return `例如 ${prefix}${String(now.getUTCFullYear()).slice(2)}${two(
    now.getUTCMonth() + 1,
  )}-001`;
}

export function businessDate(v: string | null | undefined, fallback = '—'): string {
  if (!v) return fallback;
  const shifted = shiftToBusiness(v);
  if (!shifted) return v.slice(0, 10);
  return `${shifted.getUTCFullYear()}-${two(shifted.getUTCMonth() + 1)}-${two(
    shifted.getUTCDate(),
  )}`;
}

export function businessStamp(v: string | null | undefined, fallback = ''): string {
  if (!v) return fallback;
  const shifted = shiftToBusiness(v);
  if (!shifted) return v;
  return `${businessDate(v)} ${two(shifted.getUTCHours())}:${two(
    shifted.getUTCMinutes(),
  )}:${two(shifted.getUTCSeconds())}`;
}

export function wallSpan(from: string, to: string): string {
  const [fromDay = '', fromTime = ''] = from.split('T');
  const [toDay = '', toTime = ''] = to.split('T');
  const hm = (t: string) => t.slice(0, 5);
  return fromDay === toDay
    ? `${fromDay} ${hm(fromTime)} 至 ${hm(toTime)}`
    : `${fromDay} ${hm(fromTime)} 至 ${toDay} ${hm(toTime)}`;
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
