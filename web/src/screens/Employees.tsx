import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  maskId,
  money,
  OpFailed,
  trimDecimal,
  type DimensionList,
  type EmployeeList,
  type EmployeeRow,
} from '../api/client';
import { DateField } from '../components/DateField';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

const EMPLOYMENT_TYPES: { value: string; label: string }[] = [
  { value: 'full_time', label: '正職' },
  { value: 'part_time', label: '兼職' },
  { value: 'contract', label: '約聘' },
];

const STATUSES: { value: string; label: string }[] = [
  { value: 'active', label: '在職' },
  { value: 'leaving', label: '離職中' },
  { value: 'left', label: '已離職' },
];

interface BankAccountForm {
  bank_name: string;
  account_no: string;
  is_payroll: boolean;
  note: string;
}

interface FormState {
  emp_no: string;
  name: string;
  dept_code: string;
  title: string;
  employment_type: string;
  hired_on: string;
  left_on: string;
  status: string;
  base_salary: string;
  labor_insured_salary: string;
  health_insured_salary: string;
  pension_wage: string;
  id_no: string;
  bank_accounts: BankAccountForm[];
  note: string;
}

const EMPTY_FORM: FormState = {
  emp_no: '',
  name: '',
  dept_code: '',
  title: '',
  employment_type: 'full_time',
  hired_on: '',
  left_on: '',
  status: 'active',
  base_salary: '',
  labor_insured_salary: '',
  health_insured_salary: '',
  pension_wage: '',
  id_no: '',
  bank_accounts: [],
  note: '',
};

export function Employees({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<EmployeeList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [q, setQ] = useState('');
  const [includeLeft, setIncludeLeft] = useState(false);
  const [showIds, setShowIds] = useState(false);

  const [drawer, setDrawer] = useState<'create' | 'edit' | null>(null);
  const [current, setCurrent] = useState<EmployeeRow | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [missing, setMissing] = useState<string[]>([]);
  const [dims, setDims] = useState<DimensionList | null>(null);

  const load = useCallback(() => {
    setError(null);
    callOp<EmployeeList>('list_employees', {
      include_left: includeLeft,
      ...(q.trim() ? { q: q.trim() } : {}),
    })
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, [q, includeLeft]);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    if (!drawer) return;
    if (!dims) callOp<DimensionList>('list_dimensions').then(setDims).catch(() => setDims(null));
  }, [drawer, dims]);

  function openCreate() {
    setCurrent(null);
    setForm(EMPTY_FORM);
    setMissing([]);
    setDrawer('create');
  }

  function openEdit(e: EmployeeRow) {
    setCurrent(e);
    setForm({
      emp_no: e.emp_no,
      name: e.name,
      dept_code: e.dept_code ?? '',
      title: e.title,
      employment_type: e.employment_type,
      hired_on: e.hired_on,
      left_on: e.left_on ?? '',
      status: e.status,

      base_salary: '',
      labor_insured_salary: '',
      health_insured_salary: '',
      pension_wage: '',

      id_no: e.id_no,
      bank_accounts: e.bank_accounts.map((b) => ({
        bank_name: b.bank_name,
        account_no: b.account_no,
        is_payroll: b.is_payroll,
        note: b.note,
      })),
      note: e.note,
    });
    setMissing([]);
    setDrawer('edit');
  }

  function currentAmount(
    field: 'base_salary' | 'labor_insured_salary' | 'health_insured_salary' | 'pension_wage',
  ): string {
    const value = current?.[field];
    if (!value || Number(value) === 0) return '尚未設定';
    return `目前 ${money(value)}`;
  }

  function amountFields(f: FormState): Record<string, string> {
    const keys = [
      'base_salary',
      'labor_insured_salary',
      'health_insured_salary',
      'pension_wage',
    ] as const;
    const out: Record<string, string> = {};
    for (const k of keys) {
      const v = f[k].trim();
      if (v) out[k] = v;
    }
    return out;
  }

  function setField<K extends keyof FormState>(key: K, value: FormState[K]) {
    setForm((f) => ({ ...f, [key]: value }));
  }

  function setBankAccounts(next: BankAccountForm[]) {
    setForm((f) => ({ ...f, bank_accounts: next }));
  }

  function setBankField<K extends keyof BankAccountForm>(
    index: number,
    key: K,
    value: BankAccountForm[K],
  ) {
    setBankAccounts(
      form.bank_accounts.map((b, i) => (i === index ? { ...b, [key]: value } : b)),
    );
  }

  function addBankAccount() {
    setBankAccounts([
      ...form.bank_accounts,
      {
        bank_name: '',
        account_no: '',
        is_payroll: form.bank_accounts.length === 0,
        note: '',
      },
    ]);
  }

  function setPayrollAccount(index: number) {
    setBankAccounts(form.bank_accounts.map((b, i) => ({ ...b, is_payroll: i === index })));
  }

  function removeBankAccount(index: number) {
    const left = form.bank_accounts.filter((_, i) => i !== index);
    const orphaned = left.length > 0 && !left.some((b) => b.is_payroll);
    setBankAccounts(left.map((b, i) => (orphaned && i === 0 ? { ...b, is_payroll: true } : b)));
  }

  async function save() {
    const blank: string[] = [];
    if (!form.name.trim()) blank.push('name');
    if (!form.hired_on) blank.push('hired_on');
    setMissing(blank);
    if (blank.length > 0) {
      onToast(`還要填：${blank.map((k) => (k === 'name' ? '姓名' : '到職日')).join('、')}`);
      return;
    }
    if ((form.status === 'leaving' || form.status === 'left') && !form.left_on) {
      onToast('狀態為「離職中」或「已離職」時，離職日為必填');
      return;
    }
    const accounts = form.bank_accounts.filter(
      (b) => b.bank_name.trim() || b.account_no.trim() || b.note.trim(),
    );
    if (accounts.some((b) => !b.bank_name.trim() || !b.account_no.trim())) {
      onToast('銀行帳戶要填銀行名稱與帳號；不再使用的帳戶請按 × 刪除');
      return;
    }
    setSaving(true);
    try {
      const input = {
        emp_no: form.emp_no.trim(),
        name: form.name.trim(),
        dept_code: form.dept_code || null,
        title: form.title.trim(),
        employment_type: form.employment_type,
        hired_on: form.hired_on,
        left_on: form.left_on || null,
        status: form.status,
        ...amountFields(form),
        id_no: form.id_no.trim(),
        bank_accounts: accounts.map((b) => ({
          bank_name: b.bank_name.trim(),
          account_no: b.account_no.trim(),
          is_payroll: b.is_payroll,
          note: b.note.trim(),
        })),
        note: form.note.trim(),
      };
      const op = drawer === 'edit' ? 'update_employee' : 'create_employee';
      const d = await callOp<{ employee: { emp_no: string; name: string } }>(
        op,
        input,
        drawer === 'edit' ? `更新員工 ${form.emp_no}` : `新增員工 ${form.name}`,
      );
      onToast(`${d.employee.emp_no} ${d.employee.name} 已儲存`);
      setDrawer(null);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  const deptValues = dims?.dimensions.find((d) => d.code === 'department')?.values ?? [];

  return (
    <div>
      <h1 className="page-title">員工名冊</h1>
      {data && (
        <p className="page-lede">
          在職 {data.active} 人，平均年資 {data.avg_service_label}。
        </p>
      )}

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>

          <p className="recon">{data.salary_note}</p>

          <div className="toolbar">
            <input
              className="entry-input entry-input--search"
              placeholder="找姓名、員工編號、職稱或部門"
              value={q}
              onChange={(e) => setQ(e.target.value)}
            />
            <button
              className={`chip${includeLeft ? ' chip--on' : ''}`}
              onClick={() => setIncludeLeft((v) => !v)}
            >
              併同顯示已離職
            </button>
            <button
              className={`chip${showIds ? ' chip--on' : ''}`}
              onClick={() => setShowIds((v) => !v)}
            >
              顯示身分證字號
            </button>
            <span className="spacer" />
            <span className="section-note">
              {data.by_dept.map((d) => `${d.label} ${d.count}`).join('　')}
            </span>
            <button className="btn btn--compact" onClick={openCreate}>
              ＋ 新增員工
            </button>
          </div>

          {data.employees.length === 0 ? (
            <div className="placeholder">
              <div className="placeholder__title">名冊尚無資料</div>
              <p>請由上方「＋ 新增員工」建立第一筆。</p>
            </div>
          ) : (
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>員工</th>
                    <th>部門與職稱</th>
                    <th>到職</th>
                    <th className="align-right">年資</th>
                    <th className="align-right">特休</th>
                    <th>狀態</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {data.employees.map((e) => (
                    <tr key={e.emp_no} className={e.status === 'left' ? 'row--waiting' : undefined}>
                      <td>
                        <button className="link-cell" onClick={() => openEdit(e)}>
                          {e.name}
                        </button>
                        <span className="cell-hint cell-hint--block num">
                          {e.emp_no} ·{' '}
                          {e.id_no ? (showIds ? e.id_no : maskId(e.id_no)) : '未登錄身分證字號'}
                        </span>
                      </td>
                      <td className="cell-accounts">
                        {e.dept_label ?? '未指定'}
                        <span className="cell-hint cell-hint--block">
                          {e.title ? `${e.title} · ` : ''}
                          {e.employment_label}
                        </span>
                      </td>
                      <td className="num cell-nowrap">
                        {e.hired_on}
                        {e.left_on && (
                          <span className="cell-hint cell-hint--block num">
                            離職 {e.left_on}
                          </span>
                        )}
                      </td>
                      <td className="num align-right">{e.service_label}</td>
                      <td className="num align-right">
                        {trimDecimal(e.annual_leave_left)} 日
                        <span className="cell-hint cell-hint--block num">
                          額度 {e.annual_leave_days} · 已休 {trimDecimal(e.annual_leave_taken)}
                        </span>
                      </td>
                      <td>
                        {e.status === 'active' ? (
                          <span className="cell-muted">{e.status_label}</span>
                        ) : (
                          <span
                            className={`tag tag--static${e.status === 'leaving' ? ' tag--warn' : ''}`}
                          >
                            {e.status_label}
                          </span>
                        )}
                        {e.note && <span className="cell-hint cell-hint--block">{e.note}</span>}
                      </td>
                      <td className="align-right">
                        <button className="btn btn--compact btn--secondary" onClick={() => openEdit(e)}>
                          編輯
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          <p className="hint">
            特休額度依年資與特休規則{' '}
            <span className="num">{data.annual_leave_ruleset.version}</span>{' '}
            計算（{data.annual_leave_ruleset.effective_from} 起適用）。
          </p>
        </>
      )}

      {drawer && (
        <div className="overlay" onClick={() => setDrawer(null)}>
          <aside className="drawer" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">{drawer === 'edit' ? `編輯 ${form.emp_no}` : '新增員工'}</div>
                <div className="drawer__subtitle">登入帳號另在「帳號管理」建立</div>
              </div>
              <button className="btn btn--close" onClick={() => setDrawer(null)} aria-label="關閉">
                ×
              </button>
            </div>

            <div className="drawer__body">
              <div className="field-grid">
                <label className="field">
                  員工編號
                  <input
                    className="input num"
                    value={form.emp_no}
                    disabled={drawer === 'edit'}
                    placeholder={drawer === 'edit' ? '' : '留空自動配號'}
                    onChange={(e) => setField('emp_no', e.target.value)}
                  />
                  {drawer === 'edit' && <span className="field__hint">編輯時不可變更員工編號</span>}
                </label>
                <label className="field">
                  姓名
                  <input
                    className="input"
                    value={form.name}
                    onChange={(e) => setField('name', e.target.value)}
                  />
                  {missing.includes('name') && (
                    <span className="field__hint field__hint--missing">未填</span>
                  )}
                </label>

                <label className="field">
                  部門
                  <select
                    className="input"
                    value={form.dept_code}
                    onChange={(e) => setField('dept_code', e.target.value)}
                  >
                    <option value="">未指定</option>
                    {deptValues.map((v) => (
                      <option key={v.code} value={v.code}>
                        {v.label}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  職稱
                  <input
                    className="input"
                    value={form.title}
                    onChange={(e) => setField('title', e.target.value)}
                  />
                </label>

                <label className="field">
                  僱用型態
                  <select
                    className="input"
                    value={form.employment_type}
                    onChange={(e) => setField('employment_type', e.target.value)}
                  >
                    {EMPLOYMENT_TYPES.map((t) => (
                      <option key={t.value} value={t.value}>
                        {t.label}
                      </option>
                    ))}
                  </select>
                </label>

                <label className="field">
                  到職日
                  <DateField
                    className="input"
                    value={form.hired_on}
                    onChange={(v) => setField('hired_on', v)}
                  />
                  {missing.includes('hired_on') && (
                    <span className="field__hint field__hint--missing">未填</span>
                  )}
                </label>
                <label className="field">
                  狀態
                  <select
                    className="input"
                    value={form.status}
                    onChange={(e) => setField('status', e.target.value)}
                  >
                    {STATUSES.map((s) => (
                      <option key={s.value} value={s.value}>
                        {s.label}
                      </option>
                    ))}
                  </select>
                </label>

                {form.status !== 'active' && (
                  <label className="field field--full">
                    離職日（狀態不是在職時必填）
                    <DateField
                      className="input"
                      value={form.left_on}
                      onChange={(v) => setField('left_on', v)}
                    />
                  </label>
                )}

                <label className="field field--full">
                  身分證字號
                  <input
                    className="input num"
                    value={form.id_no}
                    onChange={(e) => setField('id_no', e.target.value)}
                  />
                  <span className="field__hint">
                    勞保申報與扣繳憑單都要用到；還沒收到可以留空。
                  </span>
                </label>
              </div>

              <p className="form-warn">
                {drawer === 'edit'
                  ? '留空的欄位不會更動，要清成尚未設定請填 0。'
                  : '留空表示尚未談定，薪資作業會擋下這個人。'}
              </p>

              <div className="field-grid">
                <label className="field">
                  本薪
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.base_salary}
                    placeholder={drawer === 'edit' ? currentAmount('base_salary') : '38000'}
                    onChange={(e) => setField('base_salary', e.target.value)}
                  />
                </label>
                <label className="field">
                  勞保投保薪資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.labor_insured_salary}
                    placeholder={drawer === 'edit' ? currentAmount('labor_insured_salary') : '38200'}
                    onChange={(e) => setField('labor_insured_salary', e.target.value)}
                  />
                </label>
                <label className="field">
                  健保投保薪資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.health_insured_salary}
                    placeholder={drawer === 'edit' ? currentAmount('health_insured_salary') : '38200'}
                    onChange={(e) => setField('health_insured_salary', e.target.value)}
                  />
                </label>
                <label className="field">
                  勞退提繳工資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.pension_wage}
                    placeholder={drawer === 'edit' ? currentAmount('pension_wage') : '40100'}
                    onChange={(e) => setField('pension_wage', e.target.value)}
                  />
                </label>
              </div>

              <div className="toolbar">
                <h3 className="section-title">銀行帳戶</h3>
                <span className="spacer" />
                <button className="btn btn--compact" onClick={addBankAccount}>
                  ＋ 新增帳戶
                </button>
              </div>

              {form.bank_accounts.length === 0 ? (
                <p className="section-note">尚未登錄帳戶。</p>
              ) : (
                <>
                  <div className="bank-grid entry-head">
                    <div>銀行</div>
                    <div>帳號</div>
                    <div>用途</div>
                    <div>薪轉帳戶</div>
                    <div />
                  </div>
                  {form.bank_accounts.map((b, i) => (
                    <div className="bank-grid entry-row" key={i}>
                      <input
                        className="input"
                        placeholder="玉山銀行 城中分行"
                        value={b.bank_name}
                        onChange={(e) => setBankField(i, 'bank_name', e.target.value)}
                      />
                      <input
                        className="input num"
                        placeholder="0123456789012"
                        value={b.account_no}
                        onChange={(e) => setBankField(i, 'account_no', e.target.value)}
                      />
                      <input
                        className="input"
                        placeholder="報支退款"
                        value={b.note}
                        onChange={(e) => setBankField(i, 'note', e.target.value)}
                      />
                      <button
                        className={`chip${b.is_payroll ? ' chip--on' : ''}`}
                        onClick={() => setPayrollAccount(i)}
                      >
                        {b.is_payroll ? '薪轉帳戶' : '設為薪轉帳戶'}
                      </button>
                      <button
                        className="btn btn--icon"
                        onClick={() => removeBankAccount(i)}
                        aria-label="刪除本列帳戶"
                      >
                        ×
                      </button>
                    </div>
                  ))}
                </>
              )}

              <label className="field field--full">
                備註
                <textarea
                  className="input"
                  rows={2}
                  value={form.note}
                  onChange={(e) => setField('note', e.target.value)}
                />
              </label>
            </div>

            <div className="drawer__foot">
              <span className="drawer__foot-note">
                {drawer === 'edit' ? '更新本筆員工主檔' : '建立員工主檔'}
              </span>
              <button className={`btn${saving ? ' btn--off' : ''}`} disabled={saving} onClick={() => void save()}>
                {saving ? '存檔中…' : '存檔'}
              </button>
            </div>
          </aside>
        </div>
      )}
    </div>
  );
}
