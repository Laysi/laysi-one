import { useCallback, useEffect, useRef, useState } from 'react';
import { BrandMark } from './components/BrandMark';
import { EntryDrawer } from './components/EntryDrawer';
import { SetupWizard } from './components/SetupWizard';
import { Attendance } from './screens/Attendance';
import { Banking } from './screens/Banking';
import { Budget } from './screens/Budget';
import { Documents } from './screens/Documents';
import { Employees } from './screens/Employees';
import { Expenses } from './screens/Expenses';
import { Imports } from './screens/Imports';
import { Dashboard } from './screens/Dashboard';
import { FixedAssets } from './screens/FixedAssets';
import { GeneralLedger } from './screens/GeneralLedger';
import { HrDashboard } from './screens/HrDashboard';
import { Insurance } from './screens/Insurance';
import { Invoicing } from './screens/Invoicing';
import { Login } from './screens/Login';
import { McpTools } from './screens/McpTools';
import { Me } from './screens/Me';
import { MyExpenses } from './screens/MyExpenses';
import { MyLeave } from './screens/MyLeave';
import { MyPayslips } from './screens/MyPayslips';
import { Onboarding } from './screens/Onboarding';
import { Payables } from './screens/Payables';
import { Payroll } from './screens/Payroll';
import { Receivables } from './screens/Receivables';
import { Reports } from './screens/Reports';
import { Roles } from './screens/Roles';
import { Security } from './screens/Security';
import { Setup } from './screens/Setup';
import { Tax } from './screens/Tax';
import { Withholding } from './screens/Withholding';
import { FINANCE_NAV, HR_NAV, ME_NAV, ROADMAP, type NavEntry, type ScreenId } from './nav';
import { AuthProvider, useAuth, useAuthSession } from './auth';
import { callOp, getMeta, type Meta, type Voucher } from './api/client';

/**
 * Which tree the sidebar currently shows.
 *
 * Switching sections switches navigation, not identity. There is one
 * identity, it comes from the session, and it does not change during use;
 * the section only decides which screens the sidebar lists. The owner still
 * has to switch to HR to see payroll, and staff switching to finance see
 * those screens listed while the backend decides call by call what actually
 * works. Seeing an entry does not mean the action will succeed, which is
 * deliberate: hiding everything unavailable leaves people unable to learn
 * which permission they lack.
 */
type Section = 'fin' | 'hr' | 'me';

const NAV: Record<Section, NavEntry[]> = { fin: FINANCE_NAV, hr: HR_NAV, me: ME_NAV };
const SECTION_LABEL: Record<Section, string> = { fin: '財務', hr: '人資', me: '我的' };
/** Landing screen for each section, so switching shows something. */
const SECTION_HOME: Record<Section, ScreenId> = { fin: 'dash', hr: 'hrdash', me: 'me' };

/** Which tree a screen belongs to. A to-do on the HR dashboard can point at
 *  the general ledger, and the sidebar follows. */
function sectionOf(id: ScreenId): Section {
  if (ME_NAV.some((e) => e.kind === 'item' && e.id === id)) return 'me';
  return HR_NAV.some((e) => e.kind === 'item' && e.id === id) ? 'hr' : 'fin';
}

/**
 * Root component.
 *
 * Signed out means only the login screen: the main screens are not rendered
 * behind an overlay but not rendered at all, because they call operations on
 * mount and every call would 401.
 *
 * Someone still on the initial password stops here too. The session exists,
 * but the password has to become one only they know before "who approved
 * this" has an answer.
 */
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
      ) : (
        <Login />
      )}
    </AuthProvider>
  );
}

function Workspace() {
  const { user, signOut } = useAuth();
  const [section, setSection] = useState<Section>('fin');
  const [screen, setScreen] = useState<ScreenId>('dash');
  const [meta, setMeta] = useState<Meta | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [drawer, setDrawer] = useState(false);
  const [pendingCount, setPendingCount] = useState(0);
  const [reloadKey, setReloadKey] = useState(0);
  const [showWizard, setShowWizard] = useState(false);
  // Open the wizard automatically only the first time setup_required is seen.
  // After the user dismisses it, a reload triggered by another screen must not
  // bring it back; the sidebar reminder covers that instead.
  const wizardPromptedRef = useRef(false);

  const reload = useCallback(() => setReloadKey((k) => k + 1), []);

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    window.setTimeout(() => setToast((t) => (t === msg ? null : t)), 3200);
  }, []);

  /** Go to a screen and switch the sidebar to its tree. Identity is
   *  unaffected. */
  const go = useCallback((id: ScreenId) => {
    setSection(sectionOf(id));
    setScreen(id);
  }, []);

  useEffect(() => {
    getMeta()
      .then((m) => {
        setMeta(m);
        if (m.setup_required && !wizardPromptedRef.current) {
          wizardPromptedRef.current = true;
          setShowWizard(true);
        }
      })
      .catch(() => setMeta(null));
  }, [reloadKey]);

  // Sidebar badges: counts of unposted and unconfirmed vouchers.
  // Nothing in the self-service tree needs a badge, so those two operations
  // are not called there: succeeding would be pointless and being blocked
  // would leave two audit entries for nothing.
  useEffect(() => {
    if (section === 'me') {
      setPendingCount(0);
      return;
    }
    Promise.all([
      callOp<{ vouchers: Voucher[] }>('search_vouchers', { status: 'pending', limit: 500 }),
      callOp<{ vouchers: Voucher[] }>('search_vouchers', { status: 'draft', limit: 500 }),
    ])
      .then(([p, d]) => setPendingCount(p.vouchers.length + d.vouchers.length))
      .catch(() => setPendingCount(0));
  }, [reloadKey, section]);

  /** Switch tree and land on that section's first screen. */
  function switchSection(next: Section) {
    if (next === section) return;
    go(SECTION_HOME[next]);
  }

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="sidebar__brand">
          {/* The wordmark is generated by the Rust backend; the frontend has
              no logo file and must not have one. */}
          <BrandMark height={26} />
          <div className="sidebar__company">{meta?.company ?? '　'}</div>

          {/* This switches the navigation tree, not the identity. There is
              one identity, it comes from the session, and it cannot change. */}
          <div className="sidebar__tabs">
            {(['fin', 'hr', 'me'] as const).map((s) => (
              <button
                key={s}
                className={`sidebar__tab${section === s ? ' sidebar__tab--on' : ''}`}
                onClick={() => switchSection(s)}
              >
                {SECTION_LABEL[s]}
              </button>
            ))}
          </div>
        </div>

        <nav className="sidebar__nav">
          {NAV[section].map((e, i) =>
            e.kind === 'head' ? (
              <div className="nav-head" key={`h${i}`}>
                {e.label}
              </div>
            ) : (
              <button
                key={e.id}
                className={`nav-item${screen === e.id ? ' nav-item--on' : ''}`}
                onClick={() => setScreen(e.id)}
              >
                <span>{e.label}</span>
                {e.badge === 'pending' && pendingCount > 0 && (
                  <span className="nav-item__badge num">{pendingCount}</span>
                )}
              </button>
            ),
          )}
        </nav>

        {/* The company name is still empty. Even if the wizard was dismissed,
            there has to be a way back into it, so "not set up yet" does not
            appear once and then vanish. */}
        {meta?.setup_required && !showWizard && (
          <div className="sidebar__setup">
            <p className="sidebar__setup-note">尚未完成首次設定：公司名稱為空。</p>
            <button className="btn btn--compact" onClick={() => setShowWizard(true)}>
              繼續設定
            </button>
          </div>
        )}

        {/* The signed-in user. Nothing here switches to another person; that
            requires signing in again. */}
        <div className="sidebar__who">
          <div className="sidebar__who-name">{user?.display_name}</div>
          <div>
            {user?.username} · {user?.role_name}
          </div>
          <div>
            操作權限由後端每次呼叫即時查詢角色權限決定。
          </div>
          <button className="sidebar__signout" onClick={() => void signOut()}>
            登出
          </button>
          {meta && <div className="sidebar__attribution">{meta.attribution}</div>}
        </div>
      </aside>

      <main className="main">
        <header className="header">
          <div className="header__period">會計期間 {meta?.period ?? '—'}</div>
          <div className="header__state">{meta?.period_state ?? '—'}</div>
          <span className="spacer" />
          <div className="header__actions">
            <button className="btn" onClick={() => setDrawer(true)}>
              ＋ 建立傳票
            </button>
          </div>
        </header>

        <div className="content">
          {showWizard ? (
            <SetupWizard
              onToast={showToast}
              onProgress={reload}
              onExit={() => setShowWizard(false)}
            />
          ) : (
            <>
              {screen === 'dash' && <Dashboard onGo={go} reloadKey={reloadKey} />}
              {screen === 'gl' && (
                <GeneralLedger onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
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
              {screen === 'inv' && (
                <Invoicing onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'tax' && <Tax reloadKey={reloadKey} />}
              {screen === 'fa' && (
                <FixedAssets onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'exp' && (
                <Expenses onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'budget' && <Budget onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'rep' && <Reports reloadKey={reloadKey} />}
              {screen === 'doc' && <Documents onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'imp' && (
                <Imports onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'setup' && <Setup onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'roles' && <Roles onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'mcp' && <McpTools reloadKey={reloadKey} />}
              {screen === 'sec' && <Security onToast={showToast} reloadKey={reloadKey} />}

              {screen === 'hrdash' && <HrDashboard onGo={go} reloadKey={reloadKey} />}
              {screen === 'hr' && (
                <Employees onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'onb' && (
                <Onboarding onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'att' && (
                <Attendance onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'pay' && (
                <Payroll onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}
              {screen === 'ins' && <Insurance onToast={showToast} reloadKey={reloadKey} />}
              {screen === 'wht' && <Withholding onToast={showToast} reloadKey={reloadKey} />}

              {screen === 'me' && <Me onGo={go} reloadKey={reloadKey} />}
              {screen === 'meexp' && <MyExpenses reloadKey={reloadKey} />}
              {screen === 'mepay' && <MyPayslips reloadKey={reloadKey} />}
              {screen === 'meleave' && (
                <MyLeave onToast={showToast} reloadKey={reloadKey} onChanged={reload} />
              )}

              {ROADMAP[screen] && (
                <div className="placeholder">
                  <div className="placeholder__title">{ROADMAP[screen]}</div>
                  <p>
                    本模組於後續里程碑實作。「傳票 · 總帳」、「收付」、「稅務 · 資產」與
                    「報表 · 設定」四條路徑已完成全層級驗證，涵蓋資料庫不變量、法規引擎、
                    操作註冊表、REST 與 MCP、品牌完整性；其餘畫面依同一模式接續實作。
                  </p>
                </div>
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
