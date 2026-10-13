import { useCallback, useEffect, useRef, useState } from 'react';
import { useCurrentUser } from '../auth';
import { ConfirmModal } from '../components/ConfirmModal';
import { CopyButton } from '../components/CopyButton';
import { UpdateCard } from '../components/UpdateCard';
import {
  apiTokenList,
  backupSetting,
  businessTime,
  callOp,
  downloadFile,
  fileSizeLabel,
  getHealthz,
  getMeta,
  issuedApiToken,
  licenseState,
  oidcSetting,
  OpFailed,
  systemHealth,
  updatedBackupSetting,
  updatedOidcSetting,
  type ApiTokenList,
  type BackupSetting,
  type Healthz,
  type LicenseState,
  type Meta,
  type OidcSetting,
  type SystemHealth,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

const ISSUER_PRESETS: { key: string; name: string; issuer: string; match: string }[] = [
  {
    key: 'google',
    name: 'Google Workspace／Cloud Identity',
    issuer: 'https://accounts.google.com',
    match: 'accounts.google.com',
  },
  {
    key: 'entra',
    name: 'Microsoft Entra ID',
    issuer: 'https://login.microsoftonline.com/租戶 ID/v2.0',
    match: 'login.microsoftonline.com',
  },
  { key: 'okta', name: 'Okta', issuer: 'https://網域.okta.com', match: '.okta.com' },
  { key: 'auth0', name: 'Auth0', issuer: 'https://網域.auth0.com/', match: '.auth0.com' },
  { key: 'keycloak', name: 'Keycloak', issuer: 'https://主機/realms/領域', match: '/realms/' },
];

function presetKey(issuer: string): string {
  return ISSUER_PRESETS.find((p) => issuer.includes(p.match))?.key ?? 'other';
}
const CALLBACK_PATH = '/v1/auth/oauth/callback';

interface OidcForm {
  issuer: string;
  client_id: string;
  client_secret: string;
  redirect_url: string;
  allowed_domain: string;
  provision_role: string;
  enabled: string;
  password_login_disabled: boolean;
}

function formFrom(setting: OidcSetting): OidcForm {
  return {
    issuer: setting.issuer,
    client_id: setting.client_id,
    client_secret: '',
    redirect_url: setting.redirect_url || `${window.location.origin}${CALLBACK_PATH}`,
    allowed_domain: setting.allowed_domain,
    provision_role: setting.provision_role,
    enabled: setting.enabled ? 'on' : 'off',
    password_login_disabled: setting.password_login_disabled,
  };
}

interface BackupForm {
  enabled: boolean;
  endpoint: string;
  region: string;
  bucket: string;
  prefix: string;
  access_key: string;
  secret_key: string;
  keep_days: string;
}

function backupFormFrom(setting: BackupSetting): BackupForm {
  return {
    enabled: setting.enabled,
    endpoint: setting.endpoint,
    region: setting.region,
    bucket: setting.bucket,
    prefix: setting.prefix,
    access_key: setting.access_key,
    secret_key: '',
    keep_days: String(setting.keep_days),
  };
}

export function Security({ onToast, reloadKey }: Props) {
  const [tokens, setTokens] = useState<ApiTokenList | null>(null);
  const [health, setHealth] = useState<SystemHealth | null>(null);
  const [hz, setHz] = useState<Healthz | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [issued, setIssued] = useState<{ name: string; secret: string } | null>(null);
  const [oidc, setOidc] = useState<OidcSetting | null>(null);
  const [backup, setBackup] = useState<BackupSetting | null>(null);
  const [backupForm, setBackupForm] = useState<BackupForm | null>(null);
  const [savingBackup, setSavingBackup] = useState(false);
  const [resettingKey, setResettingKey] = useState(false);
  const [info, setInfo] = useState<Meta | null>(null);
  const [oidcForm, setOidcForm] = useState<OidcForm | null>(null);
  const [savingOidc, setSavingOidc] = useState(false);
  const [licence, setLicence] = useState<LicenseState | null>(null);
  const [activationKey, setActivationKey] = useState('');
  const [checkingLicence, setCheckingLicence] = useState(false);

  const me = useCurrentUser();
  const canReadOidc = me.permissions.includes('get_oidc_setting');
  const canEditOidc = me.permissions.includes('update_oidc_setting');
  const canReadBackup = me.permissions.includes('get_backup_setting');
  const canEditBackup = me.permissions.includes('update_backup_setting');
  const canResetBackupKey = me.permissions.includes('reset_backup_key');
  const canReadLicence = me.permissions.includes('get_license');
  const canActivateLicence = me.permissions.includes('activate_license');

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    Promise.all([
      callOp('list_api_tokens', {}, undefined, apiTokenList),
      callOp('get_system_health', {}, undefined, systemHealth),
    ])
      .then(([t, h]) => {
        if (!live) return;
        setTokens(t);
        setHealth(h);
      })
      .catch((e: Error) => live && setError(e.message));

    getHealthz()
      .then((d) => live && setHz(d))
      .catch(() => live && setHz(null));
    getMeta()
      .then((d) => live && setInfo(d))
      .catch(() => live && setInfo(null));

    if (canReadOidc) {
      callOp('get_oidc_setting', {}, undefined, oidcSetting)
        .then((d) => live && setOidc(d))
        .catch(() => live && setOidc(null));
    }

    if (canReadBackup) {
      callOp('get_backup_setting', {}, undefined, backupSetting)
        .then((d) => live && setBackup(d))
        .catch(() => live && setBackup(null));
    }

    if (canReadLicence) {
      callOp('get_license', {}, undefined, licenseState)
        .then((d) => live && setLicence(d))
        .catch(() => live && setLicence(null));
    }
    return () => running.current?.();
  }, [canReadOidc, canReadBackup, canReadLicence]);

  useEffect(load, [load, reloadKey]);

  const awaitingRestoreKey = !!backup && backup.enabled && backup.age_recipient === '';

  useEffect(() => {
    if (!awaitingRestoreKey) return;
    let left = 60;
    const timer = setInterval(() => {
      if (left-- <= 0) {
        clearInterval(timer);
        return;
      }
      callOp('get_backup_setting', {}, undefined, backupSetting)
        .then(setBackup)
        .catch(() => undefined);
    }, 5000);
    return () => clearInterval(timer);
  }, [awaitingRestoreKey]);

  async function revoke(id: number, name: string) {
    setBusy(id);
    try {
      await callOp<unknown>('revoke_api_token', { id });
      onToast(`已撤銷「${name}」，使用該 Token 的連線立即失效，如需使用請重新核發`);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  function setOidcField<K extends keyof OidcForm>(key: K, value: OidcForm[K]) {
    setOidcForm((f) => (f ? { ...f, [key]: value } : f));
  }

  async function copyRedirect() {
    if (!oidcForm) return;
    try {
      await navigator.clipboard.writeText(oidcForm.redirect_url);
      onToast('已複製轉址網址');
    } catch {
      onToast('這個瀏覽器不允許代為複製，請手動選取網址');
    }
  }

  async function saveOidc() {
    if (!oidc || !oidcForm) return;
    const issuer = oidcForm.issuer.trim();
    const input: Record<string, string | boolean> = {};
    if (issuer !== oidc.issuer) input.issuer = issuer;
    if (oidcForm.client_id.trim() !== oidc.client_id) input.client_id = oidcForm.client_id.trim();
    if (oidcForm.client_secret.trim()) input.client_secret = oidcForm.client_secret.trim();
    if (oidcForm.redirect_url.trim() !== oidc.redirect_url) {
      input.redirect_url = oidcForm.redirect_url.trim();
    }
    if (oidcForm.allowed_domain.trim() !== oidc.allowed_domain) {
      input.allowed_domain = oidcForm.allowed_domain.trim();
    }
    if (oidcForm.provision_role !== oidc.provision_role) {
      input.provision_role = oidcForm.provision_role;
    }
    if ((oidcForm.enabled === 'on') !== oidc.enabled) input.enabled = oidcForm.enabled === 'on';
    if (oidcForm.password_login_disabled !== oidc.password_login_disabled) {
      input.password_login_disabled = oidcForm.password_login_disabled;
    }
    if (Object.keys(input).length === 0) {
      onToast('沒有變更');
      setOidcForm(null);
      return;
    }

    setSavingOidc(true);
    try {
      const d = await callOp('update_oidc_setting', input, '設定外部登入', updatedOidcSetting);
      onToast(`已儲存：${d.changed.join('、')}`);
      setOidcForm(null);
      const fresh = await callOp('get_oidc_setting', {}, undefined, oidcSetting);
      setOidc(fresh);
      if (fresh.verify_error) onToast(fresh.verify_error);
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingOidc(false);
    }
  }

  async function saveBackup() {
    if (!backup || !backupForm) return;
    const input: Record<string, string | boolean | number> = {};
    if (backupForm.endpoint.trim() !== backup.endpoint) input.endpoint = backupForm.endpoint.trim();
    if (backupForm.region.trim() !== backup.region) input.region = backupForm.region.trim();
    if (backupForm.bucket.trim() !== backup.bucket) input.bucket = backupForm.bucket.trim();
    if (backupForm.prefix.trim() !== backup.prefix) input.prefix = backupForm.prefix.trim();
    if (backupForm.access_key.trim() !== backup.access_key) {
      input.access_key = backupForm.access_key.trim();
    }
    if (backupForm.secret_key.trim()) input.secret_key = backupForm.secret_key.trim();
    if (Number(backupForm.keep_days) !== backup.keep_days) {
      input.keep_days = Number(backupForm.keep_days);
    }
    if (backupForm.enabled !== backup.enabled) input.enabled = backupForm.enabled;
    if (Object.keys(input).length === 0) {
      onToast('沒有變更');
      setBackupForm(null);
      return;
    }

    setSavingBackup(true);
    try {
      const d = await callOp('update_backup_setting', input, '設定異地備份', updatedBackupSetting);
      onToast(d.blocked_reason ?? `已儲存，備份目的地 ${d.bucket}`);
      setBackupForm(null);
      setBackup(await callOp('get_backup_setting', {}, undefined, backupSetting));
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingBackup(false);
    }
  }

  async function resetBackupKey() {
    setSavingBackup(true);
    try {
      await callOp('reset_backup_key', {}, '重設還原金鑰');
      onToast('正在產生新的還原金鑰，稍候會顯示在本頁');
      setBackup(await callOp('get_backup_setting', {}, undefined, backupSetting));
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingBackup(false);
      setResettingKey(false);
    }
  }

  async function confirmBackupKey() {
    setSavingBackup(true);
    try {
      await callOp('confirm_backup_key', {}, '確認還原金鑰已保存');
      onToast('系統已不再留存還原金鑰');
      setBackup(await callOp('get_backup_setting', {}, undefined, backupSetting));
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingBackup(false);
    }
  }

  function refreshLicence() {
    callOp('get_license', {}, undefined, licenseState)
      .then(setLicence)
      .catch(() => undefined);
  }

  async function checkLicence(input: Record<string, string>) {
    setCheckingLicence(true);
    try {
      const next = await callOp('activate_license', input, undefined, licenseState);
      setLicence(next);
      setActivationKey('');
      onToast(next.note || `授權${next.standing}`);
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setCheckingLicence(false);
    }
  }

  async function issue() {
    try {
      const d = await callOp(
                        'issue_api_token',
                        {
          name: '新連線',
          kind: 'mcp',
          scope: '唯讀',
          actor: me.display_name,
          actor_role: me.role,
          rotate_days: 90,
        },
                        undefined,
                        issuedApiToken,
                      );
      setIssued({ name: d.token.name, secret: d.secret });
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  return (
    <div>
      <h1 className="page-title">安全與營運</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!tokens && !error && <p className="state-note">讀取中…</p>}

      {issued && (
        <div className="card secret-card">
          <div className="secret-card__head">
            <strong>「{issued.name}」的 Token（僅顯示一次）</strong>
            <span className="spacer" />
            <button className="btn--close" onClick={() => setIssued(null)}>
              ×
            </button>
          </div>
          <code className="secret-card__value num">{issued.secret}</code>
          <p className="form-note">
            請立即複製保存，遺失時只能重新核發。
          </p>
          <CopyButton value={issued.secret} label="複製 Token" />
        </div>
      )}

      {licence && (
        <section className="licence-card">
          <h3 className="section-title">授權</h3>
          {licence.note && (
            <p
              className={
                licence.writes_allowed ? 'state-note' : 'state-note state-note--error'
              }
            >
              {licence.note}
            </p>
          )}
          <div className="card fact-card">
            <div className="fact">
              <span className="fact__key">狀態</span>
              <span className="fact__value">{licence.standing}</span>
            </div>
            {licence.licensee && (
              <div className="fact">
                <span className="fact__key">被授權單位</span>
                <span className="fact__value">
                  {licence.licensee}
                  {licence.licensee_tax_id && (
                    <span className="fact__note num">統一編號 {licence.licensee_tax_id}</span>
                  )}
                </span>
              </div>
            )}
            {licence.partner && (
              <div className="fact">
                <span className="fact__key">授權經銷商</span>
                <span className="fact__value">{licence.partner}</span>
              </div>
            )}
            {licence.license_no && (
              <div className="fact">
                <span className="fact__key">授權編號</span>
                <span className="fact__value num">{licence.license_no}</span>
              </div>
            )}
            {licence.expires_on && (
              <div className="fact">
                <span className="fact__key">到期日</span>
                <span className="fact__value num">{licence.expires_on}</span>
              </div>
            )}
            <div className="fact">
              <span className="fact__key">最後驗證</span>
              <span className="fact__value">
                {licence.last_check_at ? businessTime(licence.last_check_at) : '尚未驗證'}
                {licence.last_error && <span className="fact__note">{licence.last_error}</span>}
              </span>
            </div>
            <div className="fact">
              <span className="fact__key">本機識別碼</span>
              <span className="fact__value num">{licence.fingerprint}</span>
            </div>
          </div>

          {canActivateLicence && (
            <div className="card form-card">
            <label className="field field--full">
              啟動金鑰
              <input
                className="input num"
                value={activationKey}
                placeholder={
                  licence.has_activation_key
                    ? '已輸入，留空表示沿用'
                    : 'LAYSI-XXXXX-XXXXX-XXXXX-XXXXX'
                }
                onChange={(e) => setActivationKey(e.target.value)}
              />
            </label>
            <div className="row-actions">
              <button
                className="btn"
                disabled={checkingLicence}
                onClick={() =>
                  void checkLicence(
                    activationKey.trim() ? { activation_key: activationKey.trim() } : {},
                  )
                }
              >
                {checkingLicence
                  ? '驗證中…'
                  : activationKey.trim()
                    ? '啟用授權'
                    : '立即驗證'}
              </button>
            </div>
            </div>
          )}
        </section>
      )}

      {licence && (
        <UpdateCard
          licence={licence}
          canUpdate={me.permissions.includes('get_update_ticket')}
          onFinished={refreshLicence}
        />
      )}

      {tokens && health && (
        <div className="split-columns">
          <section>
            <h3 className="section-title">系統健康</h3>
            <div className="card fact-card">
              <div className="fact">
                <span className="fact__key">版本</span>
                <span className="fact__value num">{info?.version ?? '讀取中…'}</span>
              </div>
              <div className="fact">
                <span className="fact__key">PostgreSQL</span>
                <span className="fact__value">
                  {hz?.db.ok ?? true ? '正常' : '無法連線'} · migration 至{' '}
                  <span className="num">{health.migration ?? '未知'}</span>
                </span>
              </div>
              <div className="fact">
                <span className="fact__key">檔案儲存</span>
                <span className="fact__value">
                  {hz ? (hz.rustfs.ok ? '正常' : '無法連線') : '無法讀取'}
                  {hz?.rustfs.endpoint ? (
                    <span className="fact__note">{hz.rustfs.endpoint}</span>
                  ) : null}
                </span>
              </div>
              <div className="fact">
                <span className="fact__key">帳務</span>
                <span className="fact__value num">
                  已過帳 {health.stats.posted_vouchers} 張 · 待過帳{' '}
                  {health.stats.unposted_vouchers} 張 · 科目 {health.stats.accounts} 個
                </span>
              </div>
              <div className="fact">
                <span className="fact__key">稽核紀錄</span>
                <span className="fact__value">
                  <span className="num">
                    {health.stats.audit_entries} 筆（{health.stats.audit_blocked} 筆被擋下、
                    {health.stats.audit_errors} 筆被規則拒絕）
                  </span>
                </span>
              </div>
              <div className="fact">
                <span className="fact__key">法規版本</span>
                <span className="fact__value">
                  {health.rulesets.filter((r) => r.applicable).length} / {health.rulesets.length}{' '}
                  個領域今天有適用版本
                  {health.rulesets_missing > 0 && (
                    <span className="fact__note">
                      {health.rulesets
                        .filter((r) => !r.applicable)
                        .map((r) => r.domain)
                        .join('、')}{' '}
                      查無適用版本，相關計算會回覆錯誤。
                    </span>
                  )}
                </span>
              </div>
              <div className="fact">
                <span className="fact__key">MCP 工具</span>
                <span className="fact__value num">
                  {health.stats.tools_enabled} / {health.stats.tools_total} 個對 AI 開啟
                </span>
              </div>
            </div>
          </section>

          <section>
            {oidc && (
              <>
                <h3 className="section-title">外部登入</h3>
                {oidcForm ? (
                  <div className="card form-card">
                    <div className="field-grid">
                      <label className="field">
                        供應商
                        <select
                          className="input"
                          value={presetKey(oidcForm.issuer)}
                          onChange={(e) => {
                            const p = ISSUER_PRESETS.find((x) => x.key === e.target.value);
                            setOidcField('issuer', p ? p.issuer : '');
                          }}
                        >
                          {ISSUER_PRESETS.map((p) => (
                            <option key={p.key} value={p.key}>
                              {p.name}
                            </option>
                          ))}
                          <option value="other">其他</option>
                        </select>
                      </label>
                      <label className="field">
                        供應商網址
                        <input
                          className="input"
                          value={oidcForm.issuer}
                          placeholder="https://login.example.com"
                          onChange={(e) => setOidcField('issuer', e.target.value)}
                        />
                      </label>
                      <label className="field field--full">
                        用戶端 ID
                        <input
                          className="input"
                          value={oidcForm.client_id}
                          placeholder="…apps.googleusercontent.com"
                          onChange={(e) => setOidcField('client_id', e.target.value)}
                        />
                      </label>
                      <label className="field field--full">
                        用戶端密鑰
                        <input
                          className="input"
                          type="password"
                          value={oidcForm.client_secret}
                          placeholder={
                            oidc.has_client_secret ? '已設定，留空表示不變更' : '貼上供應商給的密鑰'
                          }
                          onChange={(e) => setOidcField('client_secret', e.target.value)}
                        />
                        <span className="field__hint">存入後無法再顯示。</span>
                      </label>
                      <label className="field">
                        公司電子郵件網域
                        <input
                          className="input"
                          value={oidcForm.allowed_domain}
                          placeholder="example.com"
                          onChange={(e) => setOidcField('allowed_domain', e.target.value)}
                        />
                        <span className="field__hint">留空代表不限網域。</span>
                      </label>
                      <label className="field">
                        首次登入
                        <select
                          className="input"
                          value={oidcForm.provision_role}
                          onChange={(e) => setOidcField('provision_role', e.target.value)}
                        >
                          <option value="">不自動建立帳號</option>
                          {oidc.roles.map((r) => (
                            <option key={r.code} value={r.code}>
                              自動建立帳號，角色為{r.name}
                            </option>
                          ))}
                        </select>
                        <span className="field__hint">自動建立帳號需要先填公司電子郵件網域。</span>
                      </label>
                      <div className="field field--full">
                        轉址網址
                        <input
                          className="input"
                          value={oidcForm.redirect_url}
                          onChange={(e) => setOidcField('redirect_url', e.target.value)}
                        />
                        <span className="field__hint">
                          填進供應商的轉址白名單，一字不差。{' '}
                          <button className="link-cell" onClick={() => void copyRedirect()}>
                            複製
                          </button>
                        </span>
                      </div>
                      <label className="field">
                        狀態
                        <select
                          className="input"
                          value={oidcForm.enabled}
                          onChange={(e) => setOidcField('enabled', e.target.value)}
                        >
                          <option value="off">未啟用</option>
                          <option value="on">啟用，登入頁出現按鈕</option>
                        </select>
                      </label>
                      {oidcForm.enabled === 'on' && (
                        <label className="field">
                          帳號密碼登入
                          <select
                            className="input"
                            value={oidcForm.password_login_disabled ? 'off' : 'on'}
                            onChange={(e) =>
                              setOidcField('password_login_disabled', e.target.value === 'off')
                            }
                          >
                            <option value="on">開放</option>
                            <option value="off">停用</option>
                          </select>
                          <span className="field__hint">系統管理員仍可用密碼登入。</span>
                        </label>
                      )}
                    </div>
                    <div className="wizard-actions">
                      <button
                        className="btn btn--compact btn--ghost"
                        onClick={() => setOidcForm(null)}
                      >
                        取消
                      </button>
                      <button
                        className={`btn btn--compact${savingOidc ? ' btn--off' : ''}`}
                        disabled={savingOidc}
                        onClick={() => void saveOidc()}
                      >
                        {savingOidc ? '儲存中…' : '儲存'}
                      </button>
                    </div>
                  </div>
                ) : (
                  <>
                    <div className="card fact-card">
                      <div className="fact">
                        <span className="fact__key">狀態</span>
                        <span className="fact__value">
                          {!oidc.enabled
                            ? '未啟用，目前只用密碼登入'
                            : oidc.verify_error
                              ? '設定有問題，登入頁不會出現按鈕'
                              : `已啟用 · ${oidc.provider_label}`}
                          {oidc.verify_error && (
                            <span className="fact__note">{oidc.verify_error}</span>
                          )}
                        </span>
                      </div>
                      {oidc.enabled && (
                        <>
                          <div className="fact">
                            <span className="fact__key">公司電子郵件網域</span>
                            <span className="fact__value">
                              {oidc.allowed_domain ? `@${oidc.allowed_domain}` : '不限網域'}
                            </span>
                          </div>
                          <div className="fact">
                            <span className="fact__key">首次登入</span>
                            <span className="fact__value">
                              {oidc.provision_role_name
                                ? `自動建立帳號，角色為${oidc.provision_role_name}`
                                : '需由系統管理員先建立帳號'}
                            </span>
                          </div>
                          <div className="fact">
                            <span className="fact__key">帳號密碼登入</span>
                            <span className="fact__value">
                              {oidc.password_login_disabled ? '僅系統管理員' : '開放'}
                            </span>
                          </div>
                        </>
                      )}
                    </div>
                    {canEditOidc && (
                      <div className="toolbar doc-card__actions">
                        <button
                          className="btn btn--compact"
                          onClick={() => setOidcForm(formFrom(oidc))}
                        >
                          {oidc.enabled ? '變更設定' : '設定外部登入'}
                        </button>
                      </div>
                    )}
                  </>
                )}
              </>
            )}

            {backup && (
              <>
                <h3 className={`section-title${oidc ? ' section-title--spaced' : ''}`}>異地備份</h3>
                {backup.restore_key_to_save && (
                  <div className="card secret-card">
                    <div className="secret-card__head">
                      <strong>還原金鑰（僅顯示一次）</strong>
                    </div>
                    <code className="secret-card__value num">{backup.restore_key_to_save}</code>
                    <p className="form-note">
                      備份檔以這組金鑰加密，遺失後任何人都無法還原。
                      請先保存於系統之外。
                    </p>
                    <div className="toolbar doc-card__actions">
                      <button
                        className="btn btn--compact"
                        onClick={() =>
                          downloadFile(
                            '還原金鑰.txt',
                            `${backup.restore_key_to_save}\n`,
                            'text/plain;charset=utf-8',
                          )
                        }
                      >
                        下載金鑰檔
                      </button>
                      {canEditBackup && (
                        <button
                          className="btn btn--compact"
                          disabled={savingBackup}
                          onClick={() => void confirmBackupKey()}
                        >
                          已保存，不再顯示
                        </button>
                      )}
                    </div>
                  </div>
                )}
                {backupForm ? (
                  <div className="card form-card">
                    <div className="field-grid">
                      <label className="field field--full">
                        儲存位置
                        <input
                          className="input"
                          value={backupForm.endpoint}
                          placeholder="https://s3.ap-northeast-1.amazonaws.com"
                          onChange={(e) =>
                            setBackupForm({ ...backupForm, endpoint: e.target.value })
                          }
                        />
                        <span className="field__hint">相容 S3</span>
                      </label>
                      <label className="field">
                        Bucket 名稱
                        <input
                          className="input"
                          value={backupForm.bucket}
                          onChange={(e) => setBackupForm({ ...backupForm, bucket: e.target.value })}
                        />
                      </label>
                      <label className="field">
                        區域
                        <input
                          className="input"
                          value={backupForm.region}
                          onChange={(e) => setBackupForm({ ...backupForm, region: e.target.value })}
                        />
                      </label>
                      <label className="field">
                        路徑
                        <input
                          className="input"
                          value={backupForm.prefix}
                          onChange={(e) => setBackupForm({ ...backupForm, prefix: e.target.value })}
                        />
                      </label>
                      <label className="field">
                        保留天數
                        <input
                          className="input num"
                          value={backupForm.keep_days}
                          onChange={(e) =>
                            setBackupForm({ ...backupForm, keep_days: e.target.value })
                          }
                        />
                      </label>
                      <label className="field">
                        存取金鑰
                        <input
                          className="input num"
                          value={backupForm.access_key}
                          onChange={(e) =>
                            setBackupForm({ ...backupForm, access_key: e.target.value })
                          }
                        />
                      </label>
                      <label className="field">
                        存取密鑰
                        <input
                          className="input num"
                          type="password"
                          value={backupForm.secret_key}
                          placeholder={backup.has_secret_key ? '留空表示沿用原設定' : ''}
                          onChange={(e) =>
                            setBackupForm({ ...backupForm, secret_key: e.target.value })
                          }
                        />
                      </label>
                      <label className="field field--full">
                        執行備份
                        <select
                          className="input"
                          value={backupForm.enabled ? 'on' : 'off'}
                          onChange={(e) =>
                            setBackupForm({ ...backupForm, enabled: e.target.value === 'on' })
                          }
                        >
                          <option value="off">暫停</option>
                          <option value="on">每日備份</option>
                        </select>
                      </label>
                    </div>
                    <div className="wizard-actions">
                      <button className="btn btn--ghost" onClick={() => setBackupForm(null)}>
                        取消
                      </button>
                      <button
                        className="btn"
                        disabled={savingBackup}
                        onClick={() => void saveBackup()}
                      >
                        儲存
                      </button>
                    </div>
                  </div>
                ) : (
                  <>
                    <div className="card fact-card">
                      <div className="fact">
                        <span className="fact__key">狀態</span>
                        <span className="fact__value">
                          {backup.blocked_reason ?? '每日備份'}
                        </span>
                      </div>
                      {backup.bucket && (
                        <div className="fact">
                          <span className="fact__key">目的地</span>
                          <span className="fact__value">
                            {backup.bucket}
                            {backup.prefix ? ` / ${backup.prefix}` : ''}
                            <span className="fact__note">{backup.endpoint}</span>
                          </span>
                        </div>
                      )}
                      <div className="fact">
                        <span className="fact__key">上次備份</span>
                        <span className="fact__value">
                          <span className="num">{businessTime(backup.last_run_at, '尚未備份')}</span>
                          {backup.last_bytes !== null && (
                            <span className="fact__note num">
                              {fileSizeLabel(backup.last_bytes)}
                            </span>
                          )}
                          {backup.last_error && (
                            <span className="fact__note">{backup.last_error}</span>
                          )}
                        </span>
                      </div>
                      {backup.bucket && (
                        <div className="fact">
                          <span className="fact__key">保留天數</span>
                          <span className="fact__value num">{backup.keep_days}</span>
                        </div>
                      )}
                    </div>
                    <div className="toolbar doc-card__actions">
                      {canEditBackup && (
                        <button
                          className="btn btn--compact"
                          onClick={() => setBackupForm(backupFormFrom(backup))}
                        >
                          {backup.bucket ? '變更設定' : '設定異地備份'}
                        </button>
                      )}
                      {canResetBackupKey && backup.age_recipient !== '' && (
                        <button
                          className="btn btn--compact btn--secondary"
                          onClick={() => setResettingKey(true)}
                        >
                          重設還原金鑰
                        </button>
                      )}
                    </div>
                  </>
                )}
              </>
            )}

            <h3 className={`section-title${oidc || backup ? ' section-title--spaced' : ''}`}>Token 與連線</h3>
            <div className="card fact-card">
              {tokens.tokens.map((t) => (
                <div className="fact fact--stacked" key={t.id}>
                  <span className="fact__key">
                    {t.name}
                    <span className="tag tag--pale tag--static">{t.kind_label}</span>
                    {t.overdue && <span className="tag tag--bad tag--static">已達輪替期限</span>}
                  </span>
                  <span className="fact__value">
                    <span className="num">{t.prefix}…</span>　{t.scope}
                    <span className="fact__note">
                      代表 {t.actor}
                      {t.actor_role_name ? `（${t.actor_role_name}）` : ''} · 最後使用{' '}
                      <span className="num">{businessTime(t.last_used_at, '尚未使用')}</span> · {t.expiry_note}
                      {t.rotate_due_on && (
                        <>
                          （<span className="num">{t.rotate_due_on}</span> 到期）
                        </>
                      )}
                    </span>
                  </span>
                  <button
                    className="btn btn--compact btn--secondary token__revoke"
                    disabled={busy === t.id}
                    onClick={() => void revoke(t.id, t.name)}
                  >
                    {busy === t.id ? '撤銷中…' : '撤銷'}
                  </button>
                </div>
              ))}
              {tokens.tokens.length === 0 && (
                <p className="form-note">目前沒有有效的 Token。</p>
              )}
            </div>

            <div className="toolbar doc-card__actions">
              <button className="btn btn--compact" onClick={() => void issue()}>
                核發新 Token
              </button>
            </div>

            <p className="recon recon--off">{tokens.wiring_note}</p>

          </section>
        </div>
      )}

      {resettingKey && (
        <ConfirmModal
          title="重設還原金鑰"
          body={
            <p className="form-warn">
              下一次備份會產生一組新的金鑰並顯示一次。
              已經備份出去的檔案仍然只能以原金鑰開啟。
            </p>
          }
          confirmLabel="確認重設"
          busyLabel="重設中…"
          busy={savingBackup}
          onConfirm={() => void resetBackupKey()}
          onCancel={() => setResettingKey(false)}
        />
      )}
    </div>
  );
}
