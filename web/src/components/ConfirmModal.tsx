import type { ReactNode } from 'react';

interface Props {
  title: string;
  subtitle?: string;
  body: ReactNode;
  confirmLabel: string;
  busyLabel?: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmModal({
  title,
  subtitle,
  body,
  confirmLabel,
  busyLabel,
  busy = false,
  onConfirm,
  onCancel,
}: Props) {
  return (
    <div className="overlay overlay--center" onClick={onCancel}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="drawer__head">
          <div>
            <div className="drawer__title">{title}</div>
            {subtitle && <div className="drawer__subtitle">{subtitle}</div>}
          </div>
          <button className="btn btn--close" onClick={onCancel} aria-label="關閉">
            ×
          </button>
        </div>
        <div className="drawer__body">{body}</div>
        <div className="drawer__foot">
          <button className="btn btn--ghost" onClick={onCancel}>
            ← 取消
          </button>
          <span className="spacer" />
          <button className={`btn${busy ? ' btn--off' : ''}`} disabled={busy} onClick={onConfirm}>
            {busy ? (busyLabel ?? '處理中…') : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
