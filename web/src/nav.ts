/**
 * Sidebar navigation structure.
 *
 * The information architecture shows the whole system rather than only the
 * screens that are finished. An unimplemented entry explains which stage it
 * belongs to instead of disappearing or offering a broken link.
 */

export type ScreenId =
  | 'dash'
  | 'gl'
  | 'ar'
  | 'ap'
  | 'bank'
  | 'inv'
  | 'tax'
  | 'exp'
  | 'fa'
  | 'budget'
  | 'rep'
  | 'doc'
  | 'imp'
  | 'setup'
  | 'roles'
  | 'mcp'
  | 'sec'
  // HR
  | 'hrdash'
  | 'hr'
  | 'onb'
  | 'att'
  | 'pay'
  | 'ins'
  | 'wht'
  // Employee self-service
  | 'me'
  | 'meexp'
  | 'mepay'
  | 'meleave';

export interface NavItem {
  kind: 'item';
  id: ScreenId;
  label: string;
  /** Implemented. Others show a placeholder note. */
  ready?: boolean;
  /** Which count the badge shows, decided by the screen's data. */
  badge?: 'pending' | 'draft';
}

export interface NavHead {
  kind: 'head';
  label: string;
}

export type NavEntry = NavItem | NavHead;

export const FINANCE_NAV: NavEntry[] = [
  { kind: 'head', label: '日常' },
  { kind: 'item', id: 'dash', label: '總覽', ready: true },
  { kind: 'item', id: 'gl', label: '傳票 · 總帳', ready: true, badge: 'pending' },
  { kind: 'head', label: '收付' },
  { kind: 'item', id: 'ar', label: '應收帳款', ready: true },
  { kind: 'item', id: 'ap', label: '應付帳款', ready: true },
  { kind: 'item', id: 'bank', label: '銀行與現金', ready: true },
  { kind: 'head', label: '稅務' },
  { kind: 'item', id: 'inv', label: '電子發票', ready: true },
  { kind: 'item', id: 'tax', label: '稅務 401 / 403', ready: true },
  { kind: 'head', label: '費用與資產' },
  { kind: 'item', id: 'exp', label: '費用報支', ready: true },
  { kind: 'item', id: 'fa', label: '固定資產', ready: true },
  { kind: 'item', id: 'budget', label: '預算與管理', ready: true },
  { kind: 'head', label: '報表與文件' },
  { kind: 'item', id: 'rep', label: '財務報表', ready: true },
  { kind: 'item', id: 'doc', label: '文件管理', ready: true },
  { kind: 'item', id: 'imp', label: '匯入與整合', ready: true },
  { kind: 'head', label: '基礎與系統' },
  { kind: 'item', id: 'setup', label: '基礎設定', ready: true },
  { kind: 'item', id: 'roles', label: '角色與核准', ready: true },
  { kind: 'item', id: 'mcp', label: 'MCP 與 AI', ready: true },
  { kind: 'item', id: 'sec', label: '安全與營運', ready: true },
];

/**
 * The HR navigation tree.
 *
 * Two trees rather than one long one: accounting works in finance all day
 * and HR works here, and stacking 24 entries into one column would make
 * both scroll further. Switching changes the tree, not the identity.
 */
export const HR_NAV: NavEntry[] = [
  { kind: 'head', label: '人員' },
  { kind: 'item', id: 'hrdash', label: '人資總覽', ready: true },
  { kind: 'item', id: 'hr', label: '員工名冊', ready: true },
  { kind: 'item', id: 'onb', label: '到離職與異動', ready: true },
  { kind: 'head', label: '出勤' },
  { kind: 'item', id: 'att', label: '出勤與請假', ready: true },
  { kind: 'head', label: '薪酬' },
  { kind: 'item', id: 'pay', label: '薪資作業', ready: true },
  { kind: 'item', id: 'ins', label: '勞健保與勞退', ready: true },
  { kind: 'head', label: '法遵' },
  { kind: 'item', id: 'wht', label: '扣繳與法遵', ready: true },
];

/**
 * The employee self-service tree.
 *
 * A third tree. None of the five operations behind these four screens has
 * an input field that can name another person; the subject always comes
 * from `ctx.actor`. It therefore stays out of the finance and HR trees: a
 * staff member's sidebar has no reason to list the general ledger or
 * payroll.
 */
export const ME_NAV: NavEntry[] = [
  { kind: 'head', label: '我的' },
  { kind: 'item', id: 'me', label: '我的首頁', ready: true },
  { kind: 'item', id: 'meexp', label: '我的請款', ready: true },
  { kind: 'item', id: 'mepay', label: '我的薪資單', ready: true },
  { kind: 'item', id: 'meleave', label: '我的請假', ready: true },
];

/**
 * Screens not yet implemented, with a note on where they are scheduled.
 *
 * Currently empty: 17 finance, 7 HR and 4 self-service screens are all
 * connected. The type stays so that adding one later needs no structural
 * change.
 */
export const ROADMAP: Partial<Record<ScreenId, string>> = {};
