import { useCallback, useEffect, useRef, useState } from 'react';
import {
  businessTime,
  callOp,
  downloadFile,
  employeeDocumentList,
  employeeList,
  fetchedEmployeeDocument,
  isZero,
  issuedEmployeeLogin,
  updatedEmployee,
  maskId,
  money,
  OpFailed,
  readAsBase64,
  trimDecimal,
  uploadedEmployeeDocument,
  type DocumentRow,
  type EmployeeDocumentList,
  type EmployeeList,
  type EmployeeRow,
} from '../api/client';
import { DateField } from '../components/DateField';
import { RulesetNote } from '../components/RulesetNote';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
  canIssueLogin: boolean;
  canUploadDocument: boolean;
  canReadDocuments: boolean;
}

const EMPLOYMENT_TYPES: { value: string; label: string }[] = [
  { value: 'full_time', label: '正職' },
  { value: 'part_time', label: '兼職' },
  { value: 'contract', label: '約聘' },
];

const PAY_BASES: { value: string; label: string }[] = [
  { value: 'monthly', label: '月薪' },
  { value: 'hourly', label: '時薪' },
];

const INSURED_AS: { value: string; label: string }[] = [
  { value: 'employee', label: '受僱者' },
  { value: 'owner', label: '負責人' },
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

interface DependentForm {
  name: string;
  relation: string;
  id_no: string;
  birthday: string;
  health_enrolled_from: string;
  health_enrolled_until: string;
  saved_from: string;
  removing: boolean;
}

interface FormState {
  emp_no: string;
  name: string;
  dept_code: string;
  title: string;
  employment_type: string;
  pay_basis: string;
  insured_as: string;
  employment_insured: boolean;
  birthday: string;
  hired_on: string;
  left_on: string;
  status: string;
  base_salary: string;
  labor_insured_salary: string;
  health_insured_salary: string;
  pension_wage: string;
  id_no: string;
  bank_accounts: BankAccountForm[];
  dependents: DependentForm[];
  note: string;
}

const EMPTY_FORM: FormState = {
  emp_no: '',
  name: '',
  dept_code: '',
  title: '',
  employment_type: 'full_time',
  pay_basis: 'monthly',
  insured_as: 'employee',
  employment_insured: true,
  birthday: '',
  hired_on: '',
  left_on: '',
  status: 'active',
  base_salary: '',
  labor_insured_salary: '',
  health_insured_salary: '',
  pension_wage: '',
  id_no: '',
  bank_accounts: [],
  dependents: [],
  note: '',
};

export function Employees({
  onToast,
  reloadKey,
  onChanged,
  canIssueLogin,
  canUploadDocument,
  canReadDocuments,
}: Props) {
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

  const [files, setFiles] = useState<EmployeeDocumentList | null>(null);
  const [filesError, setFilesError] = useState<string | null>(null);
  const [uploading, setUploading] = useState<string | null>(null);
  const [fetching, setFetching] = useState<string | null>(null);
  const [kind, setKind] = useState('id_document');
  const picker = useRef<HTMLInputElement>(null);

  const [loginFor, setLoginFor] = useState<EmployeeRow | null>(null);
  const [loginEmail, setLoginEmail] = useState('');
  const [issuing, setIssuing] = useState(false);
  const [issued, setIssued] = useState<{ username: string; password: string | null } | null>(null);

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    callOp(
      'list_employees',
      {
        include_left: includeLeft,
        ...(q.trim() ? { q: q.trim() } : {}),
      },
      undefined,
      employeeList,
    )
      .then((d) => live && setData(d))
      .catch((e: Error) => live && setError(e.message));
    return () => running.current?.();
  }, [q, includeLeft]);

  useEffect(load, [load, reloadKey]);

  const loadFiles = useCallback(
    (empNo: string) => {
      setFilesError(null);
      setFiles(null);
      callOp('list_employee_documents', { emp_no: empNo }, undefined, employeeDocumentList)
        .then(setFiles)
        .catch((e: Error) => setFilesError(e.message));
    },
    [],
  );

  function openCreate() {
    setCurrent(null);
    setForm(EMPTY_FORM);
    setMissing([]);
    setFiles(null);
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
      pay_basis: e.pay_basis,
      insured_as: e.insured_as,
      employment_insured: e.employment_insured,
      birthday: e.birthday ?? '',
      hired_on: e.hired_on,
      left_on: e.left_on ?? '',
      status: e.status,

      base_salary: amountOf(e.base_salary),
      labor_insured_salary: amountOf(e.labor_insured_salary),
      health_insured_salary: amountOf(e.health_insured_salary),
      pension_wage: amountOf(e.pension_wage),

      id_no: e.id_no,
      bank_accounts: e.bank_accounts.map((b) => ({
        bank_name: b.bank_name,
        account_no: b.account_no,
        is_payroll: b.is_payroll,
        note: b.note,
      })),
      dependents: e.dependents.map((d) => ({
        name: d.name,
        relation: d.relation,
        id_no: d.id_no,
        birthday: d.birthday ?? '',
        health_enrolled_from: d.health_enrolled_from,
        health_enrolled_until: d.health_enrolled_until ?? '',
        saved_from: d.health_enrolled_from,
        removing: false,
      })),
      note: e.note,
    });
    setMissing([]);
    setKind('id_document');
    setDrawer('edit');
    if (canReadDocuments) loadFiles(e.emp_no);
  }

  function amountOf(value: string | undefined): string {
    return !value || isZero(value) ? '' : trimDecimal(value);
  }

  type InsuredKey = 'labor_insured_salary' | 'health_insured_salary' | 'pension_wage';

  function insuredFrom(base: string, basis: string): Record<InsuredKey, string> | null {
    const bounds = data?.insured_suggestion;
    const wage = Number(base);
    if (!bounds || basis !== 'monthly' || !Number.isFinite(wage) || wage <= 0) return null;
    const floored = Math.max(wage, Number(bounds.floor));
    return {
      labor_insured_salary: String(Math.min(floored, Number(bounds.accident_ceiling))),
      health_insured_salary: String(floored),
      pension_wage: String(floored),
    };
  }

  function laborOverCeiling(value: string): boolean {
    const bounds = data?.insured_suggestion;
    return !!bounds && Number(value) > Number(bounds.labor_ceiling);
  }

  function accidentChargedOn(value: string): string {
    const bounds = data?.insured_suggestion;
    return String(Math.min(Number(value), Number(bounds?.accident_ceiling ?? value)));
  }

  function setBaseSalary(value: string) {
    setForm((f) => {
      const next = { ...f, base_salary: value };
      const now = insuredFrom(value, f.pay_basis);
      if (!now) return next;
      const was = insuredFrom(f.base_salary, f.pay_basis);
      for (const key of Object.keys(now) as InsuredKey[]) {
        if (f[key] === '' || (was && f[key] === was[key])) next[key] = now[key];
      }
      return next;
    });
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
      const value = f[k].trim();
      const was = drawer === 'edit' ? amountOf(current?.[k]) : '';
      if (value === was) continue;
      out[k] = value === '' ? '0' : value;
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

  function setDependents(next: DependentForm[]) {
    setForm((f) => ({ ...f, dependents: next }));
  }

  function setDependent(index: number, patch: Partial<DependentForm>) {
    setDependents(form.dependents.map((d, i) => (i === index ? { ...d, ...patch } : d)));
  }

  function addDependent() {
    setDependents([
      ...form.dependents,
      {
        name: '',
        relation: data?.relations[0]?.code ?? '',
        id_no: '',
        birthday: '',
        health_enrolled_from: '',
        health_enrolled_until: '',
        saved_from: '',
        removing: false,
      },
    ]);
  }

  function removeDependent(index: number) {
    setDependents(form.dependents.filter((_, i) => i !== index));
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
    const nameless = form.dependents.findIndex((d) => !d.removing && !d.name.trim());
    if (nameless >= 0) {
      onToast(
        form.dependents[nameless]?.saved_from
          ? `第 ${nameless + 1} 位眷屬的姓名要填寫`
          : `第 ${nameless + 1} 位眷屬的姓名要填寫；不再登記的眷屬請按 × 刪除`,
      );
      return;
    }
    const dependents = form.dependents.filter((d) => !d.removing);
    const undated = dependents.find((d) => !d.health_enrolled_from);
    if (undated) {
      onToast(
        undated.saved_from
          ? `${undated.name.trim()} 的健保加保日不能清空；退保請填退保日`
          : `${undated.name.trim()} 的健保加保日要填寫`,
      );
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
        pay_basis: form.pay_basis,
        insured_as: form.insured_as,
        ...(drawer === 'edit' && current?.employment_insured === form.employment_insured
          ? {}
          : { employment_insured: form.employment_insured }),
        birthday: form.birthday || null,
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
        dependents: dependents.map((d) => ({
          name: d.name.trim(),
          relation: d.relation,
          id_no: d.id_no.trim(),
          birthday: d.birthday || null,
          health_enrolled_from: d.health_enrolled_from,
          health_enrolled_until: d.health_enrolled_until || null,
        })),
        note: form.note.trim(),
      };
      const op = drawer === 'edit' ? 'update_employee' : 'create_employee';
      const d = await callOp(
        op,
        input,
        drawer === 'edit' ? `更新員工 ${form.emp_no}` : `新增員工 ${form.name}`,
        updatedEmployee,
      );
      const closed = d.closed_access;
      const shut: string[] = [];
      if (closed && closed.logins.length > 0) shut.push(`停用登入帳號 ${closed.logins.join('、')}`);
      if (closed && closed.tokens > 0) shut.push(`撤銷 ${closed.tokens} 組 MCP Token`);
      onToast(
        shut.length > 0
          ? `${d.employee.emp_no} ${d.employee.name} 已離職，並${shut.join('、')}`
          : `${d.employee.emp_no} ${d.employee.name} 已儲存`,
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

  async function upload(file: File) {
    if (!current) return;
    setUploading(file.name);
    try {
      const d = await callOp(
        'upload_employee_document',
        {
          emp_no: current.emp_no,
          file_name: file.name,
          content_base64: await readAsBase64(file),
          kind,
        },
        `上傳 ${current.name} 的 ${file.name}`,
        uploadedEmployeeDocument,
      );
      onToast(`${d.document.doc_no} ${d.document.version_label} 已上傳`);
      loadFiles(current.emp_no);
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setUploading(null);
      if (picker.current) picker.current.value = '';
    }
  }

  async function download(d: DocumentRow) {
    if (!current) return;
    setFetching(d.doc_no);
    try {
      const r = await callOp(
        'get_employee_document',
        { emp_no: current.emp_no, doc_no: d.doc_no, include_content: true },
        undefined,
        fetchedEmployeeDocument,
      );
      if (!r.content_base64) {
        onToast('檔案儲存沒有回傳內容，請再試一次');
        return;
      }
      downloadFile(
        d.file_name,
        Uint8Array.from(atob(r.content_base64), (c) => c.charCodeAt(0)),
        d.content_type || 'application/octet-stream',
      );
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setFetching(null);
    }
  }

  async function copyPassword(password: string) {
    try {
      await navigator.clipboard.writeText(password);
      onToast('已複製密碼');
    } catch {
      onToast('這個瀏覽器不允許代為複製，請手動選取密碼');
    }
  }

  function openLogin(e: EmployeeRow) {
    setLoginFor(e);
    setLoginEmail('');
    setIssued(null);
  }

  async function issueLogin() {
    if (!loginFor) return;
    setIssuing(true);
    try {
      const d = await callOp(
        'create_employee_login',
        { emp_no: loginFor.emp_no, email: loginEmail.trim() },
        `為 ${loginFor.name} 核發登入帳號`,
        issuedEmployeeLogin,
      );
      setIssued({ username: d.username, password: d.temporary_password });
      onToast(`${d.employee_name} 的登入帳號已建立，角色為${d.role_name}`);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setIssuing(false);
    }
  }

  const deptValues = (data?.departments ?? []).filter(
    (v) => v.active || v.code === form.dept_code,
  );

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
              <div className="placeholder__actions">
                <button className="btn" onClick={openCreate}>
                  新增第一位員工
                </button>
              </div>
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
                          {e.employment_label} · {e.pay_basis_label}
                          {e.insured_as === 'owner' && ` · ${e.insured_as_label}`}
                        </span>
                      </td>
                      <td className="num cell-nowrap">
                        {e.hired_on}
                        {e.birthday && (
                          <span className="cell-hint cell-hint--block num">
                            生日 {e.birthday}
                          </span>
                        )}
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
                        <span className="cell-hint cell-hint--block num">
                          {e.annual_leave_from} 起
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
                      <td className="align-right cell-nowrap">
                        {canIssueLogin && !e.has_login && e.status !== 'left' && (
                          <button
                            className="btn btn--compact btn--secondary"
                            onClick={() => openLogin(e)}
                          >
                            核發帳號
                          </button>
                        )}
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

          <RulesetNote
            items={[
              {
                label: '特休',
                version: data.annual_leave_ruleset.version,
                from: data.annual_leave_ruleset.effective_from,
              },
            ]}
          />
        </>
      )}

      {loginFor && (
        <div className="overlay overlay--center">
          <div className="modal">
            <div className="drawer__head">
              <div>
                <div className="drawer__title">
                  {issued ? '初始密碼' : `為 ${loginFor.name} 核發登入帳號`}
                </div>
                <div className="drawer__subtitle">
                  {issued
                    ? `${loginFor.emp_no} ${loginFor.name} · ${issued.username}`
                    : '角色為一般職員，可以打卡、請假與請款。其他角色請洽系統管理員。'}
                </div>
              </div>
            </div>

            <div className="drawer__body">
              {issued ? (
                issued.password ? (
                  <>
                    <code className="secret-card__value secret-card__value--lead num">
                      {issued.password}
                    </code>
                    <p className="form-warn">
                      關掉這個視窗後無法再查看，請立即交付{loginFor.name}本人。
                    </p>
                  </>
                ) : (
                  <p className="section-note">
                    本帳號改用外部登入，沒有密碼。請{loginFor.name}以{' '}
                    <span className="num">{issued.username}</span> 登入。
                  </p>
                )
              ) : (
                <label className="field field--full">
                  電子郵件
                  <input
                    className="input"
                    type="text"
                    autoComplete="off"
                    value={loginEmail}
                    onChange={(e) => setLoginEmail(e.target.value)}
                  />
                  <span className="field__hint">這個地址就是登入名稱，建立後不可變更。</span>
                </label>
              )}
            </div>

            <div className="drawer__foot">
              {issued ? (
                <>
                  {issued.password && (
                    <button
                      className="btn btn--secondary"
                      onClick={() => void copyPassword(issued.password ?? '')}
                    >
                      複製密碼
                    </button>
                  )}
                  <span className="spacer" />
                  <button className="btn" onClick={() => setLoginFor(null)}>
                    我已交付
                  </button>
                </>
              ) : (
                <>
                  <button className="btn btn--secondary" onClick={() => setLoginFor(null)}>
                    取消
                  </button>
                  <span className="spacer" />
                  <button
                    className={`btn${issuing ? ' btn--off' : ''}`}
                    disabled={issuing || !loginEmail.trim()}
                    onClick={() => void issueLogin()}
                  >
                    {issuing ? '建立中…' : '建立帳號'}
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      )}

      {drawer && (
        <div className="overlay" onClick={() => setDrawer(null)}>
          <aside className="drawer" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">{drawer === 'edit' ? `編輯 ${form.emp_no}` : '新增員工'}</div>
                <div className="drawer__subtitle">
                  {canIssueLogin ? '登入帳號在名冊上核發' : '登入帳號由系統管理員建立'}
                </div>
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
                  計薪基準
                  <select
                    className="input"
                    value={form.pay_basis}
                    onChange={(e) => setField('pay_basis', e.target.value)}
                  >
                    {PAY_BASES.map((t) => (
                      <option key={t.value} value={t.value}>
                        {t.label}
                      </option>
                    ))}
                  </select>
                </label>

                <label className="field">
                  投保身分
                  <select
                    className="input"
                    value={form.insured_as}
                    onChange={(e) => setField('insured_as', e.target.value)}
                  >
                    {INSURED_AS.map((t) => (
                      <option key={t.value} value={t.value}>
                        {t.label}
                      </option>
                    ))}
                  </select>
                </label>

                {form.insured_as === 'employee' && (
                  <label className="field">
                    就業保險
                    <select
                      className="input"
                      value={form.employment_insured ? 'yes' : 'no'}
                      onChange={(e) => setField('employment_insured', e.target.value === 'yes')}
                    >
                      <option value="yes">參加</option>
                      <option value="no">不參加</option>
                    </select>
                  </label>
                )}

                <label className="field">
                  生日
                  <DateField
                    className="input"
                    value={form.birthday}
                    onChange={(v) => setField('birthday', v)}
                  />
                  <span className="field__hint">勞保申報與扣繳憑單要用；還沒收到可以留空。</span>
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

              <p className="form-warn">留空表示尚未談定，薪資作業會擋下這個人。</p>

              <div className="field-grid">
                <label className="field">
                  {form.pay_basis === 'hourly' ? '時薪' : '本薪'}
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.base_salary}
                    placeholder={
                      drawer === 'edit' ? '尚未設定' : form.pay_basis === 'hourly' ? '200' : '38000'
                    }
                    onChange={(e) => setBaseSalary(e.target.value)}
                  />
                  {data && insuredFrom(form.base_salary, form.pay_basis) && (
                    <span className="field__hint">{data.insured_suggestion.note}</span>
                  )}
                </label>
                <label className="field">
                  勞保投保薪資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.labor_insured_salary}
                    placeholder={drawer === 'edit' ? '尚未設定' : '38200'}
                    onChange={(e) => setField('labor_insured_salary', e.target.value)}
                  />
                  {data && laborOverCeiling(form.labor_insured_salary) && (
                    <span className="field__hint num">
                      勞保與就保按 {money(data.insured_suggestion.labor_ceiling)} 計費，職災按{' '}
                      {money(accidentChargedOn(form.labor_insured_salary))} 計費
                    </span>
                  )}
                </label>
                <label className="field">
                  健保投保薪資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.health_insured_salary}
                    placeholder={drawer === 'edit' ? '尚未設定' : '38200'}
                    onChange={(e) => setField('health_insured_salary', e.target.value)}
                  />
                </label>
                <label className="field">
                  勞退提繳工資
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={form.pension_wage}
                    placeholder={drawer === 'edit' ? '尚未設定' : '40100'}
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

              <div className="toolbar">
                <h3 className="section-title">健保眷屬</h3>
                <span className="spacer" />
                <button className="btn btn--compact" onClick={addDependent}>
                  ＋ 新增眷屬
                </button>
              </div>

              {form.dependents.length === 0 ? (
                <p className="section-note">尚未登記眷屬。</p>
              ) : (
                form.dependents.map((d, i) =>
                  d.removing ? (
                    <div className="card dependent-card" key={i}>
                      <div className="dependent-head dependent-head--removing">
                        <p className="form-warn">
                          存檔後刪除 {d.name}，勞健保頁不再計入，已算好的薪資單不變。
                        </p>
                        <button
                          className="btn btn--compact btn--secondary"
                          onClick={() => setDependent(i, { removing: false })}
                        >
                          復原
                        </button>
                      </div>
                    </div>
                  ) : (
                    <div className="card dependent-card" key={i}>
                      <div className="dependent-head">
                        <label className="field field--grow">
                          姓名
                          <input
                            className="input"
                            value={d.name}
                            onChange={(e) => setDependent(i, { name: e.target.value })}
                          />
                        </label>
                        {d.saved_from ? (
                          <button
                            className="btn btn--compact btn--secondary"
                            onClick={() => setDependent(i, { removing: true })}
                          >
                            登記錯誤，刪除
                          </button>
                        ) : (
                          <button
                            className="btn btn--icon"
                            onClick={() => removeDependent(i)}
                            aria-label="刪除本位眷屬"
                          >
                            ×
                          </button>
                        )}
                      </div>
  
                      <div className="dependent-grid">
                        <label className="field">
                          關係
                          <select
                            className="input"
                            value={d.relation}
                            onChange={(e) => setDependent(i, { relation: e.target.value })}
                          >
                            {(data?.relations ?? []).map((r) => (
                              <option key={r.code} value={r.code}>
                                {r.label}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label className="field">
                          身分證字號
                          <input
                            className="input num"
                            value={d.id_no}
                            onChange={(e) => setDependent(i, { id_no: e.target.value })}
                          />
                        </label>
                        <label className="field">
                          生日
                          <DateField
                            className="input"
                            value={d.birthday}
                            onChange={(v) => setDependent(i, { birthday: v })}
                          />
                        </label>
                      </div>
  
                      <div className="dependent-grid">
                        <label className="field">
                          健保加保日
                          <DateField
                            className="input"
                            value={d.health_enrolled_from}
                            onChange={(v) => setDependent(i, { health_enrolled_from: v })}
                          />
                        </label>
                        <label className="field">
                          健保退保日
                          <DateField
                            className="input"
                            value={d.health_enrolled_until}
                            onChange={(v) => setDependent(i, { health_enrolled_until: v })}
                          />
                          <span className="field__hint">留空表示仍在保</span>
                        </label>
                      </div>
                    </div>
                  ),
                )
              )}

              {drawer === 'edit' && canReadDocuments && (
                <>
                  <div className="toolbar">
                    <h3 className="section-title">附件</h3>
                    <span className="spacer" />
                    {canUploadDocument && files && (
                      <>
                        <select
                          className="input"
                          value={kind}
                          onChange={(e) => setKind(e.target.value)}
                        >
                          {files.kinds.map((k) => (
                            <option key={k.kind} value={k.kind}>
                              {k.label}
                            </option>
                          ))}
                        </select>
                        <input
                          ref={picker}
                          type="file"
                          className="visually-hidden"
                          onChange={(e) => {
                            const f = e.target.files?.[0];
                            if (f) void upload(f);
                          }}
                        />
                        <button
                          className="btn btn--compact"
                          disabled={uploading !== null}
                          onClick={() => picker.current?.click()}
                        >
                          {uploading ? '上傳中…' : '＋ 上傳附件'}
                        </button>
                      </>
                    )}
                  </div>

                  {filesError && <p className="state-note state-note--error">{filesError}</p>}
                  {!files && !filesError && <p className="section-note">讀取中…</p>}
                  {files && files.documents.length === 0 && (
                    <p className="section-note">尚無附件。</p>
                  )}
                  {files &&
                    files.documents.map((d) => (
                      <div className="entry-row" key={d.doc_no}>
                        <button
                          className="link-cell"
                          disabled={fetching === d.doc_no}
                          onClick={() => void download(d)}
                        >
                          {d.file_name}
                        </button>
                        <span className="cell-hint cell-hint--block">
                          <span className="num">{d.doc_no}</span> · {d.kind_label} ·{' '}
                          <span className="num">{d.size_label}</span> · {d.uploaded_by} ·{' '}
                          <span className="num">{businessTime(d.uploaded_at)}</span>
                          {d.versions > 1 && ` · 共 ${d.versions} 版`}
                          {fetching === d.doc_no && ' · 取回中…'}
                        </span>
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
