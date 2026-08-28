import { useCallback, useEffect, useRef, useState } from 'react';
import { BrandMark } from './components/BrandMark';
import { EntryDrawer } from './components/EntryDrawer';
import { SetupWizard } from './components/SetupWizard';
import { Accounts } from './screens/Accounts';
import { Attendance } from './screens/Attendance';
import { Banking } from './screens/Banking';
import { Budget } from './screens/Budget';
import { Clock } from './screens/Clock';
import { Documents } from './screens/Documents';
import { Employees } from './screens/Employees';
import { ExpenseRequest } from './screens/ExpenseRequest';
import { Expenses } from './screens/Expenses';
import { Imports } from './screens/Imports';
import { Init } from './screens/Init';
import { LeaveRequest } from './screens/LeaveRequest';
import { OvertimeRequest } from './screens/OvertimeRequest';
import { Dashboard } from './screens/Dashboard';
import { FixedAssets } from './screens/FixedAssets';
import { GeneralLedger } from './screens/GeneralLedger';
import { HrDashboard } from './screens/HrDashboard';
import { Insurance } from './screens/Insurance';
import { Login } from './screens/Login';
import { McpTools } from './screens/McpTools';
import { Me } from './screens/Me';
import { MyExpenses } from './screens/MyExpenses';
import { MyLeave } from './screens/MyLeave';
import { MyOvertime } from './screens/MyOvertime';
import { MyMcp } from './screens/MyMcp';
import { MyNotices } from './screens/MyNotices';
import { MyPayslips } from './screens/MyPayslips';
import { Notices } from './screens/Notices';
import { Payables } from './screens/Payables';
import { Payroll } from './screens/Payroll';
import { Receivables } from './screens/Receivables';
import { Reports } from './screens/Reports';
import { Roles } from './screens/Roles';
import { Security } from './screens/Security';
import { Regulations } from './screens/Regulations';
import { WorkCalendar } from './screens/WorkCalendar';
import { Setup } from './screens/Setup';
import { Tax } from './screens/Tax';
import { Withholding } from './screens/Withholding';
import {
  FINANCE_NAV,
  firstScreen,
  HR_NAV,
  ME_NAV,
  SCREEN_PATH,
  screenForPath,
  screenOp,
  screenTitle,
  SYSTEM_NAV,
  visibleEntries,
  type NavEntry,
  type ScreenId,
} from './nav';
import { AuthProvider, useAuth, useAuthSession } from './auth';
import {
  callOp,
  getMeta,
  myNoticeList,
  myPunchList,
  OpFailed,
  punchResult,
  voucherList,
  type Meta,
  type MyPunchList,
} from './api/client';

type Section = 'fin' | 'hr' | 'me' | 'sys';

const NAV: Record<Section, NavEntry[]> = {
  fin: FINANCE_NAV,
  hr: HR_NAV,
  me: ME_NAV,
  sys: SYSTEM_NAV,
};
const SECTION_LABEL: Record<Section, string> = {
  fin: '財務',
  hr: '人資',
  me: '我的',
  sys: '系統',
};
const SECTION_ORDER: Section[] = ['me', 'fin', 'hr', 'sys'];
const TAB_SECTIONS: Section[] = ['me', 'fin', 'hr'];

function usableSections(
  permissions: string[],
  employeeId: number | null | undefined,
): { section: Section; screen: ScreenId }[] {
  return SECTION_ORDER.flatMap((section) => {
    if (section === 'me' && employeeId == null) return [];
    const screen = firstScreen(NAV[section], permissions);
    return screen ? [{ section, screen }] : [];
  });
}

function sectionOf(id: ScreenId): Section {
  const found = SECTION_ORDER.find((section) =>
    NAV[section].some((e) => e.kind === 'item' && e.id === id),
  );
  return found ?? 'fin';
}

/// A typed-in address that matches no screen. The root is not one: the first
/// render replaces it with the landing screen's path.
function unknownPath(path: string): string | null {
  const clean = path.replace(/\/+$/, '') || '/';
  return clean === '/' || screenForPath(clean) !== null ? null : path;
}

function screenAllowed(
  id: ScreenId,
  permissions: string[],
  employeeId: number | null | undefined,
): boolean {
  if (sectionOf(id) === 'me' && employeeId == null) return false;
  const op = screenOp(id);
  return op !== null && permissions.includes(op);
}

export function App() {
  const auth = useAuthSession();
  const signedIn = auth.status === 'signed-in' && auth.user !== null;

  return (
    <AuthProvider value={auth}>
      {auth.status === 'loading' ? (
        <div className="login">
          <p className="state-note">確認登入狀態…</p>
        </div>
      ) : signedIn && !auth.user?.must_change_password ? (
        <Workspace />
      ) : !signedIn && auth.setup.bootstrap_available ? (
        <Init />
      ) : (
        <Login />
      )}
    </AuthProvider>
  );
}

function Workspace() {
  const { user, signOut } = useAuth();
  const permissions = user?.permissions ?? [];
  const sections = usableSections(permissions, user?.employee_id);

  const tabs = sections.filter((s) => TAB_SECTIONS.includes(s.section));
  const admin = sections.find((s) => s.section === 'sys');
  const canWriteVouchers =
    permissions.includes('create_voucher') && permissions.includes('list_accounts');
  const canSetUp = permissions.includes('update_company_setting');
  const start = tabs[0] ?? sections[0];
  const landing = start?.screen ?? 'me';
  const [screen, setScreen] = useState<ScreenId>(() => {
    const asked = screenForPath(window.location.pathname);
    return asked && screenAllowed(asked, permissions, user?.employee_id) ? asked : landing;
  });
  const [missingPath, setMissingPath] = useState<string | null>(() =>
    unknownPath(window.location.pathname),
  );
  const section = sectionOf(screen);
  const [meta, setMeta] = useState<Meta | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [drawer, setDrawer] = useState(false);
  const [pendingCount, setPendingCount] = useState(0);
  const [unreadCount, setUnreadCount] = useState(0);
  const [punch, setPunch] = useState<MyPunchList | null>(null);
  const [punching, setPunching] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [showWizard, setShowWizard] = useState(false);

  const [navOpen, setNavOpen] = useState(false);

  const wizardPromptedRef = useRef(false);

  const reload = useCallback(() => setReloadKey((k) => k + 1), []);

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    window.setTimeout(() => setToast((t) => (t === msg ? null : t)), 3200);
  }, []);

  const go = useCallback((id: ScreenId) => {
    setScreen(id);
    setMissingPath(null);
    setNavOpen(false);
    if (window.location.pathname !== SCREEN_PATH[id]) {
      window.history.pushState({ screen: id }, '', SCREEN_PATH[id]);
    }
  }, []);

  const resolvePath = useRef<(path: string) => ScreenId>(() => landing);
  resolvePath.current = (path: string) => {
    const asked = screenForPath(path);
    return asked && screenAllowed(asked, permissions, user?.employee_id) ? asked : landing;
  };

  useEffect(() => {
    if (!missingPath && window.location.pathname !== SCREEN_PATH[screen]) {
      window.history.replaceState({ screen }, '', SCREEN_PATH[screen]);
    }
    const onPop = () => {
      setScreen(resolvePath.current(window.location.pathname));
      setMissingPath(unknownPath(window.location.pathname));
    };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);

  useEffect(() => {
    if (!navOpen) return;
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setNavOpen(false);
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [navOpen]);

  useEffect(() => {
    getMeta()
      .then((m) => {
        setMeta(m);
        if (m.setup_required && canSetUp && !wizardPromptedRef.current) {
          wizardPromptedRef.current = true;
          setShowWizard(true);
        }
      })
      .catch(() => setMeta(null));
  }, [reloadKey, canSetUp]);

  useEffect(() => {
    if (section !== 'me' || !permissions.includes('list_my_notices')) {
      setUnreadCount(0);
      return;
    }
    let live = true;
    callOp('list_my_notices', {}, undefined, myNoticeList)
      .then((d) => live && setUnreadCount(d.unread_count))
      .catch(() => live && setUnreadCount(0));
    return () => {
      live = false;
    };
  }, [reloadKey, section]);

  useEffect(() => {
    if (section !== 'me' || !permissions.includes('list_my_punches')) {
      setPunch(null);
      return;
    }
    let live = true;
    callOp('list_my_punches', {}, undefined, myPunchList)
      .then((d) => live && setPunch(d))
      .catch(() => live && setPunch(null));
    return () => {
      live = false;
    };
  }, [reloadKey, section]);

  async function punchNow(kind: 'in' | 'out') {
    setPunching(true);
    try {
      const d = await callOp('punch_clock', { kind }, kind === 'in' ? '打上班卡' : '打下班卡', punchResult);
      showToast(`${d.punch.kind_label}卡 ${d.punch.at} 已記錄`);
      reload();
    } catch (e) {
      showToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setPunching(false);
    }
  }

  useEffect(() => {
    if (section === 'me' || !permissions.includes('search_vouchers')) {
      setPendingCount(0);
      return;
    }
    Promise.all([
      callOp('search_vouchers', { status: 'pending', limit: 500 }, undefined, voucherList),
      callOp('search_vouchers', { status: 'draft', limit: 500 }, undefined, voucherList),
    ])
      .then(([p, d]) => setPendingCount(p.vouchers.length + d.vouchers.length))
      .catch(() => setPendingCount(0));
  }, [reloadKey, section]);

  function switchSection(next: Section) {
    if (next === section) return;
    const target = sections.find((s) => s.section === next);
    if (target) go(target.screen);
  }

  const headerContext =
    section === 'fin' ? (
      <>
        <div className="header__context">會計期間 {meta?.period ?? '—'}</div>
        <div className="header__state">{meta?.period_state ?? '—'}</div>
      </>
    ) : section === 'me' ? (
      <div className="header__context">{user?.display_name ?? '—'}</div>
    ) : null;

  return (
    <div className="shell">

      {navOpen && <div className="scrim" onClick={() => setNavOpen(false)} />}

      <aside id="laysi-nav" className={`sidebar${navOpen ? ' sidebar--open' : ''}`}>
        <div className="sidebar__brand">

          <BrandMark height={14} />
          <div className="sidebar__company">{meta?.company ?? '　'}</div>

          {tabs.length > 1 && (
            <div className="sidebar__tabs">
              {tabs.map(({ section: s }) => (
                <button
                  key={s}
                  className={`sidebar__tab${section === s ? ' sidebar__tab--on' : ''}`}
                  onClick={() => switchSection(s)}
                >
                  {SECTION_LABEL[s]}
                </button>
              ))}
            </div>
          )}
        </div>

        <nav className="sidebar__nav">
          {visibleEntries(NAV[section], permissions).map((e, i) =>
            e.kind === 'head' ? (
              <div className="nav-head" key={`h${i}`}>
                {e.label}
              </div>
            ) : (
              <button
                key={e.id}
                className={`nav-item${screen === e.id ? ' nav-item--on' : ''}`}
                onClick={() => go(e.id)}
              >
                <span>{e.label}</span>
                {e.badge === 'pending' && pendingCount > 0 && (
                  <span className="nav-item__badge num">{pendingCount}</span>
                )}
                {e.badge === 'unread' && unreadCount > 0 && (
                  <span className="nav-item__badge num">{unreadCount}</span>
                )}
              </button>
            ),
          )}
        </nav>

        {meta?.setup_required && !showWizard && (
          <div className="sidebar__setup">
            <p className="sidebar__setup-note">
              {canSetUp ? '尚未完成首次設定。' : '尚未完成首次設定，請系統管理員設定公司資料。'}
            </p>
            {canSetUp && (
              <button className="btn btn--compact" onClick={() => setShowWizard(true)}>
                繼續設定
              </button>
            )}
          </div>
        )}

        <div className="sidebar__who">
          <div className="sidebar__who-name">{user?.display_name}</div>
          <div>{user?.role_name}</div>
          <div className="sidebar__account-actions">
            {admin && (
              <button
                className={`sidebar__admin${section === 'sys' ? ' sidebar__admin--on' : ''}`}
                onClick={() => switchSection(section === 'sys' ? (tabs[0]?.section ?? 'sys') : 'sys')}
              >
                系統
              </button>
            )}
            <button className="sidebar__signout" onClick={() => void signOut()}>
              登出
            </button>
          </div>
        </div>
      </aside>

      <main className="main">
        <header className="header">

          <button
            className="header__menu"
            aria-label="導覽"
            aria-expanded={navOpen}
            aria-controls="laysi-nav"
            onClick={() => setNavOpen((v) => !v)}
          >
            ☰
          </button>
          <span className="header__brand">
            <BrandMark height={12} />
          </span>

          <div className="header__section">{SECTION_LABEL[section]}</div>
          {headerContext && <div className="header__meta">{headerContext}</div>}

          <span className="spacer" />

          {section === 'fin' && canWriteVouchers && (
            <div className="header__actions">
              <button className="btn" onClick={() => setDrawer(true)}>
                ＋ 建立傳票
              </button>
            </div>
          )}

          {section === 'me' && punch && (
            <div className="header__actions">
              {punch.today_punches?.in_at && (
                <span className="header__state num">
                  {punch.today_punches.in_at}
                  {punch.today_punches.out_at ? ` – ${punch.today_punches.out_at}` : ' 上班中'}
                </span>
              )}
              {screen !== 'meclock' &&
                (punch.next_kind ? (
                  <button
                    className="btn"
                    disabled={punching}
                    onClick={() => void punchNow(punch.next_kind as 'in' | 'out')}
                  >
                    {punching ? '記錄中…' : `${punch.next_kind_label}打卡`}
                  </button>
                ) : (
                  <button className="btn btn--secondary" onClick={() => go('meclock')}>
                    今日已完成
                  </button>
                ))}
            </div>
          )}
        </header>

        <div className="content">
          {sections.length === 0 ? (
            <p className="state-note state-note--error">
              角色「{user?.role_name}」目前沒有任何操作權限。
              請負責人於「角色與核准」為這個角色開啟需要的權限。
            </p>
          ) : showWizard ? (
            <SetupWizard
              onToast={showToast}
              onProgress={reload}
              onExit={() => setShowWizard(false)}
            />
          ) : missingPath ? (
            <div className="placeholder">
              <div className="placeholder__title">找不到頁面</div>
              <p className="num">{missingPath}</p>
              <div className="placeholder__actions">
                <button className="btn" onClick={() => go(landing)}>
                  回到{screenTitle(landing)}
                </button>
              </div>
            </div>
          ) : (
            <>
              {screen === 'dash' && <Dashboard onGo={go} reloadKey={reloadKey} />}
              {screen === 'gl' && (
                <GeneralLedger
                  onToast={showToast}
                  reloadKey={reloadKey}
                  onChanged={reload}
                  canDiscard={permissions.includes('discard_voucher')}
                />
              )}
              {screen === 'ar' && (
                <Receivables onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'ap' && (
                <Payables onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'bank' && (
                <Banking onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'tax' && <Tax reloadKey={reloadKey} />}
              {screen === 'fa' && (
                <FixedAssets onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'exp' && (
                <Expenses
                  onToast={showToast}
                  reloadKey={reloadKey}
                  onChanged={reload}
                  canSubmit={permissions.includes('submit_expense_claim')}
                />
              )}
              {screen === 'budget' && <Budget onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'rep' && <Reports reloadKey={reloadKey} />}
              {screen === 'doc' && <Documents onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'imp' && (
                <Imports onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'setup' && (
                <Setup onGo={go} onToast={showToast} reloadKey={reloadKey} />
              )}
              {screen === 'acc' && (
                <Accounts onGo={go} onToast={showToast} reloadKey={reloadKey} />
              )}
              {screen === 'roles' && <Roles onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'mcp' && <McpTools onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'sec' && <Security onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'reg' && <Regulations onToast={showToast} reloadKey={reloadKey} />}

              {screen === 'hrdash' && <HrDashboard onGo={go} reloadKey={reloadKey} />}
              {screen === 'hr' && (
                <Employees onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'att' && (
                <Attendance onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'pay' && (
                <Payroll onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'ins' && <Insurance onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'wht' && <Withholding onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'cal' && <WorkCalendar onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'notice' && (
                <Notices
                  onToast={showToast}
                  reloadKey={reloadKey}
                  canManage={permissions.includes('create_notice')}
                />
              )}

              {screen === 'me' && <Me onGo={go} reloadKey={reloadKey} />}
              {screen === 'meclock' && (
                <Clock onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'meleavenew' && (
                <LeaveRequest
                  onToast={showToast}
                  onGo={go}
                  reloadKey={reloadKey}
                  onChanged={reload}
                />
              )}
              {screen === 'meotnew' && (
                <OvertimeRequest
                  onToast={showToast}
                  onGo={go}
                  reloadKey={reloadKey}
                  onChanged={reload}
                />
              )}
              {screen === 'meexpnew' && (
                <ExpenseRequest
                  onToast={showToast}
                  onGo={go}
                  reloadKey={reloadKey}
                  onChanged={reload}
                />
              )}
              {screen === 'meexp' && (
                <MyExpenses onGo={go} onToast={showToast} reloadKey={reloadKey} />
              )}
              {screen === 'mepay' && <MyPayslips reloadKey={reloadKey} />}
              {screen === 'meleave' && <MyLeave onGo={go} reloadKey={reloadKey} />}
              {screen === 'meot' && <MyOvertime onGo={go} reloadKey={reloadKey} />}
              {screen === 'memcp' && <MyMcp onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'menotice' && (
                <MyNotices reloadKey={reloadKey} onRead={() => setUnreadCount(0)} />
              )}
            </>
          )}
        </div>
      </main>

      <EntryDrawer
        open={drawer}
        onClose={() => setDrawer(false)}
        onToast={showToast}
        onSaved={reload}
      />

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
