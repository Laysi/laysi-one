import { useCallback, useEffect, useRef, useState } from 'react';
import {
  businessTime,
  callOp,
  documentList,
  downloadDocumentFile,
  fileSizeLabel,
  OpFailed,
  uploadDocumentFile,
  type DocumentList,
  type DocumentRow,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

export function Documents({ onToast, reloadKey }: Props) {
  const [data, setData] = useState<DocumentList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [q, setQ] = useState('');
  const [uploading, setUploading] = useState<string | null>(null);
  const [progress, setProgress] = useState(0);
  const [fetching, setFetching] = useState<string | null>(null);
  const [fetchProgress, setFetchProgress] = useState(0);
  const [openHash, setOpenHash] = useState<string | null>(null);
  const picker = useRef<HTMLInputElement>(null);

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    callOp('list_documents', q.trim() ? { q: q.trim() } : {}, undefined, documentList)
      .then((d) => live && setData(d))
      .catch((e: Error) => live && setError(e.message));
    return () => running.current?.();
  }, [q]);

  useEffect(load, [load, reloadKey]);

  async function upload(file: File) {
    if (data && file.size > data.max_object_bytes) {
      onToast(`${file.name} 超過 ${fileSizeLabel(data.max_object_bytes)}，請壓縮或分成幾個檔案再上傳`);
      if (picker.current) picker.current.value = '';
      return;
    }
    setUploading(file.name);
    setProgress(0);
    try {
      const d = await uploadDocumentFile(file, { kind: 'receipt' }, `上傳 ${file.name}`, setProgress);
      onToast(
        `${d.document.doc_no} ${d.document.version_label} 已上傳，內容雜湊 ${d.document.checksum_short}`,
      );
      load();
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

  const mb = data ? Math.floor(data.max_object_bytes / (1024 * 1024)) : 0;

  return (
    <div>
      <h1 className="page-title">文件管理</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>
          <div className="stat-grid">
            <div className="card stat-card">
              <div className="stat-card__kicker">文件</div>
              <div className="stat-card__value num">{data.stats.documents}</div>
              <div className="stat-card__note num">{data.stats.versions} 個版本</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">佔用空間</div>
              <div className="stat-card__value num">{data.stats.size_label}</div>
              <div className="stat-card__note num">
                {data.bucket ? '已存入檔案儲存' : '未接上檔案儲存'}
              </div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">缺附件的傳票</div>
              <div className="stat-card__value num">{data.stats.vouchers_without_doc}</div>
              <div className="stat-card__note num">查帳時常見的查核項目</div>
            </div>
          </div>

          <p className={`recon${data.bucket_reconciled === false ? ' recon--off' : ''}`}>
            {data.storage_error ? (
              <>無法讀取檔案儲存：{data.storage_error}</>
            ) : data.bucket ? (
              <>
                系統登記 <span className="num">{data.stats.versions}</span> 個版本／
                <span className="num">{data.stats.size_label}</span>，
                檔案儲存實際有 <span className="num">{data.bucket.objects}</span> 個檔案／
                <span className="num">{data.bucket.size_label}</span>
                {data.bucket_reconciled
                  ? '，兩者分開計數，結果相符。'
                  : `　差 ${data.bucket_difference} 位元組，需查明原因。`}
              </>
            ) : (
              <>尚未接上檔案儲存。</>
            )}
          </p>

          <div className="toolbar">
            <input
              className="entry-input entry-input--search"
              placeholder="搜尋檔名、文件編號或關聯單據號"
              value={q}
              onChange={(e) => setQ(e.target.value)}
            />
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
            <span className="hint">
              單檔 <span className="num">{mb}</span> MB 以內 · 同名再傳存為新版
            </span>
            <button
              className="btn btn--compact"
              disabled={uploading !== null}
              onClick={() => picker.current?.click()}
            >
              {uploading ? `上傳中 ${Math.round(progress * 100)}%` : '上傳檔案'}
            </button>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>檔案</th>
                  <th>關聯</th>
                  <th>內容雜湊（BLAKE3）</th>
                  <th className="align-right">大小</th>
                </tr>
              </thead>
              <tbody>
                {uploading && (
                  <tr>
                    <td colSpan={4} className="cell-muted">
                      上傳中 <span className="num">{Math.round(progress * 100)}%</span> ·{' '}
                      {uploading}
                    </td>
                  </tr>
                )}
                {data.documents.map((d) => (
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
                        <span className="num">{d.doc_no}</span> · {d.kind_label} · {d.uploaded_by}
                        {' · '}
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
                    <td className="cell-accounts num cell-nowrap">{d.link_ref || '—'}</td>
                    <td className="cell-accounts num">
                      <button
                        className="link-cell num"
                        onClick={() =>
                          setOpenHash((h) => (h === d.doc_no ? null : d.doc_no))
                        }
                      >
                        {openHash === d.doc_no ? d.checksum : d.checksum_short}
                      </button>
                    </td>
                    <td className="num align-right">{d.size_label}</td>
                  </tr>
                ))}
                {data.documents.length === 0 && !uploading && (
                  <tr>
                    <td colSpan={4} className="cell-muted">
                      尚無文件。
                      <button className="link-cell" onClick={() => picker.current?.click()}>
                        上傳第一個檔案
                      </button>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <p className="hint">{data.immutability_note}</p>
        </>
      )}
    </div>
  );
}
