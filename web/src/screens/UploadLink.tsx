import { useEffect, useRef, useState } from 'react';
import { BrandMark } from '../components/BrandMark';
import {
  businessTime,
  fileSizeLabel,
  getUploadLinkStatus,
  OpFailed,
  sendToUploadLink,
  UPLOAD_ENDPOINT,
  type UploadLinkStatus,
} from '../api/client';

type RowState = 'waiting' | 'sending' | 'done' | 'failed';

interface Row {
  key: string;
  name: string;
  size: number;
  state: RowState;
  progress: number;
  message: string;
}

const CLOSED_CODES = ['link_expired', 'link_full', 'link_not_found'];

function stateLabel(row: Row): string {
  switch (row.state) {
    case 'done':
      return '已收到';
    case 'failed':
      return '未收下';
    case 'sending':
      return `${Math.round(row.progress * 100)}%`;
    default:
      return '等待中';
  }
}

export function UploadLink({ secret }: { secret: string }) {
  const [status, setStatus] = useState<UploadLinkStatus | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [rows, setRows] = useState<Row[]>([]);
  const [busy, setBusy] = useState(false);
  const [over, setOver] = useState(false);
  const picker = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let live = true;
    getUploadLinkStatus(secret)
      .then((s) => {
        if (!live) return;
        setStatus(s);
        setRows(
          s.files.map((f, i) => ({
            key: `received-${i}`,
            name: f.file_name,
            size: f.byte_size,
            state: 'done',
            progress: 1,
            message: '',
          })),
        );
      })
      .catch((e) => live && setProblem(e instanceof OpFailed ? e.error.message : String(e)));
    return () => {
      live = false;
    };
  }, [secret]);

  function patch(key: string, change: Partial<Row>) {
    setRows((all) => all.map((r) => (r.key === key ? { ...r, ...change } : r)));
  }

  async function send(files: File[]) {
    if (!status || busy || files.length === 0) return;
    const stamp = Date.now();
    const jobs = files.map((file, i) => ({
      file,
      row: {
        key: `${stamp}-${i}`,
        name: file.name,
        size: file.size,
        state: 'waiting',
        progress: 0,
        message: '',
      } satisfies Row,
    }));
    setRows((all) => [...all, ...jobs.map((j) => j.row)]);
    setBusy(true);
    let remaining = status.remaining;
    for (const { file, row } of jobs) {
      if (remaining <= 0) {
        patch(row.key, { state: 'failed', message: '本連結已收滿，其餘檔案請重新取得上傳連結' });
        continue;
      }
      if (file.size > status.max_file_bytes) {
        patch(row.key, {
          state: 'failed',
          message: `超過 ${fileSizeLabel(status.max_file_bytes)}，請壓縮或分成幾個檔案`,
        });
        continue;
      }
      patch(row.key, { state: 'sending' });
      try {
        const staged = await sendToUploadLink(`${UPLOAD_ENDPOINT}${secret}`, file, (p) =>
          patch(row.key, { progress: p }),
        );
        remaining = staged.remaining;
        patch(row.key, { state: 'done', progress: 1, message: staged.note ?? '' });
      } catch (e) {
        const failure = e instanceof OpFailed ? e.error : null;
        patch(row.key, { state: 'failed', message: failure?.message ?? String(e) });
        if (failure && CLOSED_CODES.includes(failure.code)) remaining = 0;
      }
    }
    setStatus((s) => (s ? { ...s, remaining } : s));
    setBusy(false);
    if (picker.current) picker.current.value = '';
  }

  const closed = status !== null && status.remaining <= 0;
  const received = rows.some((r) => r.state === 'done');

  return (
    <div className="login">
      <div className="card login__panel">
        <h1 className="login__title">上傳檔案</h1>

        {problem ? (
          <p className="state-note state-note--error login__alert">{problem}</p>
        ) : status === null ? (
          <p className="state-note">確認上傳連結…</p>
        ) : (
          <>
            <dl className="consent__list">
              <div className="consent__row">
                <dt>可再上傳</dt>
                <dd>
                  <span className="num">{status.remaining}</span> 個檔案
                </dd>
              </div>
              <div className="consent__row">
                <dt>單檔上限</dt>
                <dd className="num">{fileSizeLabel(status.max_file_bytes)}</dd>
              </div>
              <div className="consent__row">
                <dt>連結失效</dt>
                <dd className="num">{businessTime(status.expires_at)}</dd>
              </div>
            </dl>

            <button
              type="button"
              className={`upload-drop${over ? ' upload-drop--over' : ''}`}
              disabled={busy || closed}
              onClick={() => picker.current?.click()}
              onDragOver={(e) => {
                e.preventDefault();
                setOver(true);
              }}
              onDragLeave={() => setOver(false)}
              onDrop={(e) => {
                e.preventDefault();
                setOver(false);
                void send(Array.from(e.dataTransfer.files));
              }}
            >
              {closed ? '本連結已收滿' : busy ? '上傳中…' : '拖曳檔案到這裡，或點此選擇檔案'}
            </button>
            <input
              ref={picker}
              type="file"
              multiple
              className="visually-hidden"
              onChange={(e) => void send(Array.from(e.target.files ?? []))}
            />

            {rows.length > 0 && (
              <table className="table upload-list">
                <tbody>
                  {rows.map((r) => (
                    <tr key={r.key}>
                      <td>
                        {r.name}
                        <span className="cell-hint cell-hint--block">
                          {r.message || <span className="num">{fileSizeLabel(r.size)}</span>}
                        </span>
                      </td>
                      <td
                        className={`num align-right cell-nowrap${
                          r.state === 'failed' ? ' upload-list__failed' : ''
                        }`}
                      >
                        {stateLabel(r)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}

            {received && !busy && (
              <p className="login__note">檔案已收到，請回到原本的畫面或對話繼續。</p>
            )}
          </>
        )}
      </div>

      <div className="login__mark">
        <BrandMark size="sm" />
      </div>
    </div>
  );
}
