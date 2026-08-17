import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  OpFailed,
  trimDecimal,
  type DimensionList,
  type EmployeeList,
  type EmployeeRow,
  type RoleMatrix,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

const EMPLOYMENT_TYPES: { value: string; label: string }[] = [
  { value: 'full_time', label: '全職' },
  { value: 'part_time', label: '兼職' },
  { value: 'contract', label: '約聘' },
];

const STATUSES: { value: string; label: string }[] = [
  { value: 'active', label: '在職' },
  { value: 'leaving', label: '離職中' },
  { value: 'left', label: '已離職' },
];

interface FormState {
  emp_no: string;
  name: string;
  dept_code: string;
  title: string;
  role_code: string;
  employment_type: string;
  hired_on: string;
  left_on: string;
  status: string;
  base_salary: string;
  labor_insured_salary: string;
  health_insured_salary: string;
  pension_wage: string;
  id_no: string;
  bank_account: string;
  note: string;
}

const EMPTY_FORM: FormState = {
  emp_no: '',
  name: '',
  dept_code: '',
  title: '',
  role_code: 'staff',
  employment_type: 'full_time',
  hired_on: '',
  left_on: '',
  status: 'active',
  base_salary: '',
  labor_insured_salary: '',
  health_insured_salary: '',
  pension_wage: '',
  id_no: '',
  bank_account: '',
  note: '',
};

export function Employees({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<EmployeeList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [q, setQ] = useState('');
  const [includeLeft, setIncludeLeft] = useState(false);

  const [drawer, setDrawer] = useState<'create' | 'edit' | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [dims, setDims] = useState<DimensionList | null>(null);
  const [roles, setRoles] = useState<{ code: string; name: string }[] | null>(null);

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
    if (!roles) {
      callOp<RoleMatrix>('list_roles')
        .then((m) => setRoles(m.roles.map((r) => ({ code: r.code, name: r.name }))))
        .catch(() => setRoles([]));
    }
  }, [drawer, dims, roles]);

  function openCreate() {
    setForm(EMPTY_FORM);
    setDrawer('create');
  }

  function openEdit(e: EmployeeRow) {
    setForm({
      emp_no: e.emp_no,
      name: e.name,
      dept_code: e.dept_code ?? '',
      title: e.title,
      role_code: e.role_code ?? 'staff',
      employment_type: e.employment_type,
      hired_on: e.hired_on,
      left_on: e.left_on ?? '',
      status: e.status,

      base_salary: '',
      labor_insured_salary: '',
      health_insured_salary: '',
      pension_wage: '',

      id_no: e.id_no_masked,
      bank_account: e.bank_account,
      note: e.note,
    });
    setDrawer('edit');
  }

  function setField<K extends keyof FormState>(key: K, value: FormState[K]) {
    setForm((f) => ({ ...f, [key]: value }));
  }

  async function save() {
    if (!form.emp_no.trim() || !form.name.trim() || !form.hired_on) {
      onToast('員工編號、姓名與到職日都要填寫');
      return;
    }
    if (
      !form.base_salary.trim() ||
      !form.labor_insured_salary.trim() ||
      !form.health_insured_salary.trim() ||
      !form.pension_wage.trim()
    ) {
      onToast('本薪與三項投保級距皆為必填，屬申報事實，系統不代為填補');
      return;
    }
    if ((form.status === 'leaving' || form.status === 'left') && !form.left_on) {
      onToast('狀態為「離職中」或「已離職」時，離職日為必填');
      return;
    }
    setSaving(true);
    try {
      const input = {
        emp_no: form.emp_no.trim(),
        name: form.name.trim(),
        dept_code: form.dept_code || null,
        title: form.title.trim(),
        role_code: form.role_code.trim() || 'staff',
        employment_type: form.employment_type,
        hired_on: form.hired_on,
        left_on: form.left_on || null,
        status: form.status,
        base_salary: form.base_salary.trim(),
        labor_insured_salary: form.labor_insured_salary.trim(),
        health_insured_salary: form.health_insured_salary.trim(),
        pension_wage: form.pension_wage.trim(),
        id_no: form.id_no.trim(),
        bank_account: form.bank_account.trim(),
        note: form.note.trim(),
      };
      const op = drawer === 'edit' ? 'update_employee' : 'create_employee';
      const d = await callOp<{ employee: { emp_no: string; name: string } }>(
        op,
        input,
        drawer === 'edit' ? `更新員工 ${form.emp_no}` : `新增員工 ${form.name}`,
      );
      onToast(
        `${d.employee.emp_no} ${d.employee.name} 已儲存；登入帳號需另行建立`,
      );
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
              <p>
                請由上方「＋ 新增員工」建立第一筆。建立主檔不會一併建立登入帳號，
                登入帳號由系統管理員另行建立。
              </p>
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
                          {e.emp_no} · {e.id_no_masked || '未登錄身分證字號'}
                        </span>
                      </td>
                      <td className="cell-accounts">
                        {e.dept_label ?? '未指定'}
                        <span className="cell-hint cell-hint--block">
                          {e.title} · {e.employment_label}
                          {e.role_name && ` · ${e.role_name}`}
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
            特休額度不逐人設定，每一列均依年資與特休規則{' '}
            <span className="num">{data.annual_leave_ruleset.version}</span>{' '}
            即時計算（{data.annual_leave_ruleset.effective_from} 起適用）。
          </p>
          <p className="hint">{data.id_note}</p>
        </>
      )}

      {drawer && (
        <div className="overlay" onClick={() => setDrawer(null)}>
          <aside className="drawer" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">{drawer === 'edit' ? `編輯 ${form.emp_no}` : '新增員工'}</div>
                <div className="drawer__subtitle">不會一併建立登入帳號</div>
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
                    placeholder="E010"
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
                  系統角色
                  <select
                    className="input"
                    value={form.role_code}
                    onChange={(e) => setField('role_code', e.target.value)}
                  >
                    {(roles ?? [{ code: form.role_code, name: form.role_code }]).map((r) => (
                      <option key={r.code} value={r.code}>
                        {r.name}
                      </option>
                    ))}
                  </select>
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
                  <input
                    className="input"
                    type="date"
                    value={form.hired_on}
                    onChange={(e) => setField('hired_on', e.target.value)}
                  />
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
                    <input
                      className="input"
                      type="date"
                      value={form.left_on}
                      onChange={(e) => setField('left_on', e.target.value)}
                    />
                  </label>
                )}

                <label className="field field--full">
                  身分證字號（只接受遮罩後的值）
                  <input
                    className="input num"
                    value={form.id_no}
                    placeholder="A12*****89"
                    onChange={(e) => setField('id_no', e.target.value)}
                  />
                  <span className="field__hint">
                    可留空。如果要填，請先遮罩；符合完整字號格式者（一個英文字母加九碼數字，例如
                    A123456789）將被後端拒絕，系統無任何功能需要完整字號。
                  </span>
                </label>

                <label className="field field--full">
                  匯薪帳號
                  <input
                    className="input num"
                    value={form.bank_account}
                    onChange={(e) => setField('bank_account', e.target.value)}
                  />
                </label>
              </div>

              <p className="form-warn">
                本薪與三項投保級距依填寫值原樣保存，系統不以勞健保規則校正。
                {drawer === 'edit' &&
                  '基於隱私，名冊不顯示這幾欄，系統也沒有別的管道讀回目前的值。請照實際薪資單與投保申報表重新填寫；留空無法儲存，填錯會直接蓋掉原本的數字。'}
              </p>

              <div className="field-grid">
                <label className="field">
                  本薪
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.base_salary}
                    placeholder={drawer === 'edit' ? '重新輸入目前的本薪' : '38000'}
                    onChange={(e) => setField('base_salary', e.target.value)}
                  />
                </label>
                <label className="field">
                  勞保投保薪資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.labor_insured_salary}
                    placeholder={drawer === 'edit' ? '重新輸入目前的投保薪資' : '38200'}
                    onChange={(e) => setField('labor_insured_salary', e.target.value)}
                  />
                </label>
                <label className="field">
                  健保投保薪資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.health_insured_salary}
                    placeholder={drawer === 'edit' ? '重新輸入目前的投保薪資' : '38200'}
                    onChange={(e) => setField('health_insured_salary', e.target.value)}
                  />
                </label>
                <label className="field">
                  勞退提繳工資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.pension_wage}
                    placeholder={drawer === 'edit' ? '重新輸入目前的提繳工資' : '40100'}
                    onChange={(e) => setField('pension_wage', e.target.value)}
                  />
                </label>
              </div>

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
                {drawer === 'edit' ? '完整更新本筆員工主檔' : '建立員工主檔，不建立登入帳號'}
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
