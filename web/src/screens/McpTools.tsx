import { useEffect, useState } from 'react';
import {
  aiAuditList,
  businessStamp,
  businessTime,
  businessToday,
  callOp,
  getOps,
  updatedMcpToolGrant,
  type AuditEntry,
  type OpInfo,
} from '../api/client';
import { useCurrentUser } from '../auth';
import { DateField } from '../components/DateField';

const SELF = 'set_mcp_tool_grant';
const EXPORT_LIMIT = 10000;

const CSV_HEADERS = [
  '時間',
  '來源',
  '身分',
  '角色',
  '來源位址',
  '原始提問',
  '結果',
  '操作名稱',
] as const;

function csvCell(value: string): string {
  return `"${value.replace(/"/g, '""')}"`;
}

function csvOf(rows: AuditEntry[]): string {
  const lines = rows.map((a) =>
    [
      businessStamp(a.at),
      a.channel === 'mcp' ? 'AI' : '人員',
      a.actor,
      a.actor_role_name,
      a.client_ip ?? '',
      a.prompt ?? '',
      (a.outcome === 'blocked' ? '已擋下 ' : '') + a.result_summary,
      a.tool_name,
    ].map(csvCell).join(','),
  );
  return '﻿' + [CSV_HEADERS.join(','), ...lines].join('\r\n');
}

function download(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/csv;charset=utf-8' }));
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

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
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  const [loadingAudit, setLoadingAudit] = useState(false);
  const [exporting, setExporting] = useState(false);

  function auditRange() {
    return { from: from || undefined, to: to || undefined };
  }

  function loadAudit() {
    setLoadingAudit(true);
    callOp('list_ai_audit', { limit: 100, ...auditRange() }, undefined, aiAuditList)
      .then((d) => setAudit(d.entries))
      .catch((e: Error) => {
        setAudit([]);
        onToast(e.message);
      })
      .finally(() => setLoadingAudit(false));
  }

  function exportAudit() {
    setExporting(true);
    callOp('list_ai_audit', { limit: EXPORT_LIMIT, ...auditRange() }, undefined, aiAuditList)
      .then((d) => {
        if (d.entries.length === 0) {
          onToast('這段期間沒有稽核紀錄');
          return;
        }
        download(`稽核紀錄-${from || '起始'}-${to || businessToday()}.csv`, csvOf(d.entries));
        onToast(
          d.entries.length === EXPORT_LIMIT
            ? `已匯出 ${EXPORT_LIMIT} 筆，達單次上限；請縮短日期區間再匯出一次`
            : `已匯出 ${d.entries.length} 筆`,
        );
      })
      .catch((e: Error) => onToast(e.message))
      .finally(() => setExporting(false));
  }

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
    callOp('list_ai_audit', { limit: 100 }, undefined, aiAuditList)
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
      <>
      <div className="toolbar toolbar--spaced">
        <DateField value={from} onChange={setFrom} className="input input--date" />
        <span className="cell-muted">至</span>
        <DateField value={to} onChange={setTo} className="input input--date" />
        <button className="btn" disabled={loadingAudit} onClick={loadAudit}>
          查詢
        </button>
        <button className="btn" disabled={exporting} onClick={exportAudit}>
          匯出 CSV
        </button>
      </div>

      <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th>時間</th>
              <th>來源</th>
              <th>身分</th>
              <th>來源位址</th>
              <th>原始提問</th>
              <th>結果</th>
              <th>操作名稱</th>
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
                <td className="num cell-muted">{a.client_ip ?? '—'}</td>
                <td className="cell-muted">{a.prompt ?? '—'}</td>
                <td>
                  {a.outcome === 'blocked' && <span className="tag tag--bad">已擋下</span>}{' '}
                  {a.result_summary}
                </td>
                <td className="num cell-muted">{a.tool_name}</td>
              </tr>
            ))}
            {audit.length === 0 && !loadingAudit && (
              <tr>
                <td colSpan={7} className="cell-muted">
                  這段期間沒有稽核紀錄
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      </>
      )}
    </div>
  );
}
