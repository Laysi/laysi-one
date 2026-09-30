import { useCallback, useEffect, useRef, useState } from 'react';
import {
  businessTime,
  callOp,
  documentList,
  downloadDocumentFile,
  fileSizeLabel,
  money,
  OpFailed,
  uploadDocumentFile,
  type DocumentRow,
  type Voucher,
} from '../api/client';

interface Props {
  voucher: Voucher | null;
  onClose: () => void;
  onToast: (msg: string) => void;
  onChanged: () => void;
}

export function VoucherDocs({ voucher, onClose, onToast, onChanged }: Props) {
  const no = voucher?.no ?? '';
  const claimNo = voucher?.claim_no ?? '';
  const [docs, setDocs] = useState<DocumentRow[] | null>(null);
  const [maxBytes, setMaxBytes] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [uploading, setUploading] = useState<string | null>(null);
  const [progress, setProgress] = useState(0);
  const [fetching, setFetching] = useState<string | null>(null);
  const [fetchProgress, setFetchProgress] = useState(0);
  const picker = useRef<HTMLInputElement>(null);

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    if (!no) return;
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    const fetchFor = (ref: string) =>
      callOp('list_documents', { q: ref, limit: 200 }, undefined, documentList);
    Promise.all(claimNo ? [fetchFor(no), fetchFor(claimNo)] : [fetchFor(no)])
      .then(([own, claim]) => {
        if (!live) return;
        const rows = own.documents.filter((x) => x.link_type === 'voucher' && x.link_ref === no);
        if (claim) {
          rows.push(
            ...claim.documents.filter(
              (x) => x.link_type === 'expense_claim' && x.link_ref === claimNo,
            ),
          );
        }
        setDocs(rows);
        setMaxBytes(own.max_object_bytes);
      })
      .catch((e: Error) => live && setError(e.message));
    return () => running.current?.();
  }, [no, claimNo]);

  useEffect(load, [load]);

  async function upload(file: File) {
    if (maxBytes > 0 && file.size > maxBytes) {
      onToast(`${file.name} 超過 ${fileSizeLabel(maxBytes)}，請壓縮或分成幾個檔案再上傳`);
      if (picker.current) picker.current.value = '';
      return;
    }
    setUploading(file.name);
    setProgress(0);
    try {
      const d = await uploadDocumentFile(
        file,
        { kind: 'receipt', link_type: 'voucher', link_ref: no },
        `把 ${file.name} 附到傳票 ${no}`,
        setProgress,
      );
      onToast(`${d.document.doc_no} ${d.document.version_label} 已附到 ${no}`);
      load();
      onChanged();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setUploading(null);
      if (picker.current) picker.current.value = '';
    }
  }

  async function download(d: DocumentRow) {
    setFetching(d.doc_no);
    setFetchProgress(0);
    try {
      await downloadDocumentFile(d, setFetchProgress);
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setFetching(null);
    }
  }

  if (!voucher) return null;

  const mb = Math.floor(maxBytes / (1024 * 1024));

  return (
    <div className="overlay" onClick={onClose}>
      <aside className="drawer" onClick={(e) => e.stopPropagation()}>
        <div className="drawer__head">
          <div>
            <div className="drawer__title">{no} 的憑證</div>
            <div className="drawer__subtitle">
              {voucher.memo} · <span className="num">{money(voucher.amount)}</span> ·{' '}
              <span className="num">{voucher.date}</span>
            </div>
          </div>
          <button className="btn btn--close" onClick={onClose} aria-label="關閉">
            ×
          </button>
        </div>

        <div className="drawer__body">
          {error && <p className="state-note state-note--error">{error}</p>}
          {!docs && !error && <p className="state-note">讀取中…</p>}

          {docs && docs.length === 0 && !uploading && (
            <p className="state-note">尚未附上發票或收據影本。</p>
          )}

          {docs && docs.length > 0 && (
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>檔案</th>
                    <th className="align-right">大小</th>
                  </tr>
                </thead>
                <tbody>
                  {docs.map((d) => (
                    <tr key={d.doc_no}>
                      <td>
                        <button
                          className="link-cell"
                          disabled={fetching === d.doc_no}
                          onClick={() => void download(d)}
                        >
                          {d.file_name}
                        </button>
                        <span className="cell-hint cell-hint--block">
                          <span className="num">{d.doc_no}</span> · {d.kind_label} ·{' '}
                          {d.link_type === 'expense_claim' && (
                            <>
                              報支 <span className="num">{d.link_ref}</span> ·{' '}
                            </>
                          )}
                          {d.uploaded_by} ·{' '}
                          <span className="num">{businessTime(d.uploaded_at)}</span>
                          {d.versions > 1 && ` · 共 ${d.versions} 版`}
                          {fetching === d.doc_no && (
                          <>
                            {' · 下載中 '}
                            <span className="num">{Math.round(fetchProgress * 100)}%</span>
                          </>
                        )}
                        </span>
                      </td>
                      <td className="num align-right">{d.size_label}</td>
                    </tr>
                  ))}
                  {uploading && (
                    <tr>
                      <td colSpan={2} className="cell-muted">
                        上傳中 <span className="num">{Math.round(progress * 100)}%</span> ·{' '}
                        {uploading}
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
          )}
        </div>

        <div className="drawer__foot">
          <span className="drawer__foot-note">
            單檔 <span className="num">{mb}</span> MB 以內 · 同名再傳存為新版
          </span>
          <span className="spacer" />
          <input
            ref={picker}
            type="file"
            className="visually-hidden"
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) void upload(f);
            }}
          />
          <button
            className="btn btn--compact"
            disabled={uploading !== null}
            onClick={() => picker.current?.click()}
          >
            {uploading ? `上傳中 ${Math.round(progress * 100)}%` : '附上檔案'}
          </button>
        </div>
      </aside>
    </div>
  );
}
