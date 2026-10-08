import { businessDate, type Cancellation } from '../api/client';

export function CancelledHint({ cancelled }: { cancelled: Cancellation }) {
  return (
    <>
      <span className="cell-hint cell-hint--block">
        {cancelled.by}
        {cancelled.role && `（${cancelled.role}）`} · {businessDate(cancelled.at)}
      </span>
      {cancelled.reason && (
        <span className="cell-hint cell-hint--block">
          <span className="cell-reason">{cancelled.reason}</span>
        </span>
      )}
    </>
  );
}
