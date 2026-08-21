import { useCallback, useEffect, useRef, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  type ImportBatchRow,
  type ImportList,
  type ImportPreview,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

const BATCH_LABEL: Record<ImportBatchRow['status'], string> = {
  preview: '預覽，尚未寫入',
  committed: '已匯入',
  rolled_back: '已回滾',
  discarded: '已放棄',
};

export function Imports({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<ImportList | null>(null);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const picker = useRef<HTMLInputElement>(null);

  const load = useCallback(() => {
    setError(null);
    callOp<ImportList>('list_imports')
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function dryRun(file: File) {
    setBusy(true);
    try {
      const content = await file.text();
      const p = await callOp<ImportPreview>('preview_import', {
        source_code: 'voucher',
        file_name: file.name,
        content,
      });
      setPreview(p);
      onToast(
        p.can_commit
          ? `${p.batch.batch_no} 預覽完成：${p.drafts.length} 張傳票可匯入，尚未寫入`
          : `${p.batch.batch_no} 預覽完成，發現 ${p.batch.error_count} 個錯誤，未寫入任何資料`,
      );
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
      if (picker.current) picker.current.value = '';
    }
  }

  async function commit(batchNo: string) {
    setBusy(true);
    try {
      const r = await callOp<{ batch: ImportBatchRow }>('commit_import', { batch_no: batchNo });
      onToast(
        `${batchNo} 已寫入 ${r.batch.voucher_nos.length} 張傳票，狀態均為待過帳；過帳前不列入報表`,
      );
      setPreview(null);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function rollback(batchNo: string) {
    setBusy(true);
    try {
      const r = await callOp<{ removed: string[] }>('rollback_import', { batch_no: batchNo });
      onToast(`${batchNo} 已回滾，刪除 ${r.removed.length} 張待過帳傳票`);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <h1 className="page-title">匯入與整合</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <div className="split-columns split-columns--wide-left">
          <section>
            <h3 className="section-title">匯入來源</h3>
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>來源</th>
                    <th>格式</th>
                    <th>頻率</th>
                    <th className="align-right">狀態</th>
                  </tr>
                </thead>
                <tbody>
                  {data.sources.map((s) => (
                    <tr key={s.code} className={s.supported ? undefined : 'row--waiting'}>
                      <td>
                        {s.label}
                        <span className="cell-hint cell-hint--block">{s.note}</span>
                      </td>
                      <td className="cell-accounts">{s.format}</td>
                      <td className="cell-accounts">{s.cadence}</td>
                      <td className="align-right">
                        {s.supported ? (
                          <span className="tag tag--warn tag--static">可匯入</span>
                        ) : (
                          <span className="tag tag--pale tag--static">尚未開放</span>
                        )}
                        {s.last_batch_no && (
                          <span className="cell-hint cell-hint--block num">{s.last_batch_no}</span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            <div className="toolbar doc-card__actions">
              <input
                ref={picker}
                type="file"
                accept=".csv,text/csv,text/plain"
                className="visually-hidden"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) void dryRun(f);
                }}
              />
              <button
                className="btn btn--compact"
                disabled={busy}
                onClick={() => picker.current?.click()}
              >
                {busy ? '解析中…' : '選擇歷史傳票 CSV 進行預覽'}
              </button>
              <span className="spacer" />
              <span className="section-note">欄位：{data.csv_columns.join('、')}</span>
            </div>
            <p className="hint">{data.format_note}</p>

            {preview && (
              <>
                <h3 className="section-title section-title--spaced">
                  預覽結果　<span className="num">{preview.batch.batch_no}</span>
                </h3>
                <div className="card pay-bar">
                  <div>
                    <div className="pay-bar__kicker">
                      {preview.batch.row_count} 列 → {preview.batch.ok_count} 張傳票
                    </div>
                    <div className="pay-bar__total num">{money(preview.total)}</div>
                  </div>
                  <span className="spacer" />
                  <div className="pay-bar__note">
                    錯誤 {preview.batch.error_count}　警告 {preview.batch.warn_count}
                    <br />
                    {preview.can_commit
                      ? '確認匯入將寫入本次預覽的結果；檔案若已修改，請重新預覽。'
                      : preview.blocked_note}
                  </div>
                  <button
                    className={`btn${preview.can_commit ? '' : ' btn--off'}`}
                    disabled={busy || !preview.can_commit}
                    onClick={() => void commit(preview.batch.batch_no)}
                  >
                    確認匯入
                  </button>
                </div>

                {preview.batch.issues.length > 0 && (
                  <div className="table-scroll">
                    <table className="table">
                      <thead>
                        <tr>
                          <th>列</th>
                          <th>問題</th>
                          <th>更正方式</th>
                        </tr>
                      </thead>
                      <tbody>
                        {preview.batch.issues.map((i, n) => (
                          <tr key={n} className={i.severity === 'error' ? 'row--pending' : undefined}>
                            <td className="num">第 {i.row_no} 列</td>
                            <td>
                              <span
                                className={`tag tag--static${i.severity === 'error' ? ' tag--bad' : ' tag--pale'}`}
                              >
                                {i.severity === 'error' ? '錯誤' : '提醒'}
                              </span>{' '}
                              {i.issue}
                            </td>
                            <td className="cell-accounts">{i.fix}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </>
            )}

            <h3 className="section-title section-title--spaced">匯入紀錄</h3>
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>批次</th>
                    <th>檔案</th>
                    <th>結果</th>
                    <th className="align-right">動作</th>
                  </tr>
                </thead>
                <tbody>
                  {data.batches.map((b) => (
                    <tr key={b.batch_no}>
                      <td className="num cell-nowrap">
                        {b.batch_no}
                        <span className="cell-hint cell-hint--block">{b.source_label}</span>
                      </td>
                      <td>
                        {b.file_name}
                        <span className="cell-hint cell-hint--block">
                          {b.row_count} 列 · {b.created_by}
                        </span>
                      </td>
                      <td>
                        <span
                          className={`tag tag--static${b.status === 'committed' ? ' tag--warn' : ' tag--pale'}`}
                        >
                          {BATCH_LABEL[b.status]}
                        </span>
                        {b.voucher_nos.length > 0 && (
                          <span className="cell-hint cell-hint--block num">
                            {b.voucher_nos.length} 張傳票
                          </span>
                        )}
                        {b.error_count > 0 && (
                          <span className="cell-hint cell-hint--block">
                            {b.error_count} 個錯誤
                          </span>
                        )}
                      </td>
                      <td className="align-right">
                        {b.status === 'committed' ? (
                          <button
                            className="btn btn--compact btn--secondary"
                            disabled={busy}
                            onClick={() => void rollback(b.batch_no)}
                          >
                            回滾
                          </button>
                        ) : (
                          <span className="cell-muted">—</span>
                        )}
                      </td>
                    </tr>
                  ))}
                  {data.batches.length === 0 && (
                    <tr>
                      <td colSpan={4} className="cell-muted">
                        尚無匯入紀錄。
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
            <p className="hint">{data.commit_note}</p>
          </section>

          <section>
            <h3 className="section-title">對外整合</h3>
            <p className="section-note">
              {data.live_integrations} / {data.integrations.length} 條實際運作中。
            </p>
            <div className="card fact-card">
              {data.integrations.map((i) => (
                <div className="fact fact--stacked" key={i.code}>
                  <span className="fact__key">
                    {i.label}
                    <span className={`tag${i.live ? ' tag--warn tag--static' : ' tag--pale'}`}>
                      {i.live ? '運作中' : '未啟用'}
                    </span>
                  </span>
                  <span className="fact__value">
                    {i.detail}
                    {i.note && <span className="fact__note">{i.note}</span>}
                  </span>
                </div>
              ))}
            </div>
            <p className="hint">
              標為「未啟用」的整合尚未接上。
            </p>
          </section>
        </div>
      )}
    </div>
  );
}
