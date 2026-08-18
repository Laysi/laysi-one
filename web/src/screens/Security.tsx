import { useCallback, useEffect, useState } from 'react';
import { useCurrentUser } from '../auth';
import {
  callOp,
  getHealthz,
  OpFailed,
  type ApiTokenList,
  type Healthz,
  type SecurityPolicy,
  type SystemHealth,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

function when(v: string | null): string {
  if (!v) return '尚未使用';
  const d = new Date(v);
  if (Number.isNaN(d.getTime())) return v;
  const two = (n: number) => String(n).padStart(2, '0');
  return `${two(d.getMonth() + 1)}/${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}`;
}

export function Security({ onToast, reloadKey }: Props) {
  const [policy, setPolicy] = useState<SecurityPolicy | null>(null);
  const [tokens, setTokens] = useState<ApiTokenList | null>(null);
  const [health, setHealth] = useState<SystemHealth | null>(null);
  const [hz, setHz] = useState<Healthz | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [issued, setIssued] = useState<{ name: string; secret: string } | null>(null);

  const me = useCurrentUser();

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
  }, []);

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
                      {r.enforced ? '執行中' : '尚未執行'}
                    </span>
                  </span>
                  <span className="fact__value">
                    {r.value}
                    {r.detail && <span className="fact__note">{r.detail}</span>}
                  </span>
                </div>
              ))}
            </div>
            <p className="recon">
              {policy.enforced_count} 條執行中，{policy.total - policy.enforced_count} 條尚未執行。
              「AI 禁止事項」該列在「MCP 與 AI」調整。
            </p>

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
                <span className="fact__key">物件儲存</span>
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
                    {health.stats.audit_entries} 筆（{health.stats.audit_blocked} 筆被擋下）
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
            <h3 className="section-title">Token 與連線</h3>
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
                      <span className="num">{when(t.last_used_at)}</span> · {t.expiry_note}
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
              <button className="btn btn--compact btn--secondary btn--off" disabled>
                立即備份
              </button>
              <button className="btn btn--compact btn--secondary btn--off" disabled>
                匯出全部資料
              </button>
            </div>

            <p className="recon recon--off">{tokens.wiring_note}</p>

            <p className="hint">
              新發的 Token 代表 <span className="num">{me.display_name}</span>
              （角色 <span className="num">{me.role}</span>）。
            </p>

            <p className="hint">
              Token 外洩時任何人都能立即撤銷。
            </p>
          </section>
        </div>
      )}
    </div>
  );
}
