import { useState, type FormEvent } from 'react';
import { BrandMark } from '../components/BrandMark';
import { CompanyBrand } from '../components/CompanyBrand';
import { AuthFailed, useAuth } from '../auth';

export function Init() {
  const { setup, brand, bootstrap } = useAuth();

  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [setupToken, setSetupToken] = useState('');
  const [activationKey, setActivationKey] = useState('');
  const [licenseToken, setLicenseToken] = useState('');
  const [offline, setOffline] = useState(false);
  const [failure, setFailure] = useState<AuthFailed | null>(null);
  const [mismatch, setMismatch] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
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
        password,
        setup_token: setupToken,
        activation_key: offline ? '' : activationKey,
        license_token: offline ? licenseToken : '',
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
          <CompanyBrand brand={brand} place="panel" />
        </div>

        <h1 className="login__title">啟用授權並設定系統管理員密碼</h1>

        <form onSubmit={(e) => void submit(e)}>
          {offline ? (
            <>
              <label className="login__field">
                <span className="login__label">本機識別碼</span>
                <input className="entry-input num" type="text" value={setup.fingerprint} readOnly />
                <span className="login__hint">交給經銷商，用來產生本機專用的授權檔。</span>
              </label>
              <label className="login__field">
                <span className="login__label">
                  離線授權檔
                  <button
                    type="button"
                    className="link-cell login__reveal"
                    onClick={() => setOffline(false)}
                  >
                    改用啟動金鑰
                  </button>
                </span>
                <textarea
                  className="entry-input"
                  rows={4}
                  autoComplete="off"
                  value={licenseToken}
                  onChange={(e) => setLicenseToken(e.target.value)}
                />
              </label>
            </>
          ) : (
            <label className="login__field">
              <span className="login__label">
                啟動金鑰
                <button
                  type="button"
                  className="link-cell login__reveal"
                  onClick={() => setOffline(true)}
                >
                  本機不對外連線
                </button>
              </span>
              <input
                className="entry-input num"
                type="text"
                autoComplete="off"
                placeholder="LAYSI-XXXXX-XXXXX-XXXXX-XXXXX"
                value={activationKey}
                autoFocus
                onChange={(e) => setActivationKey(e.target.value)}
              />
              <span className="login__hint">由經銷商提供，一組金鑰對應一台主機。</span>
            </label>
          )}

          <label className="login__field">
            <span className="login__label">登入名稱</span>
            <input className="entry-input" type="text" autoComplete="username" value="admin" readOnly />
          </label>
          <label className="login__field">
            <span className="login__label">
              密碼（至少 8 個字元）
              <button
                type="button"
                className="link-cell login__reveal"
                onClick={() => setShowPassword((v) => !v)}
              >
                {showPassword ? '隱藏密碼' : '顯示密碼'}
              </button>
            </span>
            <input
              className="entry-input"
              type={showPassword ? 'text' : 'password'}
              autoComplete="new-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </label>
          <label className="login__field">
            <span className="login__label">請再次輸入密碼</span>
            <input
              className="entry-input"
              type={showPassword ? 'text' : 'password'}
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
              <span className="login__hint">
                在伺服器的 <span className="num">setup/初始化代碼.txt</span>。
              </span>
            </label>
          )}

          {mismatch && (
            <p className="state-note state-note--error login__alert">兩次輸入的密碼不一致。</p>
          )}
          {failure && <p className="state-note state-note--error login__alert">{failure.message}</p>}

          <div className="login__actions">
            <button className="btn" type="submit" disabled={busy}>
              {busy ? '建立中…' : '設定密碼並進入系統'}
            </button>
          </div>
        </form>

      </div>

      <div className="login__mark">
        <BrandMark size="sm" />
      </div>
    </div>
  );
}
