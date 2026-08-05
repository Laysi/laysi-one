import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  OpFailed,
  trimDecimal,
  type MyLeaveList,
  type MyLeaveRequest,
  type MyLeaveResult,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

/** Value of the "other" option. Never a real leave type code. */
const OTHER = '__other__';

/** Status label for a leave request. Pending ones stand out, rejected ones
 *  more so. */
function statusTag(status: string): string {
  if (status === 'rejected') return 'tag tag--bad tag--static';
  if (status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

/**
 * Leave type options.
 *
 * No list is invented here. The valid values are the CHECK on
 * `leave_request.kind`, and no backend operation currently exposes that
 * list, so the only thing the frontend can honestly offer are the types
 * that appear in the caller's own records, with both the code and the
 * display name supplied by the backend. Copying a list into the frontend
 * would read more smoothly until the day it disagrees with the database and
 * the screen confidently submits a type that does not exist.
 *
 * Types outside that list go through "other" with a code entered by hand.
 * When it is wrong the backend replies with every type it recognises, and
 * that sentence is shown verbatim under the form.
 */
function kindOptions(requests: MyLeaveRequest[]): { kind: string; label: string }[] {
  const seen = new Map<string, string>();
  for (const r of requests) {
    if (!seen.has(r.kind)) seen.set(r.kind, r.kind_label);
  }
  return [...seen].map(([kind, label]) => ({ kind, label }));
}

export function MyLeave({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MyLeaveList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** The rejection message from submitting. The backend's wording, unedited. */
  const [formError, setFormError] = useState<string | null>(null);
  const [result, setResult] = useState<MyLeaveResult | null>(null);

  // The form. Days is a string: half a day is 0.5, and that decimal must not
  // be lost in conversion.
  const [kind, setKind] = useState('');
  const [otherKind, setOtherKind] = useState('');
  const [fromOn, setFromOn] = useState('');
  const [toOn, setToOn] = useState('');
  const [days, setDays] = useState('');
  const [reason, setReason] = useState('');

  const load = useCallback(() => {
    setError(null);
    callOp<MyLeaveList>('list_my_leave_requests')
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

  const chosenKind = kind === OTHER ? otherKind.trim() : kind;
  const ready = chosenKind !== '' && fromOn !== '' && toOn !== '' && days.trim() !== '';

  async function submit() {
    setBusy(true);
    setFormError(null);
    try {
      const d = await callOp<MyLeaveResult>(
        'request_leave',
        {
          kind: chosenKind,
          from_on: fromOn,
          to_on: toOn,
          // Strings in, strings out. The backend accepts a decimal string, not
          // a float.
          days: days.trim(),
          ...(reason.trim() ? { reason: reason.trim() } : {}),
        },
        `申請${chosenKind}，${fromOn} 至 ${toOn} 共 ${days.trim()} 日`,
      );
      setResult(d);
      onToast(
        `${d.request.leave_no} 已送出：${d.request.kind_label} ` +
          `${trimDecimal(d.request.days)} 日，待主管簽核`,
      );
      setKind('');
      setOtherKind('');
      setFromOn('');
      setToOn('');
      setDays('');
      setReason('');
      onChanged();
      load();
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      setFormError(msg);
      onToast(msg);
    } finally {
      setBusy(false);
    }
  }

  if (error) {
    return (
      <div>
        <h1 className="page-title">我的請假</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          請假單的對象是登入帳號上的 <span className="num">employee_id</span>，
          帳號未連結員工資料時回覆錯誤；送出請假單也一樣，不會為查無資料的對象開立單據。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const al = data.annual_leave;
  const options = kindOptions(data.requests);

  return (
    <div>
      <h1 className="page-title">我的請假</h1>
      <p className="page-lede">特休餘額、請假紀錄與新增申請。</p>

      <div className="stat-grid">
        <div className="card stat-card">
          <div className="stat-card__kicker">{al.year} 年特休額度</div>
          <div className="stat-card__value num">{trimDecimal(al.entitlement)}</div>
          <div className="stat-card__note">日 · 依年資即時計算</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">已休</div>
          <div className="stat-card__value num">{trimDecimal(al.taken)}</div>
          <div className="stat-card__note">日 · 僅計已核准</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">餘額</div>
          <div className="stat-card__value num">{trimDecimal(al.remaining)}</div>
          <div className="stat-card__note">日</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">待簽核</div>
          <div className="stat-card__value num">{data.pending_count}</div>
          <div className="stat-card__note">張 · 累計送出 {data.requests.length} 張</div>
        </div>
      </div>

      <p className="recon">{data.scope_note}</p>

      <div className="split-columns split-columns--wide-left">
        <section>
          <h3 className="section-title">本人歷史請假紀錄</h3>
          <table className="table">
            <thead>
              <tr>
                <th>請假單</th>
                <th>假別</th>
                <th>期間</th>
                <th className="align-right">天數</th>
                <th>狀態</th>
              </tr>
            </thead>
            <tbody>
              {data.requests.map((r) => (
                <tr
                  key={r.leave_no}
                  className={r.status === 'pending' ? 'row--pending' : undefined}
                >
                  <td className="num">
                    {r.leave_no}
                    <span className="cell-hint cell-hint--block">{r.created_at.slice(0, 10)} 送出</span>
                  </td>
                  <td>
                    {r.kind_label}
                    {r.reason && <span className="cell-hint cell-hint--block">{r.reason}</span>}
                  </td>
                  <td className="num cell-nowrap">
                    {r.from_on} 至 {r.to_on}
                  </td>
                  <td className="num align-right">{trimDecimal(r.days)}</td>
                  <td>
                    <span className={statusTag(r.status)}>{r.status_label}</span>
                    {r.status !== 'pending' && r.decided_by && (
                      <span className="cell-hint cell-hint--block">
                        {r.decided_by}
                        {r.decided_role && `（${r.decided_role}）`}
                        {r.decision_note && ` · ${r.decision_note}`}
                      </span>
                    )}
                    {r.status === 'pending' && (
                      <span className="cell-hint cell-hint--block">待主管簽核</span>
                    )}
                  </td>
                </tr>
              ))}
              {data.requests.length === 0 && (
                <tr>
                  <td colSpan={5} className="cell-muted">
                    尚無請假紀錄。
                  </td>
                </tr>
              )}
            </tbody>
          </table>

          <p className="hint">{al.note}</p>
          <p className="hint">
            額度依年資與 <span className="num">annual_leave {al.ruleset.version}</span>{' '}
            即時計算（{al.ruleset.effective_from} 起適用），
            <strong>資料庫未設「特休額度」欄位</strong>。
            依據：{al.ruleset.source_note}
          </p>
        </section>

        <section>
          <h3 className="section-title">新增請假申請</h3>
          <div className="card form-card">
            <label className="field">
              假別
              <select className="input" value={kind} onChange={(e) => setKind(e.target.value)}>
                <option value="">請選擇</option>
                {options.map((o) => (
                  <option key={o.kind} value={o.kind}>
                    {o.label}
                  </option>
                ))}
                <option value={OTHER}>其他（自行輸入代號）</option>
              </select>
            </label>

            {kind === OTHER && (
              <label className="field">
                假別代號
                <input
                  className="input"
                  placeholder="例如 sick"
                  value={otherKind}
                  onChange={(e) => setOtherKind(e.target.value)}
                />
              </label>
            )}

            <div className="field-row">
              <label className="field field--date">
                從
                <input
                  className="input input--date"
                  type="date"
                  value={fromOn}
                  onChange={(e) => setFromOn(e.target.value)}
                />
              </label>
              <label className="field field--date">
                到
                <input
                  className="input input--date"
                  type="date"
                  value={toOn}
                  onChange={(e) => setToOn(e.target.value)}
                />
              </label>
              <label className="field field--grow">
                天數
                <input
                  className="input"
                  placeholder="1 或 0.5"
                  value={days}
                  onChange={(e) => setDays(e.target.value)}
                />
              </label>
            </div>

            <label className="field">
              事由
              <input
                className="input"
                placeholder="選填"
                value={reason}
                onChange={(e) => setReason(e.target.value)}
              />
            </label>

            <button className="btn" disabled={busy || !ready} onClick={() => void submit()}>
              {busy ? '送出中…' : '送出請假單'}
            </button>

            {formError && <p className="state-note state-note--error">{formError}</p>}

            <p className="form-note">
              <strong>請假天數需自行填寫，系統不由日期區間推算。</strong>
              例假日與國定假日規則尚未收錄至規則庫，系統推算的日數看似精確實則不可靠，
              故改由申請人填寫。半日請填 <span className="num">0.5</span>。
            </p>
            <p className="form-note">
              本表單<strong>不含「申請人」欄位</strong>。單據對象由後端依目前登入身分的
              姓名查詢員工名冊決定，畫面無法指定他人。
            </p>
          </div>

          {result && (
            <div className="card doc-card">
              <div className="doc-card__head">
                <h3 className="section-title doc-card__title">
                  {result.request.leave_no} 已送出
                </h3>
                <span className="doc-card__badge">{result.request.status_label}</span>
              </div>
              <div className="doc-row">
                <span>{result.request.kind_label}</span>
                <span className="num">{trimDecimal(result.request.days)} 日</span>
              </div>
              <div className="doc-row doc-row--sub">
                <span>期間</span>
                <span className="num">
                  {result.request.from_on} 至 {result.request.to_on}
                </span>
              </div>
              <p className="hint">{result.days_source}</p>
              <p className="hint">{result.scope_note}</p>
            </div>
          )}

          <p className="hint">
            上方假別清單的代號與名稱取自<strong>本人請假紀錄</strong>，均由後端提供。
            系統目前沒有回傳「全部合法假別」的操作，故前端不自行維護可能與資料庫不符的
            清單。清單未列的假別請以「其他」填入代號；代號有誤時，後端會列出目前可用的
            全部代號，並顯示於按鈕下方。
          </p>
          <p className="hint">
            送出後單據狀態為待簽核，<strong>核准與否由主管於人資模組決定</strong>；
            狀態非「待簽核」時，資料層會擋下任何重複的簽核決定。
          </p>
        </section>
      </div>
    </div>
  );
}
