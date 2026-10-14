import { trimDecimal, type PunchDay } from '../api/client';

export function DayMarks({ day }: { day: PunchDay }) {
  return (
    <>
      {day.absent && <span className="tag tag--bad tag--static">曠職</span>}
      {day.leaves.map((l) => (
        <span key={l.leave_no} className="cell-nowrap">
          <span className={`tag tag--static${l.status === 'pending' ? ' tag--warn' : ' tag--pale'}`}>
            {l.kind_label}
            {l.status === 'pending' && `（${l.status_label}）`}
          </span>
          <span className="cell-hint num">
            {l.leave_no} · {trimDecimal(l.hours)} 小時
          </span>
        </span>
      ))}
    </>
  );
}
