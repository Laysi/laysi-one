const MIN = '2000-01-01';
const MAX = '2099-12-31';

interface Props {
  value: string;
  onChange: (value: string) => void;
  className?: string;
  min?: string;
  max?: string;
}

function usable(value: string, min: string, max: string): boolean {
  return value === '' || (/^\d{4}-\d{2}-\d{2}$/.test(value) && value >= min && value <= max);
}

export function DateField({ value, onChange, className = 'input', min = MIN, max = MAX }: Props) {
  return (
    <input
      className={className}
      type="date"
      min={min}
      max={max}
      value={value}
      onChange={(e) => {
        if (usable(e.target.value, min, max)) {
          onChange(e.target.value);
          return;
        }
        e.target.value = value;
      }}
    />
  );
}
