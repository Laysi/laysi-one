import type { Brand } from '../api/client';

interface Props {
  brand: Brand;
  place: 'rail' | 'panel';
}

export function CompanyBrand({ brand, place }: Props) {
  const showsLogo = brand.display !== '名稱' && brand.logo !== '';
  const showsName = brand.display !== '標誌' && brand.name !== '';
  if (!showsLogo && !showsName) return null;
  return (
    <div className={`company-brand company-brand--${place}`}>
      {showsLogo && (
        <img className="company-brand__logo" src={brand.logo} alt={brand.name || '公司標誌'} />
      )}
      {showsName && <span className="company-brand__name">{brand.name}</span>}
    </div>
  );
}
