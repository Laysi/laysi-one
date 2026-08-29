import { useCallback, useEffect, useState } from 'react';
import { useCurrentUser } from '../auth';
import { DateField } from '../components/DateField';
import {
  businessToday,
  callOp,
  OpFailed,
  regulationList,
  type RegulationField,
  type RegulationList,
  type RegulationVersion,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

interface Draft {
  domain: string;
  version: string;
  effective_from: string;
  effective_to: string;
  source_note: string;
  payload: Record<string, unknown>;
  fields: RegulationField[];
  isNew: boolean;
}

function setPath(payload: Record<string, unknown>, path: string, value: string) {
  const keys = path.split('.');
  const last = keys.pop() ?? '';
  const next = structuredClone(payload);
  let node: Record<string, unknown> = next;
  for (const key of keys) {
    node = node[key] as Record<string, unknown>;
  }
  node[last] = value;
  return next;
}

function getPath(payload: Record<string, unknown>, path: string) {
  let node: unknown = payload;
  for (const key of path.split('.')) {
    node = (node as Record<string, unknown>)[key];
  }
  return typeof node === 'string' ? node : '';
}

function Fields({
  nodes,
  editable,
  valueAt,
  onEdit,
}: {
  nodes: RegulationField[];
  editable: boolean;
  valueAt: (node: RegulationField) => string;
  onEdit: (path: string, value: string) => void;
}) {
  return (
    <div className="field-grid">
      {nodes.map((n) =>
        n.children ? (
          <div className="field field--full reg-group" key={n.path}>
            <h4 className="section-title">{n.label}</h4>
            <Fields nodes={n.children} editable={editable} valueAt={valueAt} onEdit={onEdit} />
          </div>
        ) : editable && n.editable ? (
          <label className="field" key={n.path}>
            {n.label}
            <input
              className="input num"
              value={valueAt(n)}
              onChange={(e) => onEdit(n.path, e.target.value)}
            />
          </label>
        ) : (
          <div className="field" key={n.path}>
            {n.label}
            <span className="field__fixed num">{valueAt(n)}</span>
          </div>
        ),
      )}
    </div>
  );
}

export function Regulations({ onToast, reloadKey }: Props) {
  const me = useCurrentUser();
  const canEdit = me.permissions.includes('save_regulation');
  const canDelete = me.permissions.includes('delete_regulation');

  const [data, setData] = useState<RegulationList | null>(null);
  const [domain, setDomain] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    callOp('list_regulations', {}, undefined, regulationList)
      .then((d) => {
        setData(d);
        setDomain((now) => now ?? d.domains[0]?.domain ?? null);
      })
      .catch(() => setData(null));
  }, []);

  useEffect(load, [load, reloadKey]);

  const chosen = data?.domains.find((d) => d.domain === domain) ?? null;

  const edit = (v: RegulationVersion, isNew: boolean) =>
    setDraft({
      domain: chosen!.domain,
      version: isNew ? '' : v.version,
      effective_from: isNew ? businessToday() : v.effective_from,
      effective_to: v.effective_to ?? '',
      source_note: v.source_note,
      payload: v.payload,
      fields: v.fields,
      isNew,
    });

  const save = async () => {
    if (!draft) return;
    setBusy(true);
    try {
      await callOp('save_regulation', {
        domain: draft.domain,
        version: draft.version,
        effective_from: draft.effective_from,
        effective_to: draft.effective_to || null,
        source_note: draft.source_note,
        payload: draft.payload,
      });
      onToast(`${chosen?.label} ${draft.version} 已儲存`);
      setDraft(null);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (version: string) => {
    setBusy(true);
    try {
      await callOp('delete_regulation', { domain, version });
      onToast(`${chosen?.label} ${version} 已刪除`);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <h1 className="page-title">法規版本</h1>
      {!data && <p className="state-note">讀取中…</p>}

      {data && (
        <div className="split-columns split-columns--wide-right">
          <div className="choice-list">
            {data.domains.map((d) => (
              <button
                key={d.domain}
                className={`choice${d.domain === domain ? ' choice--on' : ''}`}
                onClick={() => {
                  setDomain(d.domain);
                  setDraft(null);
                }}
              >
                <span className="choice__title">{d.label}</span>
                <span className="choice__desc">
                  {d.applicable ? (
                    <>
                      現行 {d.version} · 共 {d.versions.length} 版
                    </>
                  ) : (
                    <span className="tag tag--alarm tag--static">今天沒有適用的版本</span>
                  )}
                </span>
              </button>
            ))}
          </div>

          {chosen && (
            <div>
              <div className="toolbar toolbar--spaced">
                <h3 className="section-title">{chosen.label}</h3>
                {canEdit &&
                  !draft &&
                  (() => {
                    const base = chosen.versions.find((v) => v.in_force) ?? chosen.versions[0];
                    return (
                      base && (
                        <button className="btn btn--secondary" onClick={() => edit(base, true)}>
                          ＋ 新增版本
                        </button>
                      )
                    );
                  })()}
              </div>

              {!draft && (
                <div className="table-scroll">
                  <table className="table">
                    <thead>
                      <tr>
                        <th>版本</th>
                        <th>生效日</th>
                        <th>終止日</th>
                        <th>法源</th>
                        <th className="align-right" />
                      </tr>
                    </thead>
                    <tbody>
                      {chosen.versions.map((v) => (
                        <tr key={v.version}>
                          <td className="num">
                            {v.version}
                            {v.in_force && <span className="tag tag--spaced">現行</span>}
                          </td>
                          <td className="num">{v.effective_from}</td>
                          <td className="num cell-muted">{v.effective_to ?? '仍適用'}</td>
                          <td className="cell-muted">{v.source_note}</td>
                          <td className="align-right">
                            <span className="cell-actions">
                              <button
                                className="btn btn--ghost btn--compact"
                                onClick={() => edit(v, false)}
                              >
                                {canEdit ? '編輯' : '檢視'}
                              </button>
                              {canDelete && chosen.versions.length > 1 && (
                                <button
                                  className="btn btn--ghost btn--compact"
                                  disabled={busy}
                                  onClick={() => void remove(v.version)}
                                >
                                  刪除
                                </button>
                              )}
                            </span>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}

              {draft && (
                <div className="card form-card">
                  <div className="field-grid">
                    <label className="field">
                      版本
                      <input
                        className="input num"
                        value={draft.version}
                        readOnly={!draft.isNew}
                        placeholder="2027.1"
                        onChange={(e) => setDraft({ ...draft, version: e.target.value })}
                      />
                    </label>
                    <label className="field">
                      生效日
                      <DateField
                        className="input input--date"
                        value={draft.effective_from}
                        onChange={(v) => setDraft({ ...draft, effective_from: v })}
                      />
                    </label>
                    <label className="field">
                      終止日
                      <DateField
                        className="input input--date"
                        value={draft.effective_to}
                        onChange={(v) => setDraft({ ...draft, effective_to: v })}
                      />
                      <span className="field__hint">仍適用就留空。</span>
                    </label>
                    <label className="field field--full">
                      法源
                      <input
                        className="input"
                        value={draft.source_note}
                        placeholder="勞工保險條例第 13 條"
                        onChange={(e) => setDraft({ ...draft, source_note: e.target.value })}
                      />
                    </label>
                  </div>

                  <Fields
                    nodes={draft.fields}
                    editable={canEdit}
                    valueAt={(n) => (n.editable ? getPath(draft.payload, n.path) : (n.value ?? ''))}
                    onEdit={(path, value) =>
                      setDraft({ ...draft, payload: setPath(draft.payload, path, value) })
                    }
                  />

                  <div className="wizard-actions">
                    <button className="btn btn--ghost" onClick={() => setDraft(null)}>
                      取消
                    </button>
                    {canEdit && (
                      <button className="btn" disabled={busy} onClick={() => void save()}>
                        儲存
                      </button>
                    )}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
