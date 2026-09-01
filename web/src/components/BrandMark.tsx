interface Props {
  size?: 'sm' | 'md' | 'lg';
}

export function BrandMark({ size = 'md' }: Props) {
  return (
    <span className={`brand-mark brand-mark--${size}`}>
      <img className="brand-mark__icon" src="/favicon.svg" alt="" />
      Laysi One
    </span>
  );
}
