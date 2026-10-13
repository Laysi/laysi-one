#!/usr/bin/env bun
// The update card's state machine, checked without a browser.
//
// advance() decides what the card says on every poll. The container behind the
// screen disappears mid-update, so the cases that matter are the ones where
// the answer is missing: it must end in one of five states and never sit in
// "in progress", and an old result must never be read as this request's.
//
//   bun scripts/update-test.ts

import { updateResult, type UpdateResultView } from "../web/src/api/client";
import {
  ACCEPT_WAIT_MS,
  GIVE_UP_MS,
  STORAGE_KEY,
  STORED_RUN_TTL_MS,
  UNCONFIRMED_MESSAGE,
  advance,
  clearRun,
  loadRun,
  refusedMessage,
  saveRun,
  startRun,
  within,
  type KeyValueStore,
  type Phase,
  type Probe,
} from "../web/src/update";

let failed = 0;

function ok(label: string, condition: boolean): void {
  console.log(`${condition ? "ok  " : "FAIL"} ${label}`);
  if (!condition) failed += 1;
}

const T0 = 1_000_000;
const run = () => startRun("11111111-aaaa", "v0.1.12", "v0.2.0", T0);

function result(status: string, outcome: UpdateResultView["outcome"], extra: Partial<UpdateResultView> = {}): UpdateResultView {
  return {
    request_id: "11111111-aaaa",
    found: true,
    status,
    outcome,
    running_version: null,
    offered_version: "v0.2.0",
    reason: null,
    ...extra,
  };
}

const none: UpdateResultView = {
  request_id: "11111111-aaaa",
  found: false,
  status: null,
  outcome: null,
  running_version: null,
  offered_version: null,
  reason: null,
};

const up = (r: UpdateResultView | null, version: string | null = "v0.1.12"): Probe => ({
  version,
  result: r,
  reachable: r !== null,
});
const down: Probe = { version: null, result: null, reachable: false };

function phase(p: Probe, at: number, from = run()): Phase {
  return advance(from, p, T0 + at).phase;
}

// The responses the server sends, against the schema the screen validates with.
ok("a result that was found parses", updateResult.safeParse(result("rolled-back", "restored", { running_version: "v0.1.12" })).success);
ok("a missing result parses", updateResult.safeParse(none).success);
ok("an outcome the screen does not know is rejected", !updateResult.safeParse(result("x", "mystery" as never)).success);

// Four end states.
ok("accepted and preparing are in progress", phase(up(result("accepted", "in_progress")), 5000).kind === "progress");
ok("switching is in progress", phase(up(result("switching", "in_progress")), 20000).kind === "progress");
{
  const p = phase(up(result("succeeded", "succeeded", { running_version: "v0.2.0" }), "v0.2.0"), 40000);
  ok("a succeeded result names the new version", p.kind === "succeeded" && p.version === "v0.2.0");
}
{
  const p = phase(up(result("succeeded-updater-kept", "succeeded"), "v0.2.0"), 40000);
  ok("the new version is running even when the old updater stays", p.kind === "succeeded" && p.version === "v0.2.0");
}
{
  const p = phase(up(result("rolled-back", "restored", { running_version: "v0.1.12" })), 90000);
  ok("a rolled-back result names the version that came back", p.kind === "failed" && p.restored && p.version === "v0.1.12");
}
{
  const p = phase(up(result("failed-unchanged", "restored")), 90000);
  ok("a failure before anything stopped falls back to the version it started on", p.kind === "failed" && p.restored && p.version === "v0.1.12");
}
{
  const p = phase(up(result("rollback-failed", "unrestored")), 90000);
  ok("a failed rollback is never reported as restored", p.kind === "failed" && !p.restored);
}
for (const status of ["refused-signature", "refused-downgrade", "refused-same-version", "refused-installed-unknown", "refused-disk-space", "refused-busy", "refused-blocked"]) {
  const p = phase(up(result(status, "refused")), 3000);
  ok(`${status} is not run, with its own sentence`, p.kind === "not-run" && p.why === "refused" && p.message.startsWith("更新沒有執行。") && p.message !== refusedMessage(null));
}
ok("a refusal the screen has no sentence for still says it did not run", refusedMessage("refused-something-new").startsWith("更新沒有執行。"));

// A result that is not this request's never reaches advance: the operation
// answers found=false for an id with no file, so the old run's answer cannot
// be taken for this one.
ok("no result yet is still in progress before the wait is over", phase(up(none), ACCEPT_WAIT_MS - 1).kind === "progress");
{
  const p = phase(up(none), ACCEPT_WAIT_MS);
  ok("an accepted that never appears is reported as not started", p.kind === "not-run" && p.why === "never-started");
}

// The container disappears.
ok("failed polls do not break the card", phase(down, 10000).kind === "progress");
{
  let r = run();
  let p: Phase = { kind: "progress" };
  for (const at of [3000, 6000, 9000]) ({ run: r, phase: p } = advance(r, down, T0 + at));
  ok("having lost the api, the card keeps waiting past the not-started deadline", r.wentDown && p.kind === "progress");
  const later = advance(r, down, T0 + ACCEPT_WAIT_MS + 5000);
  ok("and still waits at that point", later.phase.kind === "progress");
  const back = advance(r, up(result("rolled-back", "restored", { running_version: "v0.1.12" })), T0 + 120000);
  ok("the result written while the api was away is read when it returns", back.phase.kind === "failed");
}
{
  const afterDown = advance(run(), down, T0 + 3000).run;
  const p = advance(afterDown, up(none), T0 + ACCEPT_WAIT_MS + 1000).phase;
  ok("an api that went away and came back with no result is still waiting", p.kind === "progress");
}

// The version moving is enough when nothing answers.
{
  const p = phase(up(none, "v0.2.0"), 30000);
  ok("a new version with no readable result counts as success", p.kind === "succeeded" && p.version === "v0.2.0");
}
ok("a version that has not moved is not success", phase(up(none, "v0.1.12"), 30000).kind === "progress");
ok("a result still switching outranks a version that has already moved", phase(up(result("switching", "in_progress"), "v0.2.0"), 30000).kind === "progress");

// Never stall. Once the update was accepted, or the api went away, it may
// have run: running out of time is "could not confirm", never "did not run".
for (const [label, probe] of [["api gone", down], ["switching", up(result("switching", "in_progress"))]] as const) {
  const p = phase(probe, GIVE_UP_MS);
  ok(`after the give-up time (${label}) the card says it could not confirm`, p.kind === "unconfirmed" && p.message === UNCONFIRMED_MESSAGE);
}
{
  const accepted = advance(run(), up(result("accepted", "in_progress")), T0 + 3000).run;
  const p = advance(accepted, up(none), T0 + GIVE_UP_MS).phase;
  ok("an accepted request that then goes quiet is not reported as not run", p.kind === "unconfirmed");
}
{
  const p = phase(up(none), GIVE_UP_MS);
  ok("an accepted that never appeared stays not run, however long it has been", p.kind === "not-run" && p.why === "never-started");
}
ok("the could-not-confirm sentence asks for a reload and does not claim it did not run", UNCONFIRMED_MESSAGE.includes("重新整理") && !UNCONFIRMED_MESSAGE.includes("沒有執行"));

// Reload during an update: the id is kept in the browser.
function fakeStore(): KeyValueStore & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => void data.set(k, v),
    removeItem: (k) => void data.delete(k),
  };
}
{
  const store = fakeStore();
  const started = run();
  saveRun(store, started);
  const resumed = loadRun(store, T0 + 20000);
  ok("a stored run is resumed with the same request id", resumed !== null && resumed.requestId === started.requestId && resumed.from === "v0.1.12");
  const seen = advance(started, up(result("switching", "in_progress")), T0 + 6000).run;
  saveRun(store, seen);
  const after = loadRun(store, T0 + 30000);
  ok("what the card has already seen survives the reload", after !== null && after.seenResult);
  const p = advance(after as typeof seen, up(result("switching", "in_progress")), T0 + 30000).phase;
  ok("the resumed run keeps showing progress", p.kind === "progress");
  const shown = advance(after as typeof seen, up(result("rolled-back", "restored", { running_version: "v0.1.12" })), T0 + 90000).phase;
  ok("the resumed run shows the final state", shown.kind === "failed" && shown.restored);
  clearRun(store);
  ok("clearing removes the id", loadRun(store, T0 + 90000) === null && !store.data.has(STORAGE_KEY));
}
{
  const store = fakeStore();
  saveRun(store, run());
  const resumed = loadRun(store, T0 + 60 * 60 * 1000) as ReturnType<typeof run>;
  const p = advance(resumed, up(result("succeeded", "succeeded", { running_version: "v0.2.0" }), "v0.2.0"), T0 + 60 * 60 * 1000).phase;
  ok("a stored id whose result is already final shows that result", p.kind === "succeeded" && p.version === "v0.2.0");
}
{
  const store = fakeStore();
  saveRun(store, run());
  ok("a stored id older than a day is dropped, not resumed", loadRun(store, T0 + STORED_RUN_TTL_MS) === null && !store.data.has(STORAGE_KEY));
}
{
  const store = fakeStore();
  store.setItem(STORAGE_KEY, "{not json");
  ok("unreadable storage is not an update in progress", loadRun(store, T0) === null);
  store.setItem(STORAGE_KEY, JSON.stringify({ requestId: "", from: "a" }));
  ok("a malformed record is not an update in progress and is removed", loadRun(store, T0) === null && !store.data.has(STORAGE_KEY));
  ok("no storage at all is no update in progress", loadRun(null, T0) === null);
  const throwing: KeyValueStore = {
    getItem: () => { throw new Error("denied"); },
    setItem: () => { throw new Error("denied"); },
    removeItem: () => { throw new Error("denied"); },
  };
  saveRun(throwing, run());
  clearRun(throwing);
  ok("storage that refuses access does not break the card", loadRun(throwing, T0) === null);
}
{
  const store = fakeStore();
  saveRun(store, run());
  const fresh = startRun("22222222-bbbb", "v0.2.0", "v0.3.0", T0 + 1000);
  saveRun(store, fresh);
  const loaded = loadRun(store, T0 + 2000);
  ok("a new update replaces the stored id", loaded !== null && loaded.requestId === "22222222-bbbb");
}

// A fetch that never returns must not stop the loop.
{
  const hung = within(new Promise<never>(() => undefined), 20);
  const outcome = await hung.then(() => "resolved", () => "rejected");
  ok("a poll that hangs is cut off", outcome === "rejected");
  ok("a poll that answers passes through", (await within(Promise.resolve(7), 20)) === 7);
}

console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
