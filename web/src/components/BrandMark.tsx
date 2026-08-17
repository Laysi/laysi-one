import { useState } from 'react';

interface Props {
  height?: number;
  variant?: 'wordmark' | 'monogram';
}

export function BrandMark({ height = 28, variant = 'wordmark' }: Props) {
  const [failed, setFailed] = useState(false);
  const path = variant === 'monogram' ? 'monogram.svg' : 'mark.svg';

  if (failed) {
    return (
      <div className="brand-mark__missing" style={{ height }} role="img" aria-label="Laysi One">
        字標載入失敗
      </div>
    );
  }

  return (
    <img
      className="brand-mark"
      src={`/v1/brand/${path}?h=${height}`}
      height={height}
      alt="Laysi One"
      onError={() => setFailed(true)}
    />
  );
}
