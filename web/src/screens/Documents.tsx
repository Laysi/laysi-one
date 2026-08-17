import { useCallback, useEffect, useRef, useState } from 'react';
import { callOp, OpFailed, type DocumentList, type DocumentRow } from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

function readAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onerror = () => reject(new Error('無法讀取此檔案'));
    r.onload = () => {
      const s = String(r.result);
      const comma = s.indexOf(',');
      resolve(comma < 0 ? s : s.slice(comma + 1));
    };
    r.readAsDataURL(file);
  });
}

export function Documents({ onToast, reloadKey }: Props) {
  const [data, setData] = useState<DocumentList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [q, setQ] = useState('');
  const [busy, setBusy] = useState(false);
  const [verified, setVerified] = useState<Record<string, string>>({});
  const picker = useRef<HTMLInputElement>(null);

  const load = useCallback(() => {
    setError(null);
    callOp<DocumentList>('list_documents', q.trim() ? { q: q.trim() } : {})
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, [q]);

  useEffect(load, [load, reloadKey]);

  async function upload(file: File) {
    setBusy(true);
    try {
      const content = await readAsBase64(file);
      const d = await callOp<{ document: DocumentRow; new_version: boolean }>(
        'upload_document',
        { file_name: file.name, content_base64: content, kind: 'receipt' },
        `上傳 ${file.name}`,
      );
      onToast(
        `${d.document.doc_no} ${d.document.version_label} 已上傳，檔案指紋 ${d.document.checksum_short}`,
      );
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
      if (picker.current) picker.current.value = '';
    }
  }

  async function verify(d: DocumentRow) {
    try {
      const r = await callOp<{ verified: boolean; checksum_recomputed: string; byte_size_actual: number }>(
        'get_document',
        { doc_no: d.doc_no },
      );
      setVerified((v) => ({
        ...v,
        [d.doc_no]: r.verified
          ? `已自物件儲存取回 ${r.byte_size_actual} 位元組，重算雜湊與登記值相符`
          : '雜湊不符，檔案內容已遭變更',
      }));
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  const mb = data ? Math.floor(data.max_object_bytes / (1024 * 1024)) : 0;

  return (
    <div>
      <h1 className="page-title">文件管理</h1>
      <p className="page-lede">上傳後不可覆寫，要修訂請另存新版本。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>
          <div className="stat-grid">
            <div className="card stat-card">
              <div className="stat-card__kicker">文件</div>
              <div className="stat-card__value num">{data.stats.documents}</div>
              <div className="stat-card__note">{data.stats.versions} 個版本</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">佔用空間</div>
              <div className="stat-card__value num">{data.stats.size_label}</div>
              <div className="stat-card__note">
                {data.bucket ? `儲存區 ${data.bucket.bucket}` : '未接上物件儲存'}
              </div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">尚未辨識</div>
              <div className="stat-card__value num">{data.stats.ocr_pending}</div>
              <div className="stat-card__note">系統沒有 OCR 引擎</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">缺附件的傳票</div>
              <div className="stat-card__value num">{data.stats.vouchers_without_doc}</div>
              <div className="stat-card__note">查帳時常見的查核項目</div>
            </div>
          </div>

          <p className={`recon${data.bucket_reconciled === false ? ' recon--off' : ''}`}>
            {data.storage_error ? (
              <>無法讀取物件儲存：{data.storage_error}</>
            ) : data.bucket ? (
              <>
                系統登記 <span className="num">{data.stats.versions}</span> 個版本／
                <span className="num">{data.stats.size_label}</span>，
                儲存區 <span className="num">{data.bucket.endpoint}</span> 實際有{' '}
                <span className="num">{data.bucket.objects}</span> 個檔案／
                <span className="num">{data.bucket.size_label}</span>
                {data.bucket_reconciled
                  ? '，兩者分開計數，結果相符。'
                  : `　差 ${data.bucket_difference} 位元組，需查明原因。`}
              </>
            ) : (
              <>尚未接上物件儲存。</>
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
            <button
              className="btn btn--compact"
              disabled={busy}
              onClick={() => picker.current?.click()}
            >
              {busy ? '上傳中…' : '上傳檔案'}
            </button>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>檔案</th>
                  <th>關聯</th>
                  <th>版次</th>
                  <th>內容雜湊</th>
                  <th className="align-right">大小</th>
                  <th className="align-right">驗證</th>
                </tr>
              </thead>
              <tbody>
                {data.documents.map((d) => (
                  <tr key={d.doc_no}>
                    <td>
                      {d.file_name}
                      <span className="cell-hint cell-hint--block">
                        <span className="num">{d.doc_no}</span> · {d.kind_label} · {d.uploaded_by}
                        {' · OCR '}
                        {d.ocr_label}
                      </span>
                    </td>
                    <td className="cell-accounts num cell-nowrap">{d.link_ref || '—'}</td>
                    <td className="cell-accounts num">
                      {d.version_label}
                      {d.versions > 1 && (
                        <span className="cell-hint">共 {d.versions} 版</span>
                      )}
                    </td>
                    <td className="cell-accounts num cell-nowrap" title={d.checksum}>
                      {d.checksum_short}
                    </td>
                    <td className="num align-right">{d.size_label}</td>
                    <td className="align-right">
                      <button className="btn btn--compact btn--secondary" onClick={() => void verify(d)}>
                        取回驗證
                      </button>
                      {verified[d.doc_no] && (
                        <span className="cell-hint cell-hint--block">{verified[d.doc_no]}</span>
                      )}
                    </td>
                  </tr>
                ))}
                {data.documents.length === 0 && (
                  <tr>
                    <td colSpan={6} className="cell-muted">
                      尚無文件。請使用「上傳檔案」建立第一筆。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <p className="hint">{data.ocr_note}</p>
          <p className="hint">{data.immutability_note}</p>
          <p className="hint">
            「取回驗證」會讀回檔案、重算指紋並與登記值比對。不符時回覆錯誤且
            <strong>不交出內容</strong>；遭變更的憑證外觀與正本無異，風險高於沒有憑證。
          </p>
          <p className="hint">
            AI 也可以上傳檔案，權限與畫面上的人相同。單檔上限{' '}
            <span className="num">{mb}</span> MB。
          </p>
        </>
      )}
    </div>
  );
}
