import { businessDate, type PunchCorrection } from '../api/client';

function label(c: PunchCorrection): string {
  if (c.status === 'submitted') return '待審核';
  if (c.status === 'rejected') return '已駁回，未寫入';
  if (c.hr_entered) return '人資補登';
  return c.decided_by === '' ? '自動通過' : '申請核准';
}

export function PunchTrail({ corrections }: { corrections: PunchCorrection[] }) {
  if (corrections.length === 0) return null;
  return (
    <div className="punch-trail">
      {[...corrections]
        .sort((a, b) => a.id - b.id)
        .map((c) => (
          <div className="punch-trail__item" key={c.id}>
            <p className="punch-trail__head num">
              {c.kind_label}　
              {c.status === 'approved' ? `${c.replaced_at ?? '—'} → ${c.at}` : c.at}
            </p>
            <p className="punch-trail__meta">
              {label(c)} · {c.decided_by || c.created_by} ·{' '}
              {businessDate(c.decided_at ?? c.created_at)}
              {c.reason ? ` · ${c.reason}` : ''}
            </p>
          </div>
        ))}
    </div>
  );
}
