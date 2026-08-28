import { useEffect, useState } from 'react';
import {
  aiAuditList,
  businessTime,
  callOp,
  getOps,
  updatedMcpToolGrant,
  type AuditEntry,
  type OpInfo,
} from '../api/client';
import { useCurrentUser } from '../auth';

const SELF = 'set_mcp_tool_grant';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

export function McpTools({ onToast, reloadKey }: Props) {
  const me = useCurrentUser();
  const canSwitch = me.permissions.includes('set_mcp_tool_grant');
  const [ops, setOps] = useState<OpInfo[] | null>(null);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [tab, setTab] = useState<'tools' | 'audit'>('tools');
  const [busy, setBusy] = useState<string | null>(null);

  function toggle(op: OpInfo) {
    setBusy(op.name);
    callOp(
      SELF,
      { tool_name: op.name, enabled: !op.enabled },
      undefined,
      updatedMcpToolGrant,
    )
      .then((d) => {
        setOps((list) =>
          (list ?? []).map((o) =>
            o.name === d.grant.tool_name ? { ...o, enabled: d.grant.enabled } : o,
          ),
        );
        onToast(`${d.grant.enabled ? '已開啟' : '已關閉'} ${op.summary.split('：')[0]}`);
      })
      .catch((e: Error) => onToast(e.message))
      .finally(() => setBusy(null));
  }

  useEffect(() => {
    getOps()
      .then(setOps)
      .catch(() => setOps([]));
    callOp('list_ai_audit', { limit: 20 }, undefined, aiAuditList)
      .then((d) => setAudit(d.entries))
      .catch(() => setAudit([]));
  }, [reloadKey]);

  return (
    <div>
      <h1 className="page-title">MCP 與 AI</h1>
      <p className="page-lede">
        本頁的開關只決定 AI 能呼叫哪些工具。
      </p>

      <div className="toolbar toolbar--spaced">
        <button
          className={`chip${tab === 'tools' ? ' chip--on' : ''}`}
          onClick={() => setTab('tools')}
        >
          工具與風險分級
        </button>
        <button
          className={`chip${tab === 'audit' ? ' chip--on' : ''}`}
          onClick={() => setTab('audit')}
        >
          稽核紀錄
        </button>
      </div>

      {tab === 'tools' && !ops && <p className="state-note">讀取中…</p>}
      {tab === 'tools' && ops && (
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th>工具</th>
                <th>風險</th>
                <th>AI 可用</th>
                <th>工具名稱</th>
              </tr>
            </thead>
            <tbody>
              {ops.map((o) => (
                <tr key={o.name}>
                  <td>{o.summary}</td>
                  <td>
                    <span className={`tag${o.risk === 'read_only' ? '' : ' tag--warn'}`}>
                      {o.risk_label}
                    </span>
                  </td>
                  <td>
                    {canSwitch && o.name !== SELF ? (
                      <button
                        className={`chip${o.enabled ? ' chip--on' : ''}`}
                        disabled={busy === o.name}
                        onClick={() => toggle(o)}
                      >
                        {o.enabled ? '開啟' : '關閉'}
                      </button>
                    ) : (
                      <>
                        {o.enabled ? '開啟' : <span className="cell-muted">關閉</span>}
                        {o.name === SELF && (
                          <span className="cell-hint cell-hint--block">無法對 AI 開啟</span>
                        )}
                      </>
                    )}
                  </td>
                  <td className="num cell-muted">{o.name}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {tab === 'audit' && (
      <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th>時間</th>
              <th>來源</th>
              <th>身分</th>
              <th>原始提問</th>
              <th>結果</th>
              <th>工具名稱</th>
            </tr>
          </thead>
          <tbody>
            {audit.map((a, i) => (
              <tr key={i}>
                <td className="num cell-muted">{businessTime(a.at)}</td>
                <td>
                  <span className={`tag${a.channel === 'mcp' ? ' tag--warn' : ''}`}>
                    {a.channel === 'mcp' ? 'AI' : '人員'}
                  </span>
                </td>
                <td>
                  {a.actor}
                  <span className="cell-hint cell-hint--block">{a.actor_role_name}</span>
                </td>
                <td className="cell-muted">{a.prompt ?? '—'}</td>
                <td>
                  {a.outcome === 'blocked' && <span className="tag tag--bad">已擋下</span>}{' '}
                  {a.result_summary}
                </td>
                <td className="num cell-muted">{a.tool_name}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      )}
    </div>
  );
}
