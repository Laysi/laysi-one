import { useCallback, useEffect, useState } from 'react';
import { useCurrentUser } from '../auth';
import {
  callOp,
  money,
  OpFailed,
  type ApprovalRules,
  type ApprovalStep,
  type GrantCell,
  type RoleMatrix,
} from '../api/client';
import { ConfirmModal } from '../components/ConfirmModal';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

const CYCLE = ['full', 'limited', 'none'] as const;
type Level = (typeof CYCLE)[number];

const MARK: Record<Level, string> = { full: '●', limited: '○', none: '—' };
const LEVEL_NOTE: Record<Level, string> = {
  full: '完整',
  limited: '有限',
  none: '無',
};

interface StepForm {
  step_no: string;
  role_code: string;
  label: string;
  condition_note: string;
  min_amount: string;
}

function emptyStepForm(nextStepNo: number): StepForm {
  return { step_no: String(nextStepNo), role_code: '', label: '', condition_note: '', min_amount: '' };
}

interface PendingGrant {
  roleCode: string;
  roleName: string;
  permCode: string;
  permLabel: string;
  from: Level;
  to: Level;
}

export function Roles({ onToast, reloadKey }: Props) {
  const me = useCurrentUser();
  const [pending, setPending] = useState<PendingGrant | null>(null);
  const [matrix, setMatrix] = useState<RoleMatrix | null>(null);
  const [flow, setFlow] = useState<ApprovalRules | null>(null);
  const [flowError, setFlowError] = useState<string | null>(null);
  const canReadFlow = me.permissions.includes('list_approval_rules');
  const canEditFlow = me.permissions.includes('set_approval_rule');
  const canEditMatrix = me.permissions.includes('set_role_permission');
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<'matrix' | 'flow'>('matrix');
  const [busy, setBusy] = useState<string | null>(null);

  const [stepForm, setStepForm] = useState<StepForm>(emptyStepForm(1));
  const [savingStep, setSavingStep] = useState(false);

  const load = useCallback(() => {
    setError(null);
    callOp<RoleMatrix>('list_roles')
      .then(setMatrix)
      .catch((e: Error) => setError(e.message));
    if (!canReadFlow) {
      setFlow(null);
      return;
    }
    callOp<ApprovalRules>('list_approval_rules')
      .then(setFlow)
      .catch((e: Error) => setFlowError(e.message));
  }, [canReadFlow]);

  useEffect(load, [load, reloadKey]);

  function openNewStep() {
    const nextNo = (flow?.steps.reduce((max, s) => Math.max(max, s.step_no), 0) ?? 0) + 1;
    setStepForm(emptyStepForm(nextNo));
  }

  function openEditStep(s: ApprovalStep) {
    setStepForm({
      step_no: String(s.step_no),
      role_code: s.role_code ?? '',
      label: s.label,
      condition_note: s.condition_note,
      min_amount: s.min_amount ?? '',
    });
  }

  async function saveStep() {
    const stepNo = parseInt(stepForm.step_no, 10);
    if (!stepNo || stepNo <= 0) {
      onToast('關卡編號要大於零');
      return;
    }
    if (!stepForm.label.trim()) {
      onToast('關卡名稱為必填');
      return;
    }
    setSavingStep(true);
    try {
      await callOp<unknown>(
        'set_approval_rule',
        {
          step_no: stepNo,
          role_code: stepForm.role_code || null,
          label: stepForm.label.trim(),
          condition_note: stepForm.condition_note.trim(),
          min_amount: stepForm.min_amount.trim() || null,
        },
        `設定簽核第 ${stepNo} 關`,
      );
      onToast(`第 ${stepNo} 關已儲存`);
      openNewStep();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingStep(false);
    }
  }

  async function deleteStep(s: ApprovalStep) {
    setSavingStep(true);
    try {
      await callOp<unknown>(
        'set_approval_rule',
        { step_no: s.step_no, delete: true },
        `刪除簽核第 ${s.step_no} 關`,
      );
      onToast(`第 ${s.step_no} 關（${s.label}）已刪除`);
      openNewStep();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingStep(false);
    }
  }

  function levelOf(grants: GrantCell[], role: string, perm: string): Level {
    return grants.find((g) => g.role_code === role && g.permission_code === perm)?.level ?? 'none';
  }

  async function applyGrant(g: PendingGrant) {
    const key = `${g.roleCode}/${g.permCode}`;
    setBusy(key);
    try {
      await callOp<unknown>('set_role_permission', {
        role_code: g.roleCode,
        permission_code: g.permCode,
        level: g.to,
      });
      setPending(null);
      onToast(`${g.roleName}的「${g.permLabel}」已設為「${LEVEL_NOTE[g.to]}」，下一次呼叫即適用`);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div>
      <h1 className="page-title">角色與核准</h1>
      <p className="page-lede">權限矩陣由系統管理員維護，簽核流程由負責人維護。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!matrix && !error && <p className="state-note">讀取中…</p>}

      {pending && (
        <ConfirmModal
          title={`${pending.roleName}　${pending.permLabel}`}
          subtitle={`${LEVEL_NOTE[pending.from]} → ${LEVEL_NOTE[pending.to]}`}
          body={
            <>
              <p className="form-warn">下一次呼叫起適用，已登入的人不必重新登入。</p>
              {pending.roleCode === me.role &&
                pending.permCode === 'manage_access' &&
                pending.to !== 'full' && (
                  <p className="form-warn">
                    這是你自己的角色。改完之後你就進不了這一頁，也不能改回來，
                    要另找一位系統管理員或用 <span className="num">laysi-admin</span> 還原。
                  </p>
                )}
            </>
          }
          confirmLabel={`確認設為「${LEVEL_NOTE[pending.to]}」`}
          busyLabel="設定中…"
          busy={busy === `${pending.roleCode}/${pending.permCode}`}
          onConfirm={() => void applyGrant(pending)}
          onCancel={() => setPending(null)}
        />
      )}

      {matrix && (
        <>
          <div className="toolbar toolbar--spaced">
            <button
              className={`chip${tab === 'matrix' ? ' chip--on' : ''}`}
              onClick={() => setTab('matrix')}
            >
              權限矩陣
            </button>
            <button
              className={`chip${tab === 'flow' ? ' chip--on' : ''}`}
              onClick={() => setTab('flow')}
            >
              簽核流程
            </button>
          </div>

          {tab === 'matrix' && (
          <>
          <p className="hint">● 完整　○ 有限　— 無。點選欄位可切換，並記錄異動人員與時間。</p>

          <div className="table-scroll table-scroll--sticky">
            <table className="table table--matrix">
              <thead>
                <tr>
                  <th>權限</th>
                  {matrix.roles.map((r) => (
                    <th key={r.code} className="align-center">
                      {r.name}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {matrix.permissions.map((p) => (
                  <tr key={p.code}>
                    <td>
                      {p.label}
                      {p.missing_ops.length > 0 && p.op_names.length === 0 && (
                        <span className="tag tag--pale tag--static">無對應操作</span>
                      )}
                      <span className="cell-hint cell-hint--block">{p.note}</span>
                      {p.boss_only_ops.length > 0 && (
                        <span className="cell-hint cell-hint--block">
                          其中 {p.boss_only_ops.length} 項限負責人，本列給滿仍無法通過。
                        </span>
                      )}
                      {p.admin_only_ops.length > 0 && (
                        <span className="cell-hint cell-hint--block">
                          其中 {p.admin_only_ops.length} 項限系統管理員，本列給滿仍無法通過。
                        </span>
                      )}
                    </td>
                    {matrix.roles.map((r) => {
                      const level = levelOf(matrix.grants, r.code, p.code);
                      const key = `${r.code}/${p.code}`;
                      return (
                        <td key={r.code} className="align-center">
                          <button
                            className={`perm perm--${level}`}
                            title={
                              canEditMatrix
                                ? `點選以切換：${LEVEL_NOTE[level]} → ${
                                    LEVEL_NOTE[
                                      CYCLE[(CYCLE.indexOf(level) + 1) % CYCLE.length] as Level
                                    ]
                                  }`
                                : LEVEL_NOTE[level]
                            }
                            disabled={!canEditMatrix || busy === key}
                            onClick={() =>
                              setPending({
                                roleCode: r.code,
                                roleName: r.name,
                                permCode: p.code,
                                permLabel: p.label,
                                from: level,
                                to: CYCLE[(CYCLE.indexOf(level) + 1) % CYCLE.length] as Level,
                              })
                            }
                          >
                            {MARK[level]}
                          </button>
                        </td>
                      );
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <p className="hint">
            關閉後立即生效，
            <strong>包含關閉管理權限這一項本身</strong>。
          </p>
          <p className="hint">
            本矩陣只管人員；
            <span className="num">AI</span> 能呼叫哪些操作在「MCP 與 AI」設定。
          </p>
          <p className="hint">帳號與角色在「帳號管理」建立。</p>
          </>
          )}

          {tab === 'flow' && (
          <>
          {!canReadFlow && (
            <p className="state-note">費用報支的簽核關卡由負責人維護。</p>
          )}
          {flowError && <p className="state-note state-note--error">{flowError}</p>}

          {flow && (
            <>
            <div className="flow">
              {flow.steps.map((s, i) => (
                <div className="flow__step-wrap" key={s.step_no}>
                  <div className="card flow__step">
                    <div className="flow__role">
                      第 {s.step_no} 關 · {s.label}
                    </div>
                    <div className="flow__cond">
                      {s.role_name ?? '申請人'}
                      {s.min_amount && (
                        <>
                          {' '}· <span className="num">{money(s.min_amount)}</span>{' '}
                          {s.condition_note}
                        </>
                      )}
                      {!s.min_amount && s.condition_note && <> · {s.condition_note}</>}
                    </div>
                    {canEditFlow && (
                      <div className="flow__actions">
                        <button
                          className="btn btn--compact btn--secondary"
                          onClick={() => openEditStep(s)}
                        >
                          編輯
                        </button>
                        <button
                          className="btn btn--compact btn--secondary"
                          disabled={savingStep}
                          onClick={() => void deleteStep(s)}
                        >
                          刪除
                        </button>
                      </div>
                    )}
                  </div>
                  {i < flow.steps.length - 1 && <span className="flow__arrow">→</span>}
                </div>
              ))}
            </div>
            <p className="hint">金額門檻可隨時調整。{flow.enforcement_note}</p>

            {canEditFlow && (
            <div className="card form-card">
              <h3 className="section-title">
                {flow.steps.some((s) => s.step_no === parseInt(stepForm.step_no, 10))
                  ? `編輯第 ${stepForm.step_no} 關`
                  : '新增一關'}
              </h3>
              <p className="section-note">
                沿用既有編號即為編輯，填入新編號即為新增。
              </p>
              <div className="field-grid">
                <label className="field">
                  關卡編號
                  <input
                    className="input num"
                    inputMode="numeric"
                    value={stepForm.step_no}
                    onChange={(e) => setStepForm((f) => ({ ...f, step_no: e.target.value }))}
                  />
                </label>
                <label className="field">
                  關卡名稱
                  <input
                    className="input"
                    value={stepForm.label}
                    placeholder="會計確認科目"
                    onChange={(e) => setStepForm((f) => ({ ...f, label: e.target.value }))}
                  />
                </label>
                <label className="field">
                  負責角色
                  <select
                    className="input"
                    value={stepForm.role_code}
                    onChange={(e) => setStepForm((f) => ({ ...f, role_code: e.target.value }))}
                  >
                    <option value="">申請人本人</option>
                    {matrix.roles.map((r) => (
                      <option key={r.code} value={r.code}>
                        {r.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  金額門檻（留空表示一律需經本關）
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={stepForm.min_amount}
                    placeholder="10000"
                    onChange={(e) => setStepForm((f) => ({ ...f, min_amount: e.target.value }))}
                  />
                </label>
                <label className="field field--full">
                  條件說明
                  <input
                    className="input"
                    value={stepForm.condition_note}
                    placeholder="超過此金額才需經本關"
                    onChange={(e) => setStepForm((f) => ({ ...f, condition_note: e.target.value }))}
                  />
                </label>
              </div>
              <div className="wizard-actions">
                <button className="btn btn--ghost" onClick={openNewStep}>
                  清空並新增另一關
                </button>
                <button className="btn" disabled={savingStep} onClick={() => void saveStep()}>
                  {savingStep ? '存檔中…' : '儲存本關'}
                </button>
              </div>
            </div>
            )}
            </>
          )}
          </>
          )}
        </>
      )}
    </div>
  );
}
