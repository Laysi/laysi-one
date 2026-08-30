import { useEffect, useState } from 'react';
import { callOp, myHome, OpFailed, trimDecimal, type MyHome } from '../api/client';
import type { ScreenId } from '../nav';
import { RulesetNote } from '../components/RulesetNote';

interface Props {
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
}

export function Me({ onGo, reloadKey }: Props) {
  const [data, setData] = useState<MyHome | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    callOp('get_my_home', {}, undefined, myHome)
      .then((d) => live && setData(d))
      .catch((e) => {
        if (!live) return;

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
          <div className="stat-card__kicker">休假待簽核</div>
          <div className="stat-card__value num">{data.pending.leave_requests}</div>
          <div className="stat-card__note num">張 · 累計送出 {leaves.length} 張</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">請款進行中</div>
          <div className="stat-card__value num">{data.pending.expense_claims}</div>
          <div className="stat-card__note num">張 · 累計送出 {claims.length} 張</div>
        </div>
      </div>

      <div className="toolbar">
        <button className="btn btn--compact" onClick={() => onGo('meleavenew')}>
          休假申請 →
        </button>
        <button className="btn btn--compact" onClick={() => onGo('meexpnew')}>
          請款申請 →
        </button>
      </div>

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
              {p.dept_label ?? '未指定'}
              {p.title ? ` · ${p.title}` : ''}
            </span>
          </div>
          <div className="fact">
            <span className="fact__key">在職狀態</span>
            <span className="fact__value">
              {p.status_label}
              <span className="fact__note num">
                {p.employment_label} · {p.pay_basis_label} · {p.hired_on} 起
                {p.left_on ? ` · ${p.left_on} 離職` : ''}
              </span>
            </span>
          </div>
          {p.bank_accounts.length === 0 ? (
            <div className="fact">
              <span className="fact__key">銀行帳戶</span>
              <span className="fact__value">尚未登錄，由人資維護</span>
            </div>
          ) : (
            p.bank_accounts.map((b, i) => (
              <div className="fact" key={i}>
                <span className="fact__key">{b.is_payroll ? '薪轉帳戶' : '其他帳戶'}</span>
                <span className="fact__value num">
                  {b.bank_name ? `${b.bank_name} · ` : ''}
                  {b.account_masked}
                  {b.note && <span className="fact__note">{b.note}</span>}
                </span>
              </div>
            ))
          )}
        </div>
      </section>

      <RulesetNote items={[{ label: '特休', source: al.ruleset.source_note }]} />
    </div>
  );
}
