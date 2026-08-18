import { useState, type FormEvent } from 'react';
import { BrandMark } from '../components/BrandMark';
import { AuthFailed, useAuth } from '../auth';

export function Init() {
  const { setup, bootstrap } = useAuth();

  const [username, setUsername] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [setupToken, setSetupToken] = useState('');
  const [failure, setFailure] = useState<AuthFailed | null>(null);
  const [mismatch, setMismatch] = useState(false);
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setFailure(null);

    if (password !== confirmPassword) {
      setMismatch(true);
      return;
    }
    setMismatch(false);
    setBusy(true);
    try {
      await bootstrap({
        username: username.trim(),
        display_name: displayName.trim(),
        email: email.trim(),
        password,
        setup_token: setupToken,
      });
    } catch (err) {
      setFailure(
        err instanceof AuthFailed ? err : new AuthFailed({ code: 'network', message: String(err) }),
      );
      setPassword('');
      setConfirmPassword('');
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="login">
      <div className="card login__panel">
        <div className="login__brand">
          <BrandMark height={30} />
        </div>

        <h1 className="login__title">建立系統管理員帳號</h1>
        <p className="login__lede">
          此帳號管理帳號、權限與系統設定，看不到薪資。負責人與其他成員於登入後在
          「角色與核准」建立。
        </p>

        <form onSubmit={(e) => void submit(e)}>
          <label className="login__field">
            <span className="login__label">登入名稱（英文字母、數字與 . _ -）</span>
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
            <span className="login__label">顯示名稱（記載於稽核軌跡）</span>
            <input
              className="entry-input"
              type="text"
              autoComplete="name"
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
            />
          </label>
          <label className="login__field">
            <span className="login__label">電子郵件（選填）</span>
            <input
              className="entry-input"
              type="email"
              autoComplete="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
            />
          </label>
          <label className="login__field">
            <span className="login__label">密碼（至少 12 個字元）</span>
            <input
              className="entry-input"
              type="password"
              autoComplete="new-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </label>
          <label className="login__field">
            <span className="login__label">請再次輸入密碼</span>
            <input
              className="entry-input"
              type="password"
              autoComplete="new-password"
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
            />
          </label>

          {setup.token_required && (
            <label className="login__field">
              <span className="login__label">初始化代碼</span>
              <input
                className="entry-input"
                type="password"
                autoComplete="off"
                value={setupToken}
                onChange={(e) => setSetupToken(e.target.value)}
              />
              <span className="login__hint">請向部署人員索取。</span>
            </label>
          )}

          {mismatch && (
            <p className="state-note state-note--error login__alert">兩次輸入的密碼不一致。</p>
          )}
          {failure && <p className="state-note state-note--error login__alert">{failure.message}</p>}

          <div className="login__actions">
            <button className="btn" type="submit" disabled={busy}>
              {busy ? '建立中…' : '建立帳號並進入系統'}
            </button>
          </div>
        </form>

        <p className="login__note">此步驟只執行一次，完成後直接登入並繼續設定公司資料。</p>
      </div>
    </div>
  );
}
