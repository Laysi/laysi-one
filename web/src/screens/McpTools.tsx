import { useEffect, useState } from 'react';
import { callOp } from '../api/client';
import type { Risk } from '../api/generated';

interface OpInfo {
  name: string;
  risk: Risk;
  risk_label: string;
  summary: string;
  enabled: boolean;
}

interface AuditEntry {
  at: string;
  actor: string;
  tool_name: string;
  risk: string;
  prompt: string | null;
  channel: string;
  result_summary: string;
  outcome: string;
}

export function McpTools({ reloadKey }: { reloadKey: number }) {
  const [ops, setOps] = useState<OpInfo[] | null>(null);
  const [audit, setAudit] = useState<AuditEntry[]>([]);

  useEffect(() => {
    fetch('/v1/ops')
      .then((r) => r.json())
      .then((d: { operations: OpInfo[] }) => setOps(d.operations))
      .catch(() => setOps([]));
    callOp<{ entries: AuditEntry[] }>('list_ai_audit', { limit: 20 })
      .then((d) => setAudit(d.entries))
      .catch(() => setAudit([]));
  }, [reloadKey]);

  return (
    <div>
      <h1 className="page-title">MCP 與 AI</h1>
      <p className="page-lede">
        每一個操作同時是一個 MCP 工具。畫面與 AI 使用同一份操作註冊表，並非另行實作的
        介面，因此兩者的可用範圍一致。
      </p>

      <h2 className="section-title">工具與風險分級</h2>
      <p className="section-note">
        此處的開關僅作用於 AI。關閉 <span className="num">post_voucher</span> 後，AI
        無法過帳，人員仍可於「傳票 · 總帳」執行。
      </p>

      {!ops && <p className="state-note">讀取中…</p>}
      {ops && (
        <table className="table">
          <thead>
            <tr>
              <th>工具</th>
              <th>說明</th>
              <th>風險</th>
              <th>AI 可用</th>
            </tr>
          </thead>
          <tbody>
            {ops.map((o) => (
              <tr key={o.name}>
                <td className="num">{o.name}</td>
                <td>{o.summary}</td>
                <td>
                  <span className={`tag${o.risk === 'read_only' ? '' : ' tag--warn'}`}>
                    {o.risk_label}
                  </span>
                </td>
                <td>{o.enabled ? '開啟' : <span className="cell-muted">關閉</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h2 className="section-title section-title--spaced">稽核軌跡</h2>
      <p className="section-note">
        唯讀操作同樣寫入稽核，AI 讀取帳務本身即為應留存的事實。被權限擋下的呼叫也留紀錄。
      </p>
      <table className="table">
        <thead>
          <tr>
            <th>時間</th>
            <th>來源</th>
            <th>身分</th>
            <th>工具</th>
            <th>原始提問</th>
            <th>結果</th>
          </tr>
        </thead>
        <tbody>
          {audit.map((a, i) => (
            <tr key={i}>
              <td className="num cell-muted">{a.at.slice(5, 16).replace('T', ' ')}</td>
              <td>
                <span className={`tag${a.channel === 'mcp' ? ' tag--warn' : ''}`}>
                  {a.channel === 'mcp' ? 'AI' : '人員'}
                </span>
              </td>
              <td>{a.actor}</td>
              <td className="num">{a.tool_name}</td>
              <td className="cell-muted">{a.prompt ?? '—'}</td>
              <td>
                {a.outcome === 'blocked' && <span className="tag tag--bad">已擋下</span>}{' '}
                {a.result_summary}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
