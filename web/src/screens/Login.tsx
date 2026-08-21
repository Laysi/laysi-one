import { useState, type FormEvent } from 'react';
import { BrandMark } from '../components/BrandMark';
import { AuthFailed, NO_USER_ACCOUNT, OAUTH_START_URL, useAuth } from '../auth';

export function Login() {
  const { user, oauth, signIn, changePassword } = useAuth();
  const mustChange = user?.must_change_password ?? false;

  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [currentPassword, setCurrentPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [failure, setFailure] = useState<AuthFailed | null>(null);
  const [mismatch, setMismatch] = useState(false);
  const [busy, setBusy] = useState(false);
  const passwordHidden = oauth.enabled && oauth.password_login_disabled;
  const [showPasswordForm, setShowPasswordForm] = useState(!passwordHidden);
  const [adminOnly, setAdminOnly] = useState(false);

  async function submitLogin(e: FormEvent) {
    e.preventDefault();
    setFailure(null);
    setBusy(true);
    try {
      await signIn(username.trim(), password);
    } catch (err) {
      setFailure(err instanceof AuthFailed ? err : new AuthFailed({ code: 'network', message: String(err) }));
    } finally {
      setPassword('');
      setBusy(false);
    }
  }

  async function submitChange(e: FormEvent) {
    e.preventDefault();
    setFailure(null);

    if (newPassword !== confirmPassword) {
      setMismatch(true);
      return;
    }
    setMismatch(false);
    setBusy(true);
    try {
      await changePassword(currentPassword, newPassword);
    } catch (err) {
      setFailure(err instanceof AuthFailed ? err : new AuthFailed({ code: 'network', message: String(err) }));
    } finally {
      setCurrentPassword('');
      setNewPassword('');
      setConfirmPassword('');
      setBusy(false);
    }
  }

  return (
    <div className="login">
      <div className="card login__panel">
        <div className="login__brand">

          <BrandMark height={20} />
        </div>

        {mustChange ? (
          <>
            <h1 className="login__title">請先設定新密碼</h1>

            <form onSubmit={(e) => void submitChange(e)}>
              <label className="login__field">
                <span className="login__label">目前的密碼</span>
                <input
                  className="entry-input"
                  type="password"
                  autoComplete="current-password"
                  value={currentPassword}
                  autoFocus
                  onChange={(e) => setCurrentPassword(e.target.value)}
                />
              </label>
              <label className="login__field">
                <span className="login__label">新密碼（至少 8 個字元）</span>
                <input
                  className="entry-input"
                  type="password"
                  autoComplete="new-password"
                  value={newPassword}
                  onChange={(e) => setNewPassword(e.target.value)}
                />
              </label>
              <label className="login__field">
                <span className="login__label">請再次輸入新密碼</span>
                <input
                  className="entry-input"
                  type="password"
                  autoComplete="new-password"
                  value={confirmPassword}
                  onChange={(e) => setConfirmPassword(e.target.value)}
                />
              </label>

              {mismatch && (
                <p className="state-note state-note--error login__alert">
                  兩次輸入的新密碼不一致。
                </p>
              )}
              {failure && (
                <p className="state-note state-note--error login__alert">{failure.message}</p>
              )}

              <div className="login__actions">
                <button className="btn" type="submit" disabled={busy}>
                  {busy ? '更新中…' : '完成並進入系統'}
                </button>
              </div>
            </form>
          </>
        ) : (
          <>
            <h1 className="login__title">登入</h1>

            {showPasswordForm && (
              <form onSubmit={(e) => void submitLogin(e)}>
                <label className="login__field">
                  <span className="login__label">{adminOnly ? '登入名稱' : '電子郵件'}</span>
                  <input
                    className="entry-input"
                    type="text"
                    autoComplete="username"
                    value={username}
                    autoFocus
                    onChange={(e) => setUsername(e.target.value)}
                  />
                </label>
                <label className="login__field">
                  <span className="login__label">密碼</span>
                  <input
                    className="entry-input"
                    type="password"
                    autoComplete="current-password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                  />
                </label>

                {failure && (
                  <p className="state-note state-note--error login__alert">
                    {failure.message}
                    {failure.error.code === NO_USER_ACCOUNT && (
                      <span className="login__hint">
                        請重新整理頁面建立第一個帳號，或洽管理員。
                      </span>
                    )}
                  </p>
                )}

                <div className="login__actions">
                  <button className="btn" type="submit" disabled={busy}>
                    {busy ? '確認中…' : '登入'}
                  </button>
                </div>
              </form>
            )}

            {oauth.enabled && (
              <div className="login__alt">
                {showPasswordForm && <span className="login__alt-rule">或</span>}
                <a className="btn btn--secondary login__oauth" href={OAUTH_START_URL}>
                  使用 {oauth.label} 登入
                </a>
                <p className="login__note">
                  {oauth.creates_accounts
                    ? oauth.domain
                      ? `請使用 @${oauth.domain} 的帳號。`
                      : '請使用公司帳號。'
                    : `${oauth.label} 帳號需由系統管理員先連結。`}
                </p>
              </div>
            )}

            {passwordHidden && !showPasswordForm && (
              <p className="login__note">
                <button
                  type="button"
                  className="link-cell"
                  onClick={() => {
                    setUsername('admin');
                    setAdminOnly(true);
                    setShowPasswordForm(true);
                  }}
                >
                  系統管理員改用密碼登入
                </button>
              </p>
            )}
          </>
        )}
      </div>
    </div>
  );
}
