import { useEffect, useState } from 'react';
import { BrandMark } from '../components/BrandMark';
import { CompanyBrand } from '../components/CompanyBrand';
import { useAuth } from '../auth';
import {
  decideAuthorize,
  getAuthorizeView,
  OpFailed,
  type AuthorizeView,
} from '../api/client';

function origin(uri: string): string {
  try {
    return new URL(uri).host;
  } catch {
    return uri;
  }
}

export function Consent() {
  const { user, brand } = useAuth();
  const search = window.location.search;
  const [view, setView] = useState<AuthorizeView | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    getAuthorizeView(search)
      .then((data) => live && setView(data))
      .catch((err) => live && setProblem(err instanceof OpFailed ? err.message : String(err)));
    return () => {
      live = false;
    };
  }, [search]);

  async function decide(approve: boolean) {
    setProblem(null);
    setBusy(true);
    try {
      window.location.replace(await decideAuthorize(search, approve));
    } catch (err) {
      setProblem(err instanceof OpFailed ? err.message : String(err));
      setBusy(false);
    }
  }

  return (
    <div className="login">
      <div className="card login__panel">
        <div className="login__brand">
          <CompanyBrand brand={brand} place="panel" />
        </div>

        {problem ? (
          <>
            <h1 className="login__title">無法完成授權</h1>
            <p className="state-note state-note--error login__alert">{problem}</p>
          </>
        ) : view === null ? (
          <p className="state-note">確認授權內容…</p>
        ) : (
          <>
            <h1 className="login__title">授權外部連接器</h1>
            <dl className="consent__list">
              <div className="consent__row">
                <dt>連接器</dt>
                <dd>{view.client_name || '未具名的連接器'}</dd>
              </div>
              <div className="consent__row">
                <dt>回傳位址</dt>
                <dd className="num">{origin(view.redirect_uri)}</dd>
              </div>
              <div className="consent__row">
                <dt>代表身分</dt>
                <dd>
                  {view.actor}
                  {user?.role_name ? `（${user.role_name}）` : ''}
                </dd>
              </div>
            </dl>
            <p className="login__note">
              同意後，這個連接器可以執行的範圍與本帳號相同。授權隨時可在「安全與營運」撤銷。
            </p>

            <div className="consent__actions">
              <button className="btn" type="button" disabled={busy} onClick={() => void decide(true)}>
                {busy ? '處理中…' : '同意授權'}
              </button>
              <button
                className="btn btn--secondary"
                type="button"
                disabled={busy}
                onClick={() => void decide(false)}
              >
                拒絕
              </button>
            </div>
          </>
        )}

      </div>

      <div className="login__mark">
        <BrandMark size="sm" />
      </div>
    </div>
  );
}
