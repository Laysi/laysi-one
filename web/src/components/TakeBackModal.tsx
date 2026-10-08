import { useState } from 'react';
import { ConfirmModal } from './ConfirmModal';

interface Props {
  title: string;
  subtitle: string;
  warning: string;
  reasonLabel: string;
  reasonRequired: boolean;
  confirmLabel: string;
  busy: boolean;
  onConfirm: (reason: string) => void;
  onCancel: () => void;
}

export function TakeBackModal({
  title,
  subtitle,
  warning,
  reasonLabel,
  reasonRequired,
  confirmLabel,
  busy,
  onConfirm,
  onCancel,
}: Props) {
  const [reason, setReason] = useState('');
  return (
    <ConfirmModal
      title={title}
      subtitle={subtitle}
      body={
        <>
          <p className="form-warn">{warning}</p>
          <label className="field field--full">
            {reasonLabel}
            <textarea
              className="input"
              rows={2}
              value={reason}
              onChange={(e) => setReason(e.target.value)}
            />
          </label>
        </>
      }
      confirmLabel={confirmLabel}
      busy={busy}
      confirmDisabled={reasonRequired && reason.trim() === ''}
      onConfirm={() => onConfirm(reason.trim())}
      onCancel={onCancel}
    />
  );
}
