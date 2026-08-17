import { useCallback, useEffect, useState } from 'react';
import { OPS } from '../api/generated';
import {
  callOp,
  OpFailed,
  type MyApiTokenIssued,
  type MyApiTokenList,
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

function summaryOf(name: string): string {
  return OPS.find((o) => o.name === name)?.summary ?? '';
}

export function MyMcp({ onToast, reloadKey }: Props) {
  const [data, setData] = useState<MyApiTokenList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | 'issue' | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [issued, setIssued] = useState<MyApiTokenIssued | null>(null);
  const [name, setName] = useState('');
  const [scope, setScope] = useState('');
  const [rotateDays, setRotateDays] = useState('90');
  const [showBlocked, setShowBlocked] = useState(false);

  const load = useCallback(() => {
    setError(null);
    callOp<MyApiTokenList>('list_my_api_tokens')
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function issue() {
    setBusy('issue');
    setFormError(null);
    try {
      const days = rotateDays.trim();
      const d = await callOp<MyApiTokenIssued>(
        'issue_my_api_token',
        {
          label: name.trim(),
          ...(scope.trim() ? { scope: scope.trim() } : {}),
          ...(days ? { rotate_days: Number(days) } : {}),
        },
        `核發我的 MCP Token「${name.trim()}」`,
      );
      setIssued(d);
      onToast(`「${d.token.name}」已核發，明文只顯示這一次`);
      setName('');
      setScope('');
      load();
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      setFormError(msg);
      onToast(msg);
    } finally {
      setBusy(null);
    }
  }

  async function revoke(id: number, label: string) {
    setBusy(id);
    try {
      await callOp<unknown>('revoke_my_api_token', { id }, `撤銷我的 MCP Token「${label}」`);
      onToast(`「${label}」已撤銷，使用它的連線立即失效`);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  if (error) {
    return (
      <div>
        <h1 className="page-title">我的 MCP</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          這不是「查無資料」，是帳號還沒對應到員工編號；未連結前無法核發 Token。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const tools = data.tools;

  return (
    <div>
      <h1 className="page-title">我的 MCP</h1>
      <p className="page-lede">
        核發一把代表你自己的 Token，讓 AI 助理以你的身分連進來。它能做的事與你相同，不會更多。
      </p>

      <div className="stat-grid stat-grid--two">
        <div className="card stat-card">
          <div className="stat-card__kicker">有效 Token</div>
          <div className="stat-card__value num">{data.live_count}</div>
          <div className="stat-card__note">把 · 累計核發 {data.tokens.length} 把</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">可用工具</div>
          <div className="stat-card__value num">{tools.usable.length}</div>
          <div className="stat-card__note">項 · 你的角色為 {tools.role}</div>
        </div>
      </div>

      {issued && (
        <div className="card secret-card">
          <div className="secret-card__head">
            <strong>「{issued.token.name}」的 Token（僅顯示一次）</strong>
            <span className="spacer" />
            <button className="btn--close" onClick={() => setIssued(null)}>
              ×
            </button>
          </div>
          <code className="secret-card__value num">{issued.secret}</code>
          <p className="form-note">
            請立即複製保存。系統只留前八碼與不可還原的雜湊，無法回查；遺失時只能重新核發。
            連線位址為 <span className="num">{data.endpoint}</span>，以 Bearer 帶入。
          </p>
        </div>
      )}

      <div className="split-columns">
        <section>
          <h3 className="section-title">核發新 Token</h3>
          <div className="card form-card">
            <label className="field">
              名稱
              <input
                className="input"
                placeholder="例如 桌機的 Claude"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </label>
            <label className="field">
              用途
              <input
                className="input"
                placeholder="選填，僅作標示"
                value={scope}
                onChange={(e) => setScope(e.target.value)}
              />
            </label>
            <label className="field">
              輪替天數
              <input
                className="input"
                inputMode="numeric"
                placeholder="留空表示不設輪替"
                value={rotateDays}
                onChange={(e) => setRotateDays(e.target.value)}
              />
            </label>
            <button
              className="btn"
              disabled={busy === 'issue' || name.trim() === ''}
              onClick={() => void issue()}
            >
              {busy === 'issue' ? '核發中…' : '核發'}
            </button>
            {formError && <p className="state-note state-note--error">{formError}</p>}
            <p className="form-note">
              代表對象與角色取自你的登入身分，無法指定他人。撤銷後下一次呼叫立即失效。
            </p>
          </div>

          <h3 className="section-title section-title--spaced">我核發的 Token</h3>
          <div className="card fact-card">
            {data.tokens.map((t) => (
              <div className="fact fact--stacked" key={t.id}>
                <span className="fact__key">
                  {t.name}
                  {!t.live && <span className="tag tag--pale tag--static">已失效</span>}
                  {t.expired && t.revoked_at === null && (
                    <span className="tag tag--bad tag--static">已過期</span>
                  )}
                </span>
                <span className="fact__value">
                  <span className="num">{t.prefix}…</span>　{t.scope}
                  <span className="fact__note">
                    最後使用 <span className="num">{when(t.last_used_at)}</span> · {t.expiry_note}
                  </span>
                </span>
                {t.live && (
                  <button
                    className="btn btn--compact btn--secondary token__revoke"
                    disabled={busy === t.id}
                    onClick={() => void revoke(t.id, t.name)}
                  >
                    {busy === t.id ? '撤銷中…' : '撤銷'}
                  </button>
                )}
              </div>
            ))}
            {data.tokens.length === 0 && (
              <p className="form-note">你還沒有核發過 Token。</p>
            )}
          </div>
          <p className="hint">{data.storage_note}</p>
        </section>

        <section>
          <h3 className="section-title">你的 AI 能呼叫的工具</h3>
          <div className="card fact-card">
            {tools.usable.map((t) => (
              <div className="fact fact--stacked" key={t}>
                <span className="fact__key num">{t}</span>
                <span className="fact__value">
                  <span className="fact__note">{summaryOf(t)}</span>
                </span>
              </div>
            ))}
            {tools.usable.length === 0 && (
              <p className="form-note">
                目前沒有任何工具同時符合你的角色權限與 MCP 開關。
              </p>
            )}
          </div>

          {tools.blocked_for_ai.length > 0 && (
            <>
              <div className="toolbar toolbar--spaced">
                <button
                  className={`chip${showBlocked ? ' chip--on' : ''}`}
                  onClick={() => setShowBlocked((v) => !v)}
                >
                  你有權限但對 AI 關閉的 {tools.blocked_for_ai.length} 項
                </button>
              </div>
              {showBlocked && (
                <div className="card fact-card">
                  {tools.blocked_for_ai.map((t) => (
                    <div className="fact" key={t}>
                      <span className="fact__key num">{t}</span>
                      <span className="fact__value">在「MCP 與 AI」關閉</span>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}

          <p className="hint">{tools.note}</p>
        </section>
      </div>
    </div>
  );
}
