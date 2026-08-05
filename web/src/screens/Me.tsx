import { useEffect, useState } from 'react';
import { callOp, money, OpFailed, trimDecimal, type MyHome } from '../api/client';
import type { ScreenId } from '../nav';

interface Props {
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
}

/**
 * Status label for a request. Ones still in progress stand out, rejected
 * ones more so, and completed ones stay quiet.
 *
 * Leave uses `pending` while claims use `submitted` / `approving`: different
 * words for the same thing, still with someone else. So only `rejected` and
 * `approved` are recognised and everything else counts as in progress.
 */
function statusTag(status: string): string {
  if (status === 'rejected') return 'tag tag--bad tag--static';
  if (status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function Me({ onGo, reloadKey }: Props) {
  const [data, setData] = useState<MyHome | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    callOp<MyHome>('get_my_home')
      .then((d) => live && setData(d))
      .catch((e) => {
        if (!live) return;
        // The backend has a complete sentence for "this account has no linked
        // employee record"; show it verbatim. Replacing it with "failed to
        // load" or a blank turns "the account is not linked" into "you have
        // no data".
        setError(e instanceof OpFailed ? e.error.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [reloadKey]);

  if (error) {
    return (
      <div>
        <h1 className="page-title">我的</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          五項自助操作均以登入帳號上的 <span className="num">employee_id</span>{' '}
          查詢員工主檔，未連結時回覆錯誤而非空清單；空清單易被誤解為「查無資料」，
          與「帳號尚未連結員工資料」為不同情形。
          請管理員以 <span className="num">laysi-admin</span> 將本帳號連結至對應員工編號。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const p = data.profile;
  const al = data.annual_leave;
  const leaves = data.submissions.leave_requests;
  const claims = data.submissions.expense_claims;

  return (
    <div>
      <h1 className="page-title page-title--hero">個人專區</h1>
      <p className="page-lede">
        本區僅顯示本人的薪資、請假與請款資料。
      </p>

      <div className="stat-grid stat-grid--four">
        <div className="card stat-card">
          <div className="stat-card__kicker">年資</div>
          <div className="stat-card__value">{p.service_label}</div>
          <div className="stat-card__note num">{p.hired_on} 到職</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">特休餘額</div>
          <div className="stat-card__value num">{trimDecimal(al.remaining)}</div>
          <div className="stat-card__note num">
            日 · {al.year} 年額度 {trimDecimal(al.entitlement)}、已休 {trimDecimal(al.taken)}
          </div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">請假待簽核</div>
          <div className="stat-card__value num">{data.pending.leave_requests}</div>
          <div className="stat-card__note">張 · 累計送出 {leaves.length} 張</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">請款進行中</div>
          <div className="stat-card__value num">{data.pending.expense_claims}</div>
          <div className="stat-card__note">張 · 累計送出 {claims.length} 張</div>
        </div>
      </div>

      {/* The point of this module is not what can be seen but what is
          deliberately not offered. */}
      <p className="recon recon--off">{data.identity_note}</p>

      <div className="split-columns">
        <section>
          <h3 className="section-title">我的基本資料</h3>
          <div className="card fact-card">
            <div className="fact">
              <span className="fact__key">姓名與工號</span>
              <span className="fact__value">
                {p.name}
                <span className="fact__note num">{p.emp_no}</span>
              </span>
            </div>
            <div className="fact">
              <span className="fact__key">部門與職稱</span>
              <span className="fact__value">
                {p.dept_label ?? '未指定'} · {p.title}
                {p.role_name && <span className="fact__note">系統角色：{p.role_name}</span>}
              </span>
            </div>
            <div className="fact">
              <span className="fact__key">在職狀態</span>
              <span className="fact__value">
                {p.status === 'active' ? '在職' : p.status}
                <span className="fact__note num">
                  {p.employment_type} · {p.hired_on} 起
                  {p.left_on ? ` · ${p.left_on} 離職` : ''}
                </span>
              </span>
            </div>
            <div className="fact">
              <span className="fact__key">匯薪帳戶</span>
              <span className="fact__value num">
                {p.bank_account_masked}
                <span className="fact__note">
                  由後端遮罩後輸出，完整帳號不離開伺服器。
                </span>
              </span>
            </div>
          </div>
          <p className="hint">{p.privacy_note}</p>

          <h3 className="section-title section-title--spaced">
            {data.attendance_month} 的出勤
          </h3>
          {data.attendance ? (
            <>
              <div className="card fact-card">
                <div className="fact">
                  <span className="fact__key">應出勤 / 實出勤</span>
                  <span className="fact__value num">
                    {trimDecimal(data.attendance.required_days)} /{' '}
                    {trimDecimal(data.attendance.worked_days)} 日
                  </span>
                </div>
                <div className="fact">
                  <span className="fact__key">請假</span>
                  <span className="fact__value num">
                    {trimDecimal(data.attendance.leave_days)} 日
                  </span>
                </div>
                <div className="fact">
                  <span className="fact__key">加班</span>
                  <span className="fact__value num">
                    {trimDecimal(data.attendance.overtime_hours)} 小時 ·{' '}
                    {money(data.attendance.overtime_pay)} 元
                  </span>
                </div>
                <div className="fact">
                  <span className="fact__key">遲到</span>
                  <span className="fact__value num">{data.attendance.late_count} 次</span>
                </div>
                {data.attendance.note && (
                  <div className="fact">
                    <span className="fact__key">備註</span>
                    <span className="fact__value">{data.attendance.note}</span>
                  </div>
                )}
              </div>
              {!data.attendance.overtime_recomputed && (
                <p className="recon recon--off">{data.attendance.overtime_note}</p>
              )}
            </>
          ) : (
            <p className="state-note">{data.attendance_note}</p>
          )}
        </section>

        <section>
          <h3 className="section-title">本人送出的請假單</h3>
          <table className="table">
            <thead>
              <tr>
                <th>假別</th>
                <th>期間</th>
                <th className="align-right">天數</th>
                <th>狀態</th>
              </tr>
            </thead>
            <tbody>
              {leaves.map((r) => (
                <tr key={r.leave_no} className={r.status === 'pending' ? 'row--pending' : undefined}>
                  <td>
                    {r.kind_label}
                    <span className="cell-hint cell-hint--block num">{r.leave_no}</span>
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
                        {r.decision_note && ` · ${r.decision_note}`}
                      </span>
                    )}
                  </td>
                </tr>
              ))}
              {leaves.length === 0 && (
                <tr>
                  <td colSpan={4} className="cell-muted">
                    尚無請假紀錄。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
          <button className="btn btn--compact" onClick={() => onGo('meleave')}>
            前往請假 →
          </button>

          <h3 className="section-title section-title--spaced">本人送出的請款單</h3>
          <table className="table">
            <thead>
              <tr>
                <th>報支內容</th>
                <th className="align-right">金額</th>
                <th>目前關卡</th>
              </tr>
            </thead>
            <tbody>
              {claims.map((c) => (
                <tr key={c.claim_no}>
                  <td>
                    {c.title}
                    <span className="cell-hint cell-hint--block num">
                      {c.claim_no} · {c.spent_on}
                    </span>
                  </td>
                  <td className="num align-right">{money(c.amount)}</td>
                  <td>
                    <span className={statusTag(c.status)}>{c.step_label}</span>
                    {c.voucher_no && (
                      <span className="cell-hint cell-hint--block num">
                        {c.voucher_no}
                        {c.voucher_status === 'posted' ? ' 已過帳' : ' 待過帳'}
                      </span>
                    )}
                  </td>
                </tr>
              ))}
              {claims.length === 0 && (
                <tr>
                  <td colSpan={3} className="cell-muted">
                    尚無請款紀錄。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
          <button className="btn btn--compact" onClick={() => onGo('meexp')}>
            檢視請款進度 →
          </button>
        </section>
      </div>

      <p className="hint">
        本頁沒有任何指定查詢對象的欄位：{' '}
        <strong>五項自助操作的輸入皆為空</strong>
        （請假單僅收假別、日期、日數與事由），對象一律由登入身分決定。
        輸入形狀上不存在指定他人的參數。
      </p>
      <p className="hint">
        身分本身也不是請求說了算：它由 HttpOnly 的 session cookie 查回帳號，
        再由帳號上的 <span className="num">employee_id</span> 決定是誰的資料。
        <strong>請求標頭無法指定操作者，也無法指定角色</strong>，
        因此本層的防護不限於避免誤看。
      </p>
      <p className="hint">
        上方「＋ 建立傳票」是否可用，取決於角色是否具備該項權限：
        <strong>操作分派每次均即時查詢角色權限，未獲授權即拒絕並寫入稽核</strong>。
        畫面不預先隱藏按鈕，被拒絕時可看到擋下的規則，較按鈕消失更有助於排除問題。
      </p>
      <p className="hint">
        特休餘額非資料庫欄位，額度依年資與{' '}
        <span className="num">annual_leave {al.ruleset.version}</span>
        （{al.ruleset.effective_from} 起適用）即時計算，已休天數僅加總本年度已核准的特休。
        依據：{al.ruleset.source_note}
      </p>
    </div>
  );
}
