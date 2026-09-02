import { useEffect, useMemo, useState } from 'react';
import {
  accountList,
  addDecimals,
  businessToday,
  callOp,
  dimensionList,
  isZero,
  money,
  OpFailed,
  trimDecimal,
  voucherResult,
  type Account,
  type Voucher,
} from '../api/client';
import { DateField } from './DateField';

interface Row {
  account_code: string;
  debit: string;
  credit: string;
  dept_code: string;
}

const EMPTY: Row[] = [
  { account_code: '6115', debit: '', credit: '', dept_code: '' },
  { account_code: '1102', debit: '', credit: '', dept_code: '' },
];

function clean(v: string): string {
  const t = v.replace(/,/g, '').trim();
  return /^-?\d*(\.\d*)?$/.test(t) && t !== '' && t !== '-' && t !== '.' ? t : '0';
}

function sum(rows: Row[], key: 'debit' | 'credit'): string {
  return rows.reduce((acc, r) => addDecimals(acc, clean(r[key])) ?? acc, '0');
}

function negate(v: string): string {
  return v.startsWith('-') ? v.slice(1) : `-${v}`;
}

function fmt(v: string): string {
  const abs = v.startsWith('-') ? v.slice(1) : v;
  const [whole = '0', frac] = abs.split('.');
  const grouped = money(whole);
  const rest = frac ? trimDecimal(`0.${frac}`).slice(1) : '';
  return `${grouped}${rest}`;
}

interface Props {
  open: boolean;
  onClose: () => void;
  onToast: (msg: string) => void;
  onSaved: () => void;
  edit?: Voucher | null;
}

function amountOf(v: string): string {
  return isZero(v) ? '' : trimDecimal(v);
}

export function EntryDrawer({ open, onClose, onToast, onSaved, edit }: Props) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [depts, setDepts] = useState<{ code: string; label: string }[]>([]);
  const [date, setDate] = useState(businessToday);
  const [memo, setMemo] = useState('');
  const [rows, setRows] = useState<Row[]>(EMPTY);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!open) return;
    if (edit) {
      setDate(edit.date);
      setMemo(edit.memo);
      setRows(
        edit.lines.map((l) => ({
          account_code: l.account_code,
          debit: amountOf(l.debit),
          credit: amountOf(l.credit),
          dept_code: l.dept_code ?? '',
        })),
      );
    } else {
      setDate(businessToday());
      setMemo('');
      setRows(EMPTY);
    }
  }, [open, edit]);

  useEffect(() => {
    if (!open || accounts.length) return;
    callOp('list_accounts', {}, undefined, accountList)
      .then((d) => setAccounts(d.accounts.filter((a) => a.is_postable)))
      .catch(() => onToast('無法讀取會計科目表'));
    callOp('list_dimensions', {}, undefined, dimensionList)
      .then((d) =>
        setDepts(
          d.dimensions
            .filter((x) => x.code === 'department')
            .flatMap((x) => x.values)
            .filter((v) => v.active)
            .map((v) => ({ code: v.code, label: v.label })),
        ),
      )
      .catch(() => setDepts([]));
  }, [open, accounts.length, onToast]);

  const debit = sum(rows, 'debit');
  const credit = sum(rows, 'credit');
  const diff = addDecimals(debit, negate(credit)) ?? '0';
  const balanced = !isZero(debit) && isZero(diff);

  const message = useMemo(() => {
    if (balanced) return '借貸相等，可儲存';
    if (isZero(debit) && isZero(credit)) return '尚未輸入金額';
    return `借貸差 ${fmt(diff)} 元，無法儲存`;
  }, [balanced, debit, credit, diff]);

  function setRow(i: number, key: keyof Row, value: string) {
    setRows(rows.map((r, j) => (j === i ? { ...r, [key]: value } : r)));
  }

  function removeRow(i: number) {
    if (rows.length <= 2) {
      onToast('複式記帳至少需兩行分錄');
      return;
    }
    setRows(rows.filter((_, j) => j !== i));
  }

  async function save() {
    setSaving(true);
    try {
      const d = await callOp(
        edit ? 'update_voucher' : 'create_voucher',
        {
          ...(edit ? { no: edit.no } : {}),
          date,
          memo: memo || '未填寫摘要',
          lines: rows.map((r) => ({
            account_code: r.account_code,
            debit: clean(r.debit),
            credit: clean(r.credit),
            ...(r.dept_code ? { dept_code: r.dept_code } : {}),
          })),
        },
        undefined,
        voucherResult,
      );
      if (!edit) {
        onToast(`${d.voucher.no} 已儲存，狀態為待過帳`);
        setRows(EMPTY);
        setMemo('');
      } else if (d.voucher.no === edit.no) {
        onToast(`${d.voucher.no} 已修改`);
      } else {
        onToast(`${edit.no} 已修改，日期換月，改號為 ${d.voucher.no}`);
      }
      onSaved();
      onClose();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  if (!open) return null;

  return (
    <div className="overlay" onClick={onClose}>
      <aside className="drawer" onClick={(e) => e.stopPropagation()}>
        <div className="drawer__head">
          <div>
            <div className="drawer__title">{edit ? `修改 ${edit.no}` : '建立傳票'}</div>
            <div className="drawer__subtitle">
              {edit
                ? '整份分錄改為本頁的內容。日期改到別的月份時，傳票號碼會換成該月份的號碼'
                : '儲存後狀態為待過帳，過帳後才列入報表'}
            </div>
          </div>
          <button className="btn btn--close" onClick={onClose} aria-label="關閉">
            ×
          </button>
        </div>

        <div className="drawer__body">
          <div className="field-row">
            <label className="field field--date">
              <span>日期</span>
              <DateField
                className="input input--date"
                value={date}
                onChange={(v) => setDate(v)}
              />
            </label>
            <label className="field field--grow">
              <span>摘要</span>
              <input
                className="input"
                value={memo}
                placeholder="本筆交易摘要，供日後查閱"
                onChange={(e) => setMemo(e.target.value)}
              />
            </label>
          </div>

          <div className="entry-grid entry-head">
            <div>科目</div>
            <div>部門</div>
            <div style={{ textAlign: 'right' }}>借方</div>
            <div style={{ textAlign: 'right' }}>貸方</div>
            <div />
          </div>

          {rows.map((r, i) => (
            <div className="entry-grid entry-row" key={i}>
              <select
                className="input"
                value={r.account_code}
                onChange={(e) => setRow(i, 'account_code', e.target.value)}
              >
                {accounts.map((a) => (
                  <option key={a.code} value={a.code}>
                    {a.code} {a.name}
                  </option>
                ))}
              </select>
              <select
                className="input"
                value={r.dept_code}
                onChange={(e) => setRow(i, 'dept_code', e.target.value)}
              >
                <option value="">未指定</option>
                {depts.map((d) => (
                  <option key={d.code} value={d.code}>
                    {d.label}
                  </option>
                ))}
              </select>
              <input
                className="input entry-input entry-input--amount num"
                inputMode="decimal"
                value={r.debit}
                onChange={(e) => setRow(i, 'debit', e.target.value)}
              />
              <input
                className="input entry-input entry-input--amount num"
                inputMode="decimal"
                value={r.credit}
                onChange={(e) => setRow(i, 'credit', e.target.value)}
              />
              <button className="btn btn--icon" onClick={() => removeRow(i)} aria-label="刪除本行">
                ×
              </button>
            </div>
          ))}

          <div className="entry-grid entry-total">
            <button
              className="btn btn--dashed"
              onClick={() =>
                setRows([...rows, { account_code: '1102', debit: '', credit: '', dept_code: '' }])
              }
            >
              ＋ 新增分錄
            </button>
            <div />
            <div className="entry-total__label num">{fmt(debit)}</div>
            <div className="entry-total__label num">{fmt(credit)}</div>
            <div />
          </div>

          <div className={`balance${balanced ? ' balance--ok' : ''}`}>{message}</div>
        </div>

        <div className="drawer__foot">
          <span className="drawer__foot-note">
            {edit ? '過帳後不能再修改' : '存檔後可在「傳票 · 總帳」過帳'}
          </span>
          <button
            className={`btn${balanced && !saving ? '' : ' btn--off'}`}
            disabled={!balanced || saving}
            onClick={() => void save()}
          >
            {saving ? '存檔中…' : edit ? '儲存修改' : '儲存為待過帳'}
          </button>
        </div>
      </aside>
    </div>
  );
}
