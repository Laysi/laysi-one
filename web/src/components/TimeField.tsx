const HOURS = Array.from({ length: 24 }, (_, i) => String(i).padStart(2, '0'));
const MINUTES = Array.from({ length: 60 }, (_, i) => String(i).padStart(2, '0'));

interface Props {
  value: string;
  onChange: (value: string) => void;
  label?: string;
  disabled?: boolean;
}

export function TimeField({ value, onChange, label, disabled = false }: Props) {
  const parts = /^(\d{2}):(\d{2})$/.exec(value);
  const hour = parts ? parts[1] : '';
  const minute = parts ? parts[2] : '';

  return (
    <span className="time-field">
      <select
        className="input input--clock num"
        aria-label={label ? `${label}（時）` : '時'}
        value={hour}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value ? `${e.target.value}:${minute || '00'}` : '')}
      >
        <option value="">—</option>
        {HOURS.map((h) => (
          <option key={h} value={h}>
            {h}
          </option>
        ))}
      </select>
      <span className="time-field__colon">：</span>
      <select
        className="input input--clock num"
        aria-label={label ? `${label}（分）` : '分'}
        value={minute}
        disabled={disabled || hour === ''}
        onChange={(e) => onChange(`${hour}:${e.target.value}`)}
      >
        {hour === '' && <option value="">—</option>}
        {MINUTES.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </select>
    </span>
  );
}
