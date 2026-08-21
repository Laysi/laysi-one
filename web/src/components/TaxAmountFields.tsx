import { useState } from 'react';

export interface TaxTriple {
  net: string;
  tax: string;
  total: string;
}

export const EMPTY_TAX_TRIPLE: TaxTriple = { net: '', tax: '', total: '' };

function round(value: number): string {
  return String(Math.round(value));
}

function derive(field: keyof TaxTriple, raw: string, rate: number): TaxTriple {
  const n = Number(raw);
  if (raw.trim() === '' || Number.isNaN(n)) {
    return { ...EMPTY_TAX_TRIPLE, [field]: raw };
  }
  if (field === 'net') {
    const tax = Math.round(n * rate);
    return { net: raw, tax: String(tax), total: round(n + tax) };
  }
  if (field === 'total') {
    const tax = Math.round(n - n / (1 + rate));
    return { net: round(n - tax), tax: String(tax), total: raw };
  }
  return { net: '', tax: raw, total: '' };
}

interface Props {
  label: string;
  taxLabel: string;
  value: TaxTriple;
  rate: string | null;
  ratePercent: string | null;
  missing?: boolean;
  onChange: (next: TaxTriple) => void;
}

export function TaxAmountFields({
  label,
  taxLabel,
  value,
  rate,
  ratePercent,
  missing = false,
  onChange,
}: Props) {
  const [edited, setEdited] = useState<keyof TaxTriple>('total');
  const numericRate = rate === null ? null : Number(rate);

  function change(field: keyof TaxTriple, raw: string) {
    setEdited(field);
    if (numericRate === null || Number.isNaN(numericRate)) {
      onChange({ ...value, [field]: raw });
      return;
    }
    if (field === 'tax') {
      const tax = Number(raw);
      const base = Number(edited === 'net' ? value.net : value.total);
      if (raw.trim() !== '' && !Number.isNaN(tax) && !Number.isNaN(base)) {
        onChange(
          edited === 'net'
            ? { net: value.net, tax: raw, total: round(base + tax) }
            : { net: round(base - tax), tax: raw, total: value.total },
        );
        return;
      }
      onChange({ ...value, tax: raw });
      return;
    }
    onChange(derive(field, raw, numericRate));
  }

  return (
    <>
      <label className="field">
        {label}（含稅）
        <input
          className="input num"
          inputMode="decimal"
          value={value.total}
          onChange={(e) => change('total', e.target.value)}
        />
        {missing && <span className="field__hint field__hint--missing">還沒填</span>}
      </label>
      <label className="field">
        未稅金額
        <input
          className="input num"
          inputMode="decimal"
          value={value.net}
          onChange={(e) => change('net', e.target.value)}
        />
      </label>
      <label className="field">
        {taxLabel}
        <input
          className="input num"
          inputMode="decimal"
          value={value.tax}
          onChange={(e) => change('tax', e.target.value)}
        />
        <span className="field__hint">
          {ratePercent ? `零稅率或免稅填 0，其餘依 ${ratePercent}% 自動帶入` : '零稅率或免稅填 0'}
        </span>
      </label>
    </>
  );
}
