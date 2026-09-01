import { useState } from 'react';

interface Props {
  value: string;
  label?: string;
}

export function CopyButton({ value, label = '複製' }: Props) {
  const [state, setState] = useState<'idle' | 'done' | 'failed'>('idle');

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setState('done');
    } catch {
      setState('failed');
    }
  }

  return (
    <button className="btn btn--compact btn--secondary" onClick={() => void copy()}>
      {state === 'done' ? '已複製' : state === 'failed' ? '請手動選取' : label}
    </button>
  );
}
