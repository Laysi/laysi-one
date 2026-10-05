interface Props {
  periods: string[];
  value: string | null | undefined;
  onChange: (period: string) => void;
}

const YEAR_CHIP_LIMIT = 5;
const RECENT_YEARS = 3;

export function PeriodPicker({ periods, value, onChange }: Props) {
  const sorted = [...new Set(value ? [...periods, value] : periods)].sort();
  const latest = sorted.at(-1);
  if (!latest) return null;

  const year = (value ?? latest).slice(0, 4);
  const years = [...new Set(sorted.map((p) => p.slice(0, 4)))];
  const older = years.length > YEAR_CHIP_LIMIT ? years.slice(0, -RECENT_YEARS) : [];
  const recent = years.slice(older.length);
  const months = sorted.filter((p) => p.startsWith(`${year}-`));

  const pickYear = (y: string) => {
    const p = sorted.filter((q) => q.startsWith(`${y}-`)).at(-1);
    if (p && y !== year) onChange(p);
  };

  return (
    <div className="period-picker">
      {years.length > 1 && (
        <div className="period-picker__years">
          {older.length > 0 && (
            <select
              className={`chip${older.includes(year) ? ' chip--on' : ''}`}
              aria-label="更早的年份"
              value={older.includes(year) ? year : ''}
              onChange={(e) => pickYear(e.target.value)}
            >
              <option value="" disabled>
                更早
              </option>
              {[...older].reverse().map((y) => (
                <option key={y} value={y}>
                  {y} 年
                </option>
              ))}
            </select>
          )}
          {recent.map((y) => (
            <button
              key={y}
              type="button"
              className={`chip${y === year ? ' chip--on' : ''}`}
              onClick={() => pickYear(y)}
            >
              {y} 年
            </button>
          ))}
        </div>
      )}
      <div className="period-picker__months">
        {months.map((p) => (
          <button
            key={p}
            type="button"
            className={`chip${p === value ? ' chip--on' : ''}`}
            onClick={() => onChange(p)}
          >
            {Number(p.slice(5, 7))} 月
          </button>
        ))}
      </div>
    </div>
  );
}
