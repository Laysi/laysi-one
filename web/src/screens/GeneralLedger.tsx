import { useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  reversedVoucher,
  STATUS_LABEL,
  voucherList,
  voucherResult,
  type Voucher,
} from '../api/client';
import { ConfirmModal } from '../components/ConfirmModal';
import { EntryDrawer } from '../components/EntryDrawer';
import { DateField } from '../components/DateField';

type Filter = 'all' | 'draft' | 'pending' | 'posted';

const FILTERS: { id: Filter; label: string }[] = [
  { id: 'all', label: '全部' },
  { id: 'draft', label: '待確認' },
  { id: 'pending', label: '待過帳' },
  { id: 'posted', label: '已過帳' },
];

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
  canDiscard: boolean;
  canEdit: boolean;
}

type CorrectionKind = 'void' | 'reverse';

interface Correction {
  voucher: Voucher;
  kind: CorrectionKind | null;
}

export function GeneralLedger({ onToast, reloadKey, onChanged, canDiscard, canEdit }: Props) {
  const [filter, setFilter] = useState<Filter>('all');
  const [vouchers, setVouchers] = useState<Voucher[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [posting, setPosting] = useState<Voucher | null>(null);
  const [postBusy, setPostBusy] = useState(false);
  const [editing, setEditing] = useState<Voucher | null>(null);
  const [discarding, setDiscarding] = useState<Voucher | null>(null);
  const [discardBusy, setDiscardBusy] = useState(false);
  const [correction, setCorrection] = useState<Correction | null>(null);
  const [voidReason, setVoidReason] = useState('');
  const [reverseDate, setReverseDate] = useState('');
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    let live = true;
    setError(null);
    callOp(
      'search_vouchers',
      {
        status: filter === 'all' ? null : filter,
        limit: 200,
      },
      undefined,
      voucherList,
    )
      .then((d) => live && setVouchers(d.vouchers))
      .catch((e: Error) => live && setError(e.message));
    return () => {
      live = false;
    };
  }, [filter, reloadKey]);

  async function post(no: string) {
    setPostBusy(true);
    try {
      await callOp('post_voucher', { no }, `將 ${no} 過帳`, voucherResult);
      setPosting(null);
      onToast(`${no} 已過帳`);
      onChanged();
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      onToast(msg);
    } finally {
      setPostBusy(false);
    }
  }

  async function discard(no: string) {
    setDiscardBusy(true);
    try {
      await callOp<unknown>('discard_voucher', { no }, `刪除未過帳傳票 ${no}`);
      setDiscarding(null);
      onToast(`${no} 已刪除`);
      onChanged();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setDiscardBusy(false);
    }
  }

  function openCorrection(v: Voucher) {
    setCorrection({ voucher: v, kind: null });
    setVoidReason('');
    setReverseDate('');
  }

  function closeCorrection() {
    setCorrection(null);
  }

  async function submitVoid() {
    if (!correction) return;
    if (!voidReason.trim()) {
      onToast('請填寫作廢理由');
      return;
    }
    setSubmitting(true);
    try {
      await callOp(
              'void_voucher',
              { no: correction.voucher.no, reason: voidReason.trim() },
              `${correction.voucher.no} 不應存在：${voidReason.trim()}`,
              voucherResult,
            );
      onToast(`${correction.voucher.no} 已作廢，理由：${voidReason.trim()}`);
      closeCorrection();
      onChanged();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSubmitting(false);
    }
  }

  async function submitReverse() {
    if (!correction) return;
    if (!reverseDate) {
      onToast('反向傳票日期為必填，通常為發現錯誤當日');
      return;
    }
    setSubmitting(true);
    try {
      const d = await callOp(
                        'reverse_voucher',
                        { no: correction.voucher.no, date: reverseDate },
                        `${correction.voucher.no} 金額或科目記載錯誤，開立反向傳票更正`,
                        reversedVoucher,
                      );
      onToast(
        `已產生並過帳沖銷傳票 ${d.voucher.no}，${money(d.voucher.amount)}，原傳票 ${correction.voucher.no} 維持不動`,
      );
      closeCorrection();
      onChanged();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSubmitting(false);
    }
  }

  const pending = vouchers?.filter((v) => v.status === 'pending' || v.status === 'draft') ?? [];

  return (
    <div>
      <h1 className="page-title">傳票 · 總帳</h1>
      <div className="toolbar">
        {FILTERS.map((f) => (
          <button
            key={f.id}
            className={`chip${filter === f.id ? ' chip--on' : ''}`}
            onClick={() => setFilter(f.id)}
          >
            {f.label}
          </button>
        ))}
        <span className="spacer" />
        {pending.length > 0 && (
          <span className="section-note">{pending.length} 張待過帳</span>
        )}
      </div>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!vouchers && !error && <p className="state-note">讀取中…</p>}

      {vouchers && (
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th>傳票號碼</th>
                <th>日期</th>
                <th>摘要</th>
                <th>科目</th>
                <th style={{ textAlign: 'right' }}>金額</th>
                <th>狀態</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {vouchers.map((v) => (
                <tr key={v.no}>
                  <td className="num">{v.no}</td>
                  <td className="num cell-muted">{v.date.slice(5)}</td>
                  <td>{v.memo}</td>
                  <td className="cell-accounts">{v.accounts_label}</td>
                  <td className="num" style={{ textAlign: 'right' }}>
                    {money(v.amount)}
                  </td>
                  <td>
                    <span
                      className={`tag${v.status === 'pending' ? ' tag--warn' : ''}${
                        v.status === 'draft' ? ' tag--bad' : ''
                      }`}
                    >
                      {STATUS_LABEL[v.status]}
                    </span>
                  </td>
                  <td style={{ textAlign: 'right' }}>
                    {v.status === 'pending' || v.status === 'draft' ? (
                      <div className="flow__actions">
                        <button className="btn btn--compact" onClick={() => setPosting(v)}>
                          過帳
                        </button>
                        {canEdit && (v.source === 'manual' || v.source === 'ai_draft') && (
                          <button
                            className="btn btn--compact btn--secondary"
                            onClick={() => setEditing(v)}
                          >
                            修改
                          </button>
                        )}
                        {canDiscard && (v.source === 'manual' || v.source === 'ai_draft') && (
                          <button
                            className="btn btn--compact btn--secondary"
                            onClick={() => setDiscarding(v)}
                          >
                            刪除
                          </button>
                        )}
                      </div>
                    ) : v.status === 'posted' ? (
                      <button className="btn btn--compact btn--secondary" onClick={() => openCorrection(v)}>
                        更正
                      </button>
                    ) : (
                      <span className="cell-muted">—</span>
                    )}
                  </td>
                </tr>
              ))}
              {vouchers.length === 0 && (
                <tr>
                  <td colSpan={7} className="cell-muted">
                    目前篩選條件下沒有傳票。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      <EntryDrawer
        open={editing !== null}
        edit={editing}
        onClose={() => setEditing(null)}
        onToast={onToast}
        onSaved={onChanged}
      />

      {discarding && (
        <ConfirmModal
          title={`刪除 ${discarding.no}`}
          subtitle={`${discarding.date} · ${discarding.memo}`}
          body={
            <p className="form-warn">
              刪除 <span className="num">{money(discarding.amount)}</span> 這張未過帳傳票，
              無法復原。總帳不受影響，這張傳票從未進入總帳。
            </p>
          }
          confirmLabel={`確認刪除 ${discarding.no}`}
          busyLabel="刪除中…"
          busy={discardBusy}
          onConfirm={() => void discard(discarding.no)}
          onCancel={() => setDiscarding(null)}
        />
      )}

      {posting && (
        <ConfirmModal
          title={`過帳 ${posting.no}`}
          subtitle={`${posting.date} · ${posting.memo}`}
          body={
            <p className="form-warn">
              過帳 <span className="num">{money(posting.amount)}</span> 後這張傳票不能修改，
              更正只能另開一張反向分錄。
            </p>
          }
          confirmLabel={`確認過帳 ${posting.no}`}
          busyLabel="過帳中…"
          busy={postBusy}
          onConfirm={() => void post(posting.no)}
          onCancel={() => setPosting(null)}
        />
      )}

      {correction && (
        <div className="overlay overlay--center" onClick={closeCorrection}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">更正 {correction.voucher.no}</div>
                <div className="drawer__subtitle">
                  {correction.voucher.memo} ·{' '}
                  <span className="num">{money(correction.voucher.amount)}</span> ·{' '}
                  <span className="num">{correction.voucher.date}</span>
                </div>
              </div>
              <button className="btn btn--close" onClick={closeCorrection} aria-label="關閉">
                ×
              </button>
            </div>

            <div className="drawer__body">
              {correction.kind === null && (
                <div className="choice-list">
                  <button className="choice" onClick={() => setCorrection({ ...correction, kind: 'void' })}>
                    <div className="choice__title">該傳票不應存在</div>
                    <div className="choice__desc">
                      例如重複過帳，或交易對象完全錯誤。無法復原。
                    </div>
                  </button>
                  <button className="choice" onClick={() => setCorrection({ ...correction, kind: 'reverse' })}>
                    <div className="choice__title">交易確實發生，僅金額或科目記錯</div>
                    <div className="choice__desc">
                      另開一張借貸相反的傳票並過帳，原傳票不動。
                    </div>
                  </button>
                </div>
              )}

              {correction.kind === 'void' && (
                <>
                  <p className="form-warn">
                    作廢後<strong>無法復原</strong>，{correction.voucher.no} 標記為「已作廢」。
                  </p>
                  <label className="field field--full">
                    作廢理由（必填，會寫進稽核摘要）
                    <textarea
                      className="input"
                      rows={3}
                      value={voidReason}
                      placeholder="例如：與 V2607-041 重複過帳"
                      onChange={(e) => setVoidReason(e.target.value)}
                    />
                  </label>
                </>
              )}

              {correction.kind === 'reverse' && (
                <>
                  <p className="form-warn">
                    另開一張與 {correction.voucher.no} 借貸相反的傳票並<strong>過帳</strong>，
                    {correction.voucher.no} 維持不動。<strong>無法復原</strong>。
                  </p>
                  <label className="field field--full">
                    反向傳票日期（通常填發現錯誤的當日）
                    <DateField
                      className="input"
                      value={reverseDate}
                      onChange={(v) => setReverseDate(v)}
                    />
                  </label>
                </>
              )}
            </div>

            <div className="drawer__foot">
              {correction.kind === null ? (
                <span className="drawer__foot-note">選擇後仍要再次確認才送出</span>
              ) : (
                <>
                  <button
                    className="btn btn--ghost"
                    onClick={() => setCorrection({ ...correction, kind: null })}
                  >
                    ← 重新選擇
                  </button>
                  <span className="spacer" />
                  <button
                    className={`btn${submitting ? ' btn--off' : ''}`}
                    disabled={submitting}
                    onClick={() => void (correction.kind === 'void' ? submitVoid() : submitReverse())}
                  >
                    {submitting
                      ? '處理中…'
                      : correction.kind === 'void'
                        ? '確認作廢，無法復原'
                        : '確認沖銷，開立並過帳反向傳票'}
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
