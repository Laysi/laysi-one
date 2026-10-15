import { z } from 'zod';
import { OpFailed, type UpdateResultView } from './api/client';

export const DOWNTIME_PHRASE = '約一分鐘';
export const POLL_MS = 3000;
export const ACCEPT_WAIT_MS = 60 * 1000;
export const GIVE_UP_MS = 45 * 60 * 1000;
export const GIVE_UP_MINUTES = GIVE_UP_MS / 60000;

export interface UpdateRun {
  requestId: string;
  from: string;
  offered: string;
  startedAt: number;
  seenResult: boolean;
  wentDown: boolean;
}

export interface Probe {
  version: string | null;
  result: UpdateResultView | null;
  reachable: boolean;
}

export type NotRunWhy = 'refused' | 'never-started' | 'error';

export const UNCONFIRMED_MESSAGE =
  '無法確認更新結果，請重新整理本頁查看目前版本。仍無法確認時請洽經銷商。';

export const UNREACHABLE_MESSAGE = '系統更新後一直沒有回應。請盡快洽經銷商。';

export type Phase =
  | { kind: 'progress' }
  | { kind: 'succeeded'; version: string }
  | { kind: 'failed'; restored: boolean; version: string }
  | { kind: 'not-run'; why: NotRunWhy; message: string }
  | { kind: 'unconfirmed'; message: string }
  | { kind: 'unreachable'; message: string };

export type UpdateAction = 'no-release' | 'unmanaged' | 'button' | 'admin-only' | 'hidden';

export function canRequestUpdate(permissions: readonly string[]): boolean {
  return permissions.includes('get_update_ticket');
}

export function updateAction(
  released: string | null,
  updaterPresent: boolean,
  canUpdate: boolean,
  phase: Phase | null,
): UpdateAction {
  if (!released) return phase ? 'hidden' : 'no-release';
  if (phase?.kind === 'progress') return 'hidden';
  if (
    phase?.kind === 'succeeded' ||
    phase?.kind === 'unconfirmed' ||
    phase?.kind === 'unreachable'
  ) {
    return 'hidden';
  }
  if (!updaterPresent) return 'unmanaged';
  return canUpdate ? 'button' : 'admin-only';
}

export function startRun(requestId: string, from: string, offered: string, now: number): UpdateRun {
  return { requestId, from, offered, startedAt: now, seenResult: false, wentDown: false };
}

const REFUSED: Record<string, string> = {
  'refused-signature': '版本清單的簽章無法驗證。請洽經銷商。',
  'refused-downgrade': '放行的版本低於目前版本。',
  'refused-same-version': '目前已經是放行的版本。',
  'refused-installed-unknown': '無法辨識目前的版本。請洽經銷商。',
  'refused-disk-space': '主機的磁碟空間不足。清出空間後可以再試。',
  'refused-busy': '另一次更新正在進行。請稍後再試。',
  'refused-blocked': '上一次更新回復時留下了資料庫備份，需要先檢查。請洽經銷商。',
};

export function refusedMessage(status: string | null, reason: string | null = null): string {
  const shortfall = reason?.trim().replace(/[。.]+$/, '');
  if (status === 'refused-disk-space' && shortfall) {
    return `更新沒有執行。${shortfall}。清出空間後可以再試。`;
  }
  const sentence = (status && REFUSED[status]) || '請洽經銷商。';
  return `更新沒有執行。${sentence}`;
}

function gaveUp(
  run: UpdateRun,
  probe: Probe,
  now: number,
): { run: UpdateRun; phase: Phase } | null {
  if (now - run.startedAt < GIVE_UP_MS) return null;
  if (!probe.reachable && probe.version === null) {
    return { run, phase: { kind: 'unreachable', message: UNREACHABLE_MESSAGE } };
  }
  return { run, phase: { kind: 'unconfirmed', message: UNCONFIRMED_MESSAGE } };
}

export function advance(
  run: UpdateRun,
  probe: Probe,
  now: number,
): { run: UpdateRun; phase: Phase } {
  const result = probe.result;
  if (result && result.found && result.outcome) {
    switch (result.outcome) {
      case 'succeeded':
        return {
          run,
          phase: {
            kind: 'succeeded',
            version: result.running_version ?? probe.version ?? run.offered,
          },
        };
      case 'restored':
        return {
          run,
          phase: { kind: 'failed', restored: true, version: result.running_version ?? run.from },
        };
      case 'unrestored':
        return {
          run,
          phase: { kind: 'failed', restored: false, version: result.running_version ?? run.from },
        };
      case 'refused':
        return {
          run,
          phase: {
            kind: 'not-run',
            why: 'refused',
            message: refusedMessage(result.status, result.reason),
          },
        };
      case 'in_progress':
        return gaveUp({ ...run, seenResult: true }, probe, now) ?? {
          run: { ...run, seenResult: true },
          phase: { kind: 'progress' },
        };
    }
  }

  if (probe.version && probe.version !== run.from) {
    return { run, phase: { kind: 'succeeded', version: probe.version } };
  }

  const next = probe.reachable ? run : { ...run, wentDown: true };
  const elapsed = now - run.startedAt;

  if (!next.seenResult && !next.wentDown && elapsed >= ACCEPT_WAIT_MS) {
    return {
      run: next,
      phase: {
        kind: 'not-run',
        why: 'never-started',
        message: '更新沒有啟動，系統照常使用。請稍後再試；仍然無法啟動時請洽經銷商。',
      },
    };
  }
  const over = gaveUp(next, probe, now);
  if (over) return over;
  return { run: next, phase: { kind: 'progress' } };
}

export function serverUnreachable(e: unknown): boolean {
  return !(e instanceof OpFailed) || e.error.code === 'network';
}

export function within<T>(work: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timeout')), ms);
    work.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

export const STORAGE_KEY = 'laysi.update.run';
export const STORED_RUN_TTL_MS = 24 * 60 * 60 * 1000;

export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const storedRun = z.object({
  requestId: z.string().min(1),
  from: z.string(),
  offered: z.string(),
  startedAt: z.number().finite(),
  seenResult: z.boolean(),
  wentDown: z.boolean(),
});

export function loadRun(store: KeyValueStore | null, now: number): UpdateRun | null {
  if (!store) return null;
  try {
    const raw = store.getItem(STORAGE_KEY);
    if (raw === null) return null;
    const parsed = storedRun.safeParse(JSON.parse(raw));
    if (parsed.success) {
      const age = now - parsed.data.startedAt;
      if (age >= 0 && age < STORED_RUN_TTL_MS) return parsed.data;
    }
    store.removeItem(STORAGE_KEY);
  } catch {
    return null;
  }
  return null;
}

export function saveRun(store: KeyValueStore | null, run: UpdateRun): void {
  try {
    store?.setItem(STORAGE_KEY, JSON.stringify(run));
  } catch {
    return;
  }
}

export function clearRun(store: KeyValueStore | null): void {
  try {
    store?.removeItem(STORAGE_KEY);
  } catch {
    return;
  }
}

export function browserStore(): KeyValueStore | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}
