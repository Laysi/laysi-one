import { useEffect, useState } from 'react';
import {
  callOp,
  getMeta,
  OpFailed,
  updateResult,
  updateTicket,
  type LicenseState,
} from '../api/client';
import {
  advance,
  browserStore,
  clearRun,
  DOWNTIME_PHRASE,
  loadRun,
  POLL_MS,
  saveRun,
  startRun,
  updateAction,
  within,
  type Phase,
  type UpdateRun,
} from '../update';
import { ConfirmModal } from './ConfirmModal';

const PROBE_TIMEOUT_MS = 5000;
const TICKET_TIMEOUT_MS = 15000;

interface Props {
  licence: LicenseState;
  canUpdate: boolean;
  onFinished: () => void;
}

function failureText(e: unknown): string {
  return e instanceof OpFailed ? e.error.message : '更新要求沒有送出，請稍後再試。';
}

export function UpdateCard({ licence, canUpdate, onFinished }: Props) {
  const [run, setRun] = useState<UpdateRun | null>(() => loadRun(browserStore(), Date.now()));
  const [phase, setPhase] = useState<Phase | null>(() =>
    loadRun(browserStore(), Date.now()) ? { kind: 'progress' } : null,
  );
  const [confirming, setConfirming] = useState(false);
  const [starting, setStarting] = useState(false);

  const requestId = run?.requestId ?? null;

  useEffect(() => {
    if (!run) return;
    let live = true;
    let current = run;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const tick = async () => {
      const [meta, result] = await Promise.allSettled([
        within(getMeta(), PROBE_TIMEOUT_MS),
        within(
          callOp(
            'get_update_result',
            { request_id: current.requestId },
            undefined,
            updateResult,
          ),
          PROBE_TIMEOUT_MS,
        ),
      ]);
      if (!live) return;
      const step = advance(
        current,
        {
          version: meta.status === 'fulfilled' ? meta.value.version : null,
          result: result.status === 'fulfilled' ? result.value : null,
          reachable: result.status === 'fulfilled',
        },
        Date.now(),
      );
      current = step.run;
      setPhase(step.phase);
      if (step.phase.kind === 'progress') {
        saveRun(browserStore(), current);
        timer = setTimeout(() => void tick(), POLL_MS);
        return;
      }
      clearRun(browserStore());
      setRun(null);
      onFinished();
    };

    timer = setTimeout(() => void tick(), POLL_MS);
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, [requestId]);

  async function start() {
    setConfirming(false);
    setStarting(true);
    try {
      const ticket = await within(
        callOp('get_update_ticket', {}, undefined, updateTicket),
        TICKET_TIMEOUT_MS,
      );
      const started = startRun(
        ticket.request_id,
        licence.running_version,
        ticket.offered_version,
        Date.now(),
      );
      saveRun(browserStore(), started);
      setPhase({ kind: 'progress' });
      setRun(started);
    } catch (e) {
      setPhase({ kind: 'not-run', why: 'error', message: failureText(e) });
    } finally {
      setStarting(false);
    }
  }

  const released = licence.released_version;
  const action = updateAction(released, licence.updater_present, canUpdate, phase);

  return (
    <section className="licence-card">
      <h3 className="section-title">系統更新</h3>
      <div className="card fact-card">
        <div className="fact">
          <span className="fact__key">目前版本</span>
          <span className="fact__value num">{licence.running_version}</span>
        </div>
        {released && (
          <>
            <div className="fact">
              <span className="fact__key">可安裝版本</span>
              <span className="fact__value num">{released}</span>
            </div>
            {licence.updater_present && (
              <>
                <div className="fact">
                  <span className="fact__key">更新期間</span>
                  <span className="fact__value">系統暫時無法使用，{DOWNTIME_PHRASE}</span>
                </div>
                <div className="fact">
                  <span className="fact__key">更新失敗時</span>
                  <span className="fact__value">自動回到目前版本，系統可以繼續使用</span>
                </div>
              </>
            )}
          </>
        )}
      </div>

      {action === 'no-release' && <p className="state-note">目前沒有可安裝的版本。</p>}

      {phase && (
        <p
          role="status"
          className={
            phase.kind === 'progress' || phase.kind === 'succeeded'
              ? 'state-note'
              : 'state-note state-note--error'
          }
        >
          {phase.kind === 'progress' && '更新進行中，請稍候。'}
          {phase.kind === 'succeeded' && (
            <>
              更新完成，目前版本 <span className="num">{phase.version}</span>。
            </>
          )}
          {phase.kind === 'failed' && phase.restored && (
            <>
              更新沒有完成，已回到 <span className="num">{phase.version}</span>
              ，系統可以繼續使用。再次失敗時請洽經銷商。
            </>
          )}
          {phase.kind === 'failed' && !phase.restored && (
            <>更新沒有完成，也沒有回到原來的版本。請盡快洽經銷商。</>
          )}
          {(phase.kind === 'not-run' ||
            phase.kind === 'unconfirmed' ||
            phase.kind === 'unreachable') &&
            phase.message}
        </p>
      )}

      {(phase?.kind === 'succeeded' || phase?.kind === 'unconfirmed') && (
        <div className="row-actions">
          <button className="btn" onClick={() => window.location.reload()}>
            重新載入
          </button>
        </div>
      )}

      {(action === 'unmanaged' || action === 'button' || action === 'admin-only') && (
        <div className="row-actions">
          {action === 'unmanaged' ? (
            <span className="cell-muted">本安裝的更新由維運人員處理，請洽經銷商。</span>
          ) : action === 'button' ? (
            <button className="btn" disabled={starting} onClick={() => setConfirming(true)}>
              {starting ? '送出中…' : `更新至 ${released}`}
            </button>
          ) : (
            <span className="cell-muted">更新由系統管理員執行。</span>
          )}
        </div>
      )}

      {confirming && released && (
        <ConfirmModal
          title="確認更新"
          body={
            <p className="form-note">
              系統將更新至 <span className="num">{released}</span>。
            </p>
          }
          confirmLabel="更新"
          onConfirm={() => void start()}
          onCancel={() => setConfirming(false)}
        />
      )}
    </section>
  );
}
