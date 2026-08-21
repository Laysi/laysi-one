import { useCallback, useEffect, useState } from 'react';
import {
  businessTime,
  callOp,
  OpFailed,
  type ChangedUserRole,
  type DeletedUserAccount,
  type NewUserAccount,
  type RoleMatrix,
  type UpdatedUserAccount,
  type UserAccountList,
  type UserAccountRow,
  type UserPasswordReset,
} from '../api/client';
import type { ScreenId } from '../nav';
import { useCurrentUser } from '../auth';

interface Props {
  onGo: (screen: ScreenId) => void;
  onToast: (msg: string) => void;
  reloadKey: number;
}

interface FormState {
  username: string;
  display_name: string;
  email: string;
  role_code: string;
  employee_no: string;
  status: string;
}

const EMPTY_FORM: FormState = {
  username: '',
  display_name: '',
  email: '',
  role_code: '',
  employee_no: '',
  status: 'active',
};

interface Secret {
  heading: string;
  username: string;
  subtitle: string;
  password: string;
  recipient: string;
}

export function Accounts({ onGo, onToast, reloadKey }: Props) {
  const me = useCurrentUser();
  const [list, setList] = useState<UserAccountList | null>(null);
  const [matrix, setMatrix] = useState<RoleMatrix | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [drawer, setDrawer] = useState<'create' | 'edit' | null>(null);
  const [original, setOriginal] = useState<UserAccountRow | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [confirming, setConfirming] = useState<'reset' | 'delete' | 'role' | null>(null);
  const [nextRole, setNextRole] = useState('');
  const [saving, setSaving] = useState(false);
  const [secret, setSecret] = useState<Secret | null>(null);
  const [copied, setCopied] = useState(false);

  const load = useCallback(() => {
    setError(null);
    callOp<UserAccountList>('list_user_accounts')
      .then(setList)
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    if (!drawer || matrix) return;
    callOp<RoleMatrix>('list_roles')
      .then(setMatrix)
      .catch(() => setMatrix(null));
  }, [drawer, matrix]);

  function roleName(code: string) {
    return (matrix?.roles ?? []).find((r) => r.code === code)?.name ?? code;
  }

  function setField<K extends keyof FormState>(key: K, value: FormState[K]) {
    setForm((f) => ({ ...f, [key]: value }));
  }

  function close() {
    setDrawer(null);
    setConfirming(null);
  }

  function openCreate() {
    setOriginal(null);
    setForm(EMPTY_FORM);
    setConfirming(null);
    setDrawer('create');
  }

  function openEdit(a: UserAccountRow, confirm: 'reset' | 'delete' | 'role' | null = null) {
    setOriginal(a);
    setForm({
      username: a.username,
      display_name: a.display_name,
      email: a.email,
      role_code: a.role_code,
      employee_no: a.employee_no ?? '',
      status: a.status,
    });
    setConfirming(confirm);
    setNextRole('');
    setDrawer('edit');
  }

  async function createAccount() {
    if (!form.username.trim() || !form.display_name.trim()) {
      onToast('登入名稱與顯示名稱都要填寫');
      return;
    }
    if (!form.role_code) {
      onToast('請選擇角色');
      return;
    }
    setSaving(true);
    try {
      const d = await callOp<NewUserAccount>(
        'create_user_account',
        {
          username: form.username.trim(),
          display_name: form.display_name.trim(),
          email: form.email.trim(),
          role_code: form.role_code,
          employee_no: form.employee_no.trim(),
        },
        `建立帳號 ${form.username.trim()}`,
      );
      setSecret({
        heading: '初始密碼',
        username: d.user.username,
        subtitle: `${d.user.display_name} · ${d.user.role_name} · 員工 ${d.user.employee_no}`,
        password: d.temporary_password,
        recipient: d.user.display_name,
      });
      setCopied(false);
      close();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function saveEdit() {
    if (!original) return;
    if (!form.display_name.trim()) {
      onToast('顯示名稱為必填');
      return;
    }
    const input: Record<string, string> = { username: original.username };
    if (form.display_name.trim() !== original.display_name) {
      input.display_name = form.display_name.trim();
    }
    if (form.email.trim() !== original.email) input.email = form.email.trim();
    if (form.status !== original.status) input.status = form.status;
    if (form.employee_no.trim() !== (original.employee_no ?? '')) {
      input.employee_no = form.employee_no.trim();
    }
    if (Object.keys(input).length === 1) {
      onToast('沒有變更');
      close();
      return;
    }

    setSaving(true);
    try {
      const d = await callOp<UpdatedUserAccount>(
        'update_user_account',
        input,
        `修改帳號 ${original.username}`,
      );
      onToast(`${d.user.username}：${d.changed.join('、')}`);
      close();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function toggleStatus(a: UserAccountRow) {
    const next = a.status === 'active' ? 'suspended' : 'active';
    setSaving(true);
    try {
      await callOp<UpdatedUserAccount>(
        'update_user_account',
        { username: a.username, status: next },
        `${next === 'active' ? '啟用' : '停用'}帳號 ${a.username}`,
      );
      onToast(`${a.username} 已${next === 'active' ? '啟用' : '停用'}`);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function resetPassword() {
    if (!original) return;
    setSaving(true);
    try {
      const d = await callOp<UserPasswordReset>(
        'reset_user_password',
        { username: original.username, confirm: true },
        `重設 ${original.username} 的密碼`,
      );
      setSecret({
        heading: '新的臨時密碼',
        username: d.user.username,
        subtitle: `${d.user.display_name} · ${d.user.role_name}`,
        password: d.temporary_password,
        recipient: d.user.display_name,
      });
      setCopied(false);
      close();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
      setConfirming(null);
    } finally {
      setSaving(false);
    }
  }

  async function changeRole() {
    if (!original || !nextRole) return;
    setSaving(true);
    try {
      const d = await callOp<ChangedUserRole>(
        'change_user_role',
        { username: original.username, role_code: nextRole, confirm: true },
        `將 ${original.username} 變更為 ${roleName(nextRole)}`,
      );
      onToast(
        `${d.user.username} 由${d.user.previous_role_name}改為${d.user.role_name}，` +
          `登出 ${d.revoked_sessions} 個工作階段、撤銷 ${d.revoked_tokens} 把 Token`,
      );
      close();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
      setConfirming(null);
    } finally {
      setSaving(false);
    }
  }

  async function copyPassword() {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret.password);
      setCopied(true);
      onToast(`已複製 ${secret.username} 的密碼`);
    } catch {
      onToast('這個瀏覽器不允許代為複製，請手動選取密碼');
    }
  }

  async function removeAccount() {
    if (!original) return;
    setSaving(true);
    try {
      const d = await callOp<DeletedUserAccount>(
        'delete_user_account',
        { username: original.username, confirm: true },
        `刪除帳號 ${original.username}`,
      );
      onToast(`已刪除 ${d.user.username}，剩下 ${d.remaining} 個帳號`);
      close();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
      setConfirming(null);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div>
      <h1 className="page-title">帳號管理</h1>
      <p className="page-lede">變更角色會讓該帳號登出，本人重新登入後才會拿到新的權限。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!list && !error && <p className="state-note">讀取中…</p>}

      {list && (
        <>
          <div className="toolbar">
            <span className="spacer" />
            <button className="btn btn--compact" onClick={openCreate}>
              ＋ 建立帳號
            </button>
          </div>

          {list.accounts.length === 0 ? (
            <div className="placeholder">
              <div className="placeholder__title">尚無帳號</div>
              <p>請由上方「＋ 建立帳號」建立第一個。</p>
            </div>
          ) : (
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>登入名稱</th>
                    <th>姓名</th>
                    <th>角色</th>
                    <th>員工</th>
                    <th>最後登入</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {list.accounts.map((a) => (
                    <tr
                      key={a.username}
                      className={a.status !== 'active' ? 'row--waiting' : undefined}
                    >
                      <td>
                        <button className="link-cell num" onClick={() => openEdit(a)}>
                          {a.username}
                        </button>
                        {a.status !== 'active' && (
                          <span className="tag tag--bad tag--static">已停用</span>
                        )}
                        {a.must_change_password && (
                          <span className="tag tag--warn tag--static">未變更初始密碼</span>
                        )}
                        {!a.has_password && (
                          <span className="tag tag--pale tag--static">只能用外部帳號登入</span>
                        )}
                      </td>
                      <td>
                        {a.display_name}
                        {a.email && <span className="cell-hint cell-hint--block">{a.email}</span>}
                      </td>
                      <td>{a.role_name}</td>
                      <td>
                        {a.employee_no ? (
                          <>
                            <span className="num">{a.employee_no}</span>
                            {a.employee_name && ` ${a.employee_name}`}
                            {a.employee_status === 'left' && (
                              <span className="tag tag--bad tag--static">已離職</span>
                            )}
                          </>
                        ) : (
                          <span className="cell-hint">未連結員工，編輯後可填入</span>
                        )}
                      </td>
                      <td className="num cell-nowrap">{businessTime(a.last_login_at, '尚未登入')}</td>
                      <td className="align-right cell-nowrap">
                        <button
                          className="btn btn--compact btn--secondary"
                          onClick={() => openEdit(a)}
                        >
                          編輯
                        </button>{' '}
                        <button
                          className="btn btn--compact btn--secondary"
                          disabled={saving}
                          onClick={() => void toggleStatus(a)}
                        >
                          {a.status === 'active' ? '停用' : '啟用'}
                        </button>{' '}
                        <button
                          className="btn btn--compact btn--secondary"
                          onClick={() => openEdit(a, 'delete')}
                        >
                          刪除
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}

      {secret && (
        <div className="overlay overlay--center">
          <div className="modal">
            <div className="drawer__head">
              <div>
                <div className="drawer__title">
                  <span className="num">{secret.username}</span> 的{secret.heading}
                </div>
                <div className="drawer__subtitle">{secret.subtitle}</div>
              </div>
            </div>

            <div className="drawer__body">
              <code className="secret-card__value secret-card__value--lead num">
                {secret.password}
              </code>
              <p className="form-warn">
                關掉這個視窗後無法再查看，請立即交付{secret.recipient}本人。
              </p>
            </div>

            <div className="drawer__foot">
              <button className="btn btn--secondary" onClick={() => void copyPassword()}>
                {copied ? '已複製，再複製一次' : '複製密碼'}
              </button>
              <span className="spacer" />
              <button className="btn" onClick={() => setSecret(null)}>
                我已交付
              </button>
            </div>
          </div>
        </div>
      )}

      {drawer && (
        <div className="overlay" onClick={close}>
          <aside className="drawer" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">
                  {drawer === 'edit' ? `編輯 ${form.username}` : '建立帳號'}
                </div>
                <div className="drawer__subtitle">
                  {drawer === 'edit'
                    ? '密碼只能由本人變更'
                    : '密碼由系統產生，建立後只顯示一次'}
                </div>
              </div>
              <button className="btn btn--close" onClick={close} aria-label="關閉">
                ×
              </button>
            </div>

            <div className="drawer__body">
              {confirming === 'delete' ? (
                <p className="form-warn">
                  刪除 <span className="num">{form.username}</span> 後
                  <strong>無法復原</strong>，該帳號的登入工作階段立刻失效。
                  員工主檔、稽核紀錄與既有單據都保留。
                </p>
              ) : confirming === 'role' ? (
                <div className="field-grid">
                  <div className="field field--full">
                    變更 <span className="num">{form.username}</span> 的角色
                    <select
                      className="input"
                      value={nextRole}
                      onChange={(e) => setNextRole(e.target.value)}
                    >
                      <option value="">請選擇</option>
                      {(matrix?.roles ?? [])
                        .filter((r) => r.code !== original?.role_code)
                        .map((r) => (
                          <option key={r.code} value={r.code}>
                            {r.name}
                          </option>
                        ))}
                    </select>
                    <span className="field__hint">
                      {(matrix?.roles ?? []).find((r) => r.code === nextRole)?.note ??
                        `目前為${roleName(form.role_code)}。`}
                    </span>
                  </div>
                  <p className="form-warn field--full">
                    存檔後 {form.display_name} 已登入的裝置全部登出，本人核發的 MCP Token 一併
                    撤銷，需要時請重新核發。
                  </p>
                </div>
              ) : confirming === 'reset' ? (
                <p className="form-warn">
                  重設後 <span className="num">{form.username}</span> 的舊密碼立刻失效，
                  已登入的裝置全部登出。新密碼只顯示一次，請當面交付 {form.display_name} 本人。
                </p>
              ) : (
                <div className="field-grid">
                  <label className="field">
                    登入名稱（3 到 64 個字元）
                    <input
                      className="input"
                      value={form.username}
                      placeholder="chen.mei"
                      disabled={drawer === 'edit'}
                      onChange={(e) => setField('username', e.target.value)}
                    />
                  </label>
                  <label className="field">
                    顯示名稱
                    <input
                      className="input"
                      value={form.display_name}
                      placeholder="陳美玲"
                      onChange={(e) => setField('display_name', e.target.value)}
                    />
                  </label>

                  <div className="field">
                    角色
                    {drawer === 'edit' ? (
                      <span className="field__fixed">
                        {roleName(form.role_code)}
                        {me.permissions.includes('change_user_role') &&
                          original?.username !== me.username && (
                            <>
                              {' '}
                              <button
                                className="link-cell"
                                onClick={() => {
                                  setNextRole('');
                                  setConfirming('role');
                                }}
                              >
                                變更角色
                              </button>
                            </>
                          )}
                      </span>
                    ) : (
                      <select
                        className="input"
                        value={form.role_code}
                        onChange={(e) => setField('role_code', e.target.value)}
                      >
                        <option value="">請選擇</option>
                        {(matrix?.roles ?? []).map((r) => (
                          <option key={r.code} value={r.code}>
                            {r.name}
                          </option>
                        ))}
                      </select>
                    )}
                    <span className="field__hint">
                      {(matrix?.roles ?? []).find((r) => r.code === form.role_code)?.note ??
                        '選一個角色，下面會說明它能做什麼。'}
                      {drawer === 'create' && (
                        <>
                          {' '}
                          <button className="link-cell" onClick={() => onGo('roles')}>
                            看完整權限矩陣
                          </button>
                        </>
                      )}
                    </span>
                  </div>
                  <label className="field">
                    員工編號
                    <input
                      className="input num"
                      value={form.employee_no}
                      placeholder="留空自動配號"
                      onChange={(e) => setField('employee_no', e.target.value)}
                    />
                    <span className="field__hint">
                      名冊上沒有這個人時一併建立員工主檔，部門與薪資留給人資填。
                    </span>
                  </label>

                  {drawer === 'edit' && (
                    <label className="field">
                      狀態
                      <select
                        className="input"
                        value={form.status}
                        onChange={(e) => setField('status', e.target.value)}
                      >
                        <option value="active">啟用</option>
                        <option value="suspended">停用（無法登入）</option>
                      </select>
                    </label>
                  )}

                  <label className="field field--full">
                    電子郵件（選填）
                    <input
                      className="input"
                      type="email"
                      value={form.email}
                      onChange={(e) => setField('email', e.target.value)}
                    />
                  </label>
                </div>
              )}
            </div>

            <div className="drawer__foot">
              {confirming ? (
                <>
                  <button className="btn btn--ghost" onClick={() => setConfirming(null)}>
                    ← 返回編輯
                  </button>
                  <span className="spacer" />
                  <button
                    className={`btn${saving ? ' btn--off' : ''}`}
                    disabled={saving || (confirming === 'role' && !nextRole)}
                    onClick={() =>
                      void (confirming === 'delete'
                        ? removeAccount()
                        : confirming === 'role'
                          ? changeRole()
                          : resetPassword())
                    }
                  >
                    {confirming === 'delete'
                      ? saving
                        ? '刪除中…'
                        : `確認刪除 ${form.username}`
                      : confirming === 'role'
                        ? saving
                          ? '變更中…'
                          : nextRole
                            ? `確認變更為${roleName(nextRole)}`
                            : '請先選擇角色'
                        : saving
                          ? '重設中…'
                          : `確認重設 ${form.username} 的密碼`}
                  </button>
                </>
              ) : (
                <>
                  {drawer === 'edit' ? (
                    <>
                      {original?.has_password && original.username !== me.username && (
                        <button
                          className="btn btn--compact btn--secondary"
                          onClick={() => setConfirming('reset')}
                        >
                          重設密碼
                        </button>
                      )}
                      <button
                        className="btn btn--compact btn--ghost"
                        onClick={() => setConfirming('delete')}
                      >
                        刪除帳號
                      </button>
                    </>
                  ) : null}
                  <span className="spacer" />
                  <button
                    className={`btn${saving ? ' btn--off' : ''}`}
                    disabled={saving}
                    onClick={() => void (drawer === 'edit' ? saveEdit() : createAccount())}
                  >
                    {saving ? '儲存中…' : drawer === 'edit' ? '存檔' : '建立帳號'}
                  </button>
                </>
              )}
            </div>
          </aside>
        </div>
      )}
    </div>
  );
}
