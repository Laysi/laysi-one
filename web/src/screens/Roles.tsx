import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  type ApprovalRules,
  type ApprovalStep,
  type GrantCell,
  type RoleMatrix,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

/** Three states per cell, cycled by clicking, matching the table's level
 *  column. */
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

export function Roles({ onToast, reloadKey }: Props) {
  const [matrix, setMatrix] = useState<RoleMatrix | null>(null);
  const [flow, setFlow] = useState<ApprovalRules | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  // set_approval_rule is owner only. This does not decide whether you are
  // the owner: the backend checks on click, and its wording is shown when
  // the call is refused.
  const [stepForm, setStepForm] = useState<StepForm>(emptyStepForm(1));
  const [savingStep, setSavingStep] = useState(false);

  // Every cell is clickable. Whether it takes effect is the backend's
  // permission check. On a screen about who may do what, the frontend
  // computing that itself is exactly what it must not demonstrate.
  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<RoleMatrix>('list_roles'),
      callOp<ApprovalRules>('list_approval_rules'),
    ])
      .then(([m, f]) => {
        setMatrix(m);
        setFlow(f);
      })
      .catch((e: Error) => setError(e.message));
  }, []);

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

  async function toggle(role: string, perm: string, current: Level) {
    const next = CYCLE[(CYCLE.indexOf(current) + 1) % CYCLE.length] as Level;
    const key = `${role}/${perm}`;
    setBusy(key);
    try {
      await callOp<unknown>('set_role_permission', {
        role_code: role,
        permission_code: perm,
        level: next,
      });
      onToast(`已將${role}的該項權限設為「${LEVEL_NOTE[next]}」，下一次呼叫即適用`);
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
      <p className="page-lede">角色權限與簽核關卡設定，僅負責人可異動。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!matrix && !error && <p className="state-note">讀取中…</p>}

      {matrix && flow && (
        <>
          {/* This paragraph must stay, though it now says the opposite: this
              matrix really does stop people. */}
          <p className={`recon${matrix.enforced ? '' : ' recon--off'}`}>
            <strong>{matrix.enforced ? '本矩陣執行中。' : '本矩陣目前未執行。'}</strong>{' '}
            {matrix.enforcement_note}
          </p>

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
                      <span className="tag tag--pale tag--static">對應操作尚未實作</span>
                    )}
                    <span className="cell-hint cell-hint--block">
                      {p.op_names.length > 0 ? (
                        <>對應 {p.op_names.join('、')}</>
                      ) : (
                        p.note
                      )}
                    </span>
                    {p.boss_only_ops.length > 0 && (
                      <span className="cell-hint cell-hint--block">
                        其中 {p.boss_only_ops.join('、')} 另標示為「限老闆」，為本列之外的
                        額外限制；即使本列權限給滿，非負責人仍無法通過。
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
                          title={`點選以切換：${LEVEL_NOTE[level]} → ${
                            LEVEL_NOTE[CYCLE[(CYCLE.indexOf(level) + 1) % CYCLE.length] as Level]
                          }`}
                          disabled={busy === key}
                          onClick={() => void toggle(r.code, p.code, level)}
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

          <p className="hint">
            ● 完整　○ 有限　— 無。點選欄位可切換，並記錄異動人員與時間。
            變更權限本身也需具備權限，權限不足時會被拒絕並顯示擋下的規則，
            該次呼叫同樣寫入稽核。
          </p>
          <p className="hint">
            「— 無」要特別留意：本表採 deny-by-default，關閉任一欄位後，該角色下一次呼叫
            即遭拒絕，不用重新登入或重啟，
            <strong>含關閉本項授權</strong>。
          </p>
          <p className="hint">
            工具開關（「MCP 與 AI」那一頁）管的是 <span className="num">AI</span> 能呼叫什麼，
            本矩陣管的是<span className="num">人員</span>能做什麼。將 post_voucher 對 AI 關閉，
            不應影響會計人員過帳，因此兩者分屬不同設定表。
          </p>

          <h3 className="section-title section-title--spaced">簽核流程</h3>
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
                  <div className="flow__actions">
                    <button className="btn btn--compact btn--secondary" onClick={() => openEditStep(s)}>
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
                </div>
                {i < flow.steps.length - 1 && <span className="flow__arrow">→</span>}
              </div>
            ))}
          </div>
          <p className="hint">
            金額門檻屬公司政策，存於資料表而非程式碼；各公司門檻不同，調整門檻不應需要重新
            編譯。{flow.enforcement_note}
          </p>

          <div className="card form-card">
            <h3 className="section-title">
              {flow.steps.some((s) => s.step_no === parseInt(stepForm.step_no, 10))
                ? `編輯第 ${stepForm.step_no} 關`
                : '新增一關'}
            </h3>
            <p className="section-note">
              關卡編號相同時直接覆寫：沿用既有編號即為編輯，填入新編號即為新增。
              本操作標示為「限老闆」，權限不足時會被拒絕並顯示後端訊息。
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
        </>
      )}
    </div>
  );
}
