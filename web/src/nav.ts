export type ScreenId =
  | 'dash'
  | 'gl'
  | 'ar'
  | 'ap'
  | 'bank'
  | 'tax'
  | 'exp'
  | 'fa'
  | 'budget'
  | 'rep'
  | 'doc'
  | 'imp'

  | 'setup'
  | 'acc'
  | 'roles'
  | 'mcp'
  | 'sec'
  | 'reg'

  | 'hrdash'
  | 'hr'
  | 'att'
  | 'pay'
  | 'ins'
  | 'wht'
  | 'notice'
  | 'cal'

  | 'me'
  | 'meclock'
  | 'meleavenew'
  | 'meotnew'
  | 'meexpnew'
  | 'meexp'
  | 'mepay'
  | 'meleave'
  | 'meot'
  | 'memcp'
  | 'menotice';

export interface NavItem {
  kind: 'item';
  id: ScreenId;
  label: string;
  op: string;
  ready?: boolean;
  badge?: 'pending' | 'draft' | 'unread';
}

export interface NavHead {
  kind: 'head';
  label: string;
}

export type NavEntry = NavItem | NavHead;

export const FINANCE_NAV: NavEntry[] = [
  { kind: 'head', label: '日常' },
  { kind: 'item', id: 'dash', label: '總覽', op: 'get_dashboard', ready: true },
  { kind: 'item', id: 'gl', label: '傳票 · 總帳', op: 'search_vouchers', ready: true, badge: 'pending' },
  { kind: 'head', label: '收付' },
  { kind: 'item', id: 'ar', label: '應收帳款', op: 'get_ar_aging', ready: true },
  { kind: 'item', id: 'ap', label: '應付帳款', op: 'list_payables', ready: true },
  { kind: 'item', id: 'bank', label: '銀行與現金', op: 'get_cash_position', ready: true },
  { kind: 'head', label: '費用' },
  { kind: 'item', id: 'exp', label: '費用報支', op: 'list_expense_claims', ready: true },
  { kind: 'item', id: 'budget', label: '預算與管理', op: 'get_budget_status', ready: true },
  { kind: 'head', label: '結帳與申報' },
  { kind: 'item', id: 'rep', label: '財務報表', op: 'get_balance_sheet', ready: true },
  { kind: 'item', id: 'tax', label: '稅務 401 / 403', op: 'get_vat_return', ready: true },
  { kind: 'item', id: 'fa', label: '固定資產', op: 'list_fixed_assets', ready: true },
  { kind: 'head', label: '資料' },
  { kind: 'item', id: 'doc', label: '文件管理', op: 'list_documents', ready: true },
  { kind: 'item', id: 'imp', label: '資料匯入', op: 'list_imports', ready: true },
];

export const SYSTEM_NAV: NavEntry[] = [
  { kind: 'head', label: '設定' },
  { kind: 'item', id: 'setup', label: '基礎設定', op: 'get_company_settings', ready: true },
  { kind: 'item', id: 'acc', label: '帳號管理', op: 'list_user_accounts', ready: true },
  { kind: 'item', id: 'roles', label: '角色與核准', op: 'list_roles', ready: true },
  { kind: 'item', id: 'reg', label: '法規版本', op: 'list_regulations', ready: true },
  { kind: 'head', label: 'AI 與監控' },
  { kind: 'item', id: 'mcp', label: 'MCP 與 AI', op: 'list_ai_audit', ready: true },
  { kind: 'item', id: 'sec', label: '安全與營運', op: 'get_security_policy', ready: true },
];

export const HR_NAV: NavEntry[] = [
  { kind: 'head', label: '每月作業' },
  { kind: 'item', id: 'hrdash', label: '人資總覽', op: 'get_hr_dashboard', ready: true },
  { kind: 'item', id: 'att', label: '出勤與休假', op: 'list_attendance_punches', ready: true },
  { kind: 'item', id: 'cal', label: '工作日曆', op: 'list_work_calendar', ready: true },
  { kind: 'item', id: 'pay', label: '薪資作業', op: 'get_payroll_summary', ready: true },
  { kind: 'item', id: 'ins', label: '勞健保與勞退', op: 'get_insurance_summary', ready: true },
  { kind: 'head', label: '人員' },
  { kind: 'item', id: 'hr', label: '員工名冊', op: 'list_employees', ready: true },
  { kind: 'item', id: 'notice', label: '公告與規章', op: 'list_notices', ready: true },
  { kind: 'head', label: '法遵' },
  { kind: 'item', id: 'wht', label: '扣繳與法遵', op: 'get_hr_compliance', ready: true },
];

export const ME_NAV: NavEntry[] = [
  { kind: 'head', label: '我的' },

  { kind: 'item', id: 'meclock', label: '打卡', op: 'punch_clock', ready: true },
  { kind: 'item', id: 'me', label: '個人專區', op: 'get_my_home', ready: true },
  {
    kind: 'item',
    id: 'menotice',
    label: '公告與規章',
    op: 'list_my_notices',
    ready: true,
    badge: 'unread',
  },
  { kind: 'head', label: '提出申請' },

  { kind: 'item', id: 'meleavenew', label: '休假申請', op: 'request_leave', ready: true },
  {
    kind: 'item',
    id: 'meotnew',
    label: '加班申請',
    op: 'submit_my_overtime_request',
    ready: true,
  },
  {
    kind: 'item',
    id: 'meexpnew',
    label: '請款申請',
    op: 'submit_my_expense_claim',
    ready: true,
  },
  { kind: 'head', label: '查詢紀錄' },
  { kind: 'item', id: 'meleave', label: '我的休假', op: 'list_my_leave_requests', ready: true },
  {
    kind: 'item',
    id: 'meot',
    label: '我的加班',
    op: 'list_my_overtime_requests',
    ready: true,
  },
  { kind: 'item', id: 'meexp', label: '我的請款', op: 'list_my_expense_claims', ready: true },
  { kind: 'item', id: 'mepay', label: '我的薪資單', op: 'list_my_payslips', ready: true },
  { kind: 'head', label: '連線' },
  { kind: 'item', id: 'memcp', label: '我的 MCP', op: 'list_my_api_tokens', ready: true },
];

export function visibleEntries(entries: NavEntry[], permissions: string[]): NavEntry[] {
  const allowed = new Set(permissions);
  const kept = entries.filter((e) => e.kind === 'head' || allowed.has(e.op));
  return kept.filter(
    (e, i) => e.kind !== 'head' || kept[i + 1]?.kind === 'item',
  );
}

export function firstScreen(entries: NavEntry[], permissions: string[]): ScreenId | null {
  const visible = visibleEntries(entries, permissions);
  const item = visible.find((e): e is NavItem => e.kind === 'item');
  return item ? item.id : null;
}

export const SCREEN_PATH: Record<ScreenId, string> = {
  meclock: '/me/clock',
  me: '/me',
  meleavenew: '/me/leave/new',
  meotnew: '/me/overtime/new',
  meexpnew: '/me/expense/new',
  meleave: '/me/leave',
  meot: '/me/overtime',
  meexp: '/me/expense',
  mepay: '/me/payslip',
  memcp: '/me/mcp',
  menotice: '/me/notices',

  dash: '/finance',
  gl: '/finance/ledger',
  ar: '/finance/receivables',
  ap: '/finance/payables',
  bank: '/finance/bank',
  exp: '/finance/expenses',
  budget: '/finance/budget',
  rep: '/finance/reports',
  tax: '/finance/tax',
  fa: '/finance/assets',
  doc: '/finance/documents',
  imp: '/finance/imports',

  hrdash: '/hr',
  att: '/hr/attendance',
  pay: '/hr/payroll',
  ins: '/hr/insurance',
  hr: '/hr/employees',
  wht: '/hr/withholding',
  notice: '/hr/notices',
  cal: '/hr/calendar',

  setup: '/system/settings',
  acc: '/system/accounts',
  roles: '/system/roles',
  mcp: '/system/mcp',
  sec: '/system/security',
  reg: '/system/regulations',
};

const PATH_SCREEN: Record<string, ScreenId> = Object.fromEntries(
  Object.entries(SCREEN_PATH).map(([id, path]) => [path, id as ScreenId]),
);

export function screenForPath(path: string): ScreenId | null {
  const clean = path.replace(/\/+$/, '') || '/';
  return PATH_SCREEN[clean] ?? null;
}

const ALL_NAV: NavEntry[] = [...FINANCE_NAV, ...HR_NAV, ...ME_NAV, ...SYSTEM_NAV];

export function screenOp(id: ScreenId): string | null {
  const item = ALL_NAV.find((e): e is NavItem => e.kind === 'item' && e.id === id);
  return item ? item.op : null;
}

export function screenTitle(id: ScreenId): string {
  const item = ALL_NAV.find((e): e is NavItem => e.kind === 'item' && e.id === id);
  return item ? item.label : '首頁';
}
