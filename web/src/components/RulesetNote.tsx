interface Item {
  label: string;
  version?: string | null;
  from?: string | null;
  source?: string | null;
}

export function RulesetNote({ items }: { items: Item[] }) {
  const shown = items.filter((r) => r.version || r.from || r.source);
  if (shown.length === 0) return null;
  return (
    <p className="ruleset-note">
      <span className="ruleset-note__kicker">依據</span>
      {shown.map((r, i) => (
        <span className="ruleset-note__item" key={`${r.label}${i}`}>
          <span className="ruleset-note__label">{r.label}</span>
          {r.version && <span className="num">{r.version}</span>}
          {r.from && <span className="num">{r.from} 起</span>}
          {r.source && <span>{r.source}</span>}
        </span>
      ))}
    </p>
  );
}
