import { useCallback, useEffect, useState } from 'react';
import { useCurrentUser } from '../auth';
import {
  businessTime,
  callOp,
  getHealthz,
  OpFailed,
  type ApiTokenList,
  type Healthz,
  type OidcSetting,
  type SecurityPolicy,
  type SystemHealth,
  type UpdatedOidcSetting,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

const GOOGLE_ISSUER = 'https://accounts.google.com';
const CALLBACK_PATH = '/v1/auth/oauth/callback';

interface OidcForm {
  provider: string;
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
  const google = setting.issuer === '' || setting.issuer === GOOGLE_ISSUER;
  return {
    provider: google ? 'google' : 'other',
    issuer: setting.issuer || GOOGLE_ISSUER,
    client_id: setting.client_id,
    client_secret: '',
    redirect_url: setting.redirect_url || `${window.location.origin}${CALLBACK_PATH}`,
    allowed_domain: setting.allowed_domain,
    provision_role: setting.provision_role,
    enabled: setting.enabled ? 'on' : 'off',
    password_login_disabled: setting.password_login_disabled,
  };
}

export function Security({ onToast, reloadKey }: Props) {
  const [policy, setPolicy] = useState<SecurityPolicy | null>(null);
  const [tokens, setTokens] = useState<ApiTokenList | null>(null);
  const [health, setHealth] = useState<SystemHealth | null>(null);
  const [hz, setHz] = useState<Healthz | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [issued, setIssued] = useState<{ name: string; secret: string } | null>(null);
  const [oidc, setOidc] = useState<OidcSetting | null>(null);
  const [oidcForm, setOidcForm] = useState<OidcForm | null>(null);
  const [savingOidc, setSavingOidc] = useState(false);

  const me = useCurrentUser();
  const canReadOidc = me.permissions.includes('get_oidc_setting');
  const canEditOidc = me.permissions.includes('update_oidc_setting');

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<SecurityPolicy>('get_security_policy'),
      callOp<ApiTokenList>('list_api_tokens'),
      callOp<SystemHealth>('get_system_health'),
    ])
      .then(([p, t, h]) => {
        setPolicy(p);
        setTokens(t);
        setHealth(h);
      })
      .catch((e: Error) => setError(e.message));

    getHealthz().then(setHz).catch(() => setHz(null));

    if (canReadOidc) {
      callOp<OidcSetting>('get_oidc_setting')
        .then(setOidc)
        .catch(() => setOidc(null));
    }
  }, [canReadOidc]);

  useEffect(load, [load, reloadKey]);

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
    const issuer = oidcForm.provider === 'google' ? GOOGLE_ISSUER : oidcForm.issuer.trim();
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
      const d = await callOp<UpdatedOidcSetting>('update_oidc_setting', input, '設定外部登入');
      onToast(`已儲存：${d.changed.join('、')}`);
      setOidcForm(null);
      const fresh = await callOp<OidcSetting>('get_oidc_setting');
      setOidc(fresh);
      if (fresh.verify_error) onToast(fresh.verify_error);
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingOidc(false);
    }
  }

  async function issue() {
    try {
      const d = await callOp<{ token: { name: string }; secret: string }>('issue_api_token', {
        name: '新連線',
        kind: 'mcp',
        scope: '唯讀',
        actor: me.display_name,
        actor_role: me.role,
        rotate_days: 90,
      });
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
      {!policy && !error && <p className="state-note">讀取中…</p>}

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
        </div>
      )}

      {policy && tokens && health && (
        <div className="split-columns">
          <section>
            <h3 className="section-title">控制規則</h3>
            <div className="card fact-card">
              {policy.rules.map((r) => (
                <div className="fact fact--stacked" key={r.code}>
                  <span className="fact__key">
                    {r.label}
                    <span className={`tag${r.enforced ? ' tag--warn tag--static' : ' tag--pale'}`}>
                      {r.enforced ? '執行中' : '未啟用'}
                    </span>
                  </span>
                  <span className="fact__value">
                    {r.value}
                    {r.detail && <span className="fact__note">{r.detail}</span>}
                  </span>
                </div>
              ))}
            </div>
            <p className="recon">「AI 禁止事項」在「MCP 與 AI」調整。</p>

            <h3 className="section-title section-title--spaced">系統健康</h3>
            <div className="card fact-card">
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
                  <span className="fact__note">{hz?.rustfs.endpoint ?? '由 /healthz 回報'}</span>
                </span>
              </div>
              <div className="fact">
                <span className="fact__key">品牌完整性</span>
                <span className="fact__value">
                  {hz ? (
                    <>
                      {hz.brand.signed ? '已簽章' : '未簽章'} · {hz.brand.files} 個檔案
                      <span className="fact__note num">指紋 {hz.brand.fingerprint}</span>
                    </>
                  ) : (
                    '無法讀取'
                  )}
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
                    {health.stats.audit_entries} 筆（{health.stats.audit_blocked} 筆被權限擋下、
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
                  <div className="card">
                    <div className="field-grid">
                      <label className="field">
                        供應商
                        <select
                          className="input"
                          value={oidcForm.provider}
                          onChange={(e) => setOidcField('provider', e.target.value)}
                        >
                          <option value="google">Google Workspace／Cloud Identity</option>
                          <option value="other">其他</option>
                        </select>
                        <span className="field__hint">
                          在 Google Cloud console 建立「網頁應用程式」類型的用戶端，
                          同意畫面選 Internal。
                        </span>
                      </label>
                      {oidcForm.provider === 'other' && (
                        <label className="field">
                          供應商網址
                          <input
                            className="input"
                            value={oidcForm.issuer}
                            placeholder="https://login.example.com"
                            onChange={(e) => setOidcField('issuer', e.target.value)}
                          />
                        </label>
                      )}
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
                        允許的網域
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
                        <span className="field__hint">自動建立帳號需要先填允許的網域。</span>
                      </label>
                      <div className="field field--full">
                        轉址網址
                        <input
                          className="input"
                          value={oidcForm.redirect_url}
                          onChange={(e) => setOidcField('redirect_url', e.target.value)}
                        />
                        <span className="field__hint">
                          這個網址要一字不差地填進供應商的轉址白名單。{' '}
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
                    <div className="toolbar doc-card__actions">
                      <button
                        className="btn btn--compact btn--ghost"
                        onClick={() => setOidcForm(null)}
                      >
                        取消
                      </button>
                      <span className="spacer" />
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
                            <span className="fact__key">允許的網域</span>
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

            <h3 className={`section-title${oidc ? ' section-title--spaced' : ''}`}>Token 與連線</h3>
            <p className="section-note">{tokens.storage_note}</p>
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

            <p className="hint">
              新發的 Token 代表<span className="num">{me.display_name}</span>。
            </p>
          </section>
        </div>
      )}
    </div>
  );
}
