#!/usr/bin/env bun
/**
 * Rebuilds demo.laysi.one from nothing, on the host that serves it.
 *
 *   bun scripts/reset-demo.ts
 *
 * A shared evaluation install has no other way to stay usable: the first
 * visitor to post a voucher, approve a leave request or edit the company name
 * changes what the second one sees. Throwing the books away every night is
 * cheaper than deciding which of yesterday's changes were the demonstration
 * and which were somebody experimenting.
 *
 * The signing key, the database password and the setup token live in the
 * `secrets` volume and are left alone. Only the books and the documents go.
 *
 * Run by laysi-demo-reset.timer; see deploy/README.md.
 */

const DIR = process.env.DEMO_DIR ?? `${process.env.HOME}/laysi-one-demo`;
const PROJECT = "laysi-one-demo";
const COMPOSE = ["-f", "docker-compose.prod.yml", "-f", "docker-compose.demo.yml"];
const BASE = "http://127.0.0.1:8091";

// Three of the five sign-ins handed out with the invitation; `admin` comes
// from the bootstrap call and `demo` from seed-demo. Fixed rather than
// generated: the invitation is written once and the install is rebuilt every
// night, so a password that changed with the rebuild would be wrong by
// morning. They protect nothing -- everything behind them is fictional and
// gone by tomorrow.
const PASSWORD = process.env.DEMO_PASSWORD ?? "laysi-demo-2026";
const LOGINS = [
  { username: "boss", role: "boss", name: "陳文彬" },
  { username: "hr", role: "hr", name: "林淑芬" },
  { username: "acct", role: "acct", name: "黃佩儀" },
];

/**
 * Restores the five shared logins on every update, and refuses to let them be
 * deleted.
 *
 * Without it one visitor changes the demonstration password at ten in the
 * morning and everybody invited that day is locked out until the nightly
 * rebuild. Scoped to those usernames on purpose: an account a visitor creates
 * themselves still changes its password, gets suspended and gets deleted like
 * anywhere else, so the screens that do those things still tell the truth.
 */
const FREEZE = `
CREATE OR REPLACE FUNCTION demo_freeze_shared_login() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RETURN NULL;
    END IF;
    NEW.username := OLD.username;
    NEW.password_hash := OLD.password_hash;
    NEW.must_change_password := OLD.must_change_password;
    NEW.status := OLD.status;
    NEW.role_code := OLD.role_code;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS demo_freeze_shared_login ON user_account;
CREATE TRIGGER demo_freeze_shared_login
    BEFORE UPDATE OR DELETE ON user_account
    FOR EACH ROW
    WHEN (OLD.username IN ('admin', 'boss', 'hr', 'acct', 'demo'))
    EXECUTE FUNCTION demo_freeze_shared_login();
`;

function run(command: string, args: string[], stdin?: string) {
  const done = Bun.spawnSync([command, ...args], {
    cwd: DIR,
    stdin: stdin === undefined ? "inherit" : new TextEncoder().encode(stdin),
    stdout: "pipe",
    stderr: "pipe",
  });
  const out = done.stdout.toString();
  const err = done.stderr.toString();
  if (done.exitCode !== 0) {
    console.error(err.trim());
    fail(`${command} ${args.join(" ")} 失敗（結束碼 ${done.exitCode}）`);
  }
  if (err.trim() !== "") console.error(err.trim());
  return out.trim();
}

const compose = (...args: string[]) => run("podman-compose", ["-p", PROJECT, ...COMPOSE, ...args]);
const inApi = (...args: string[]) => run("podman", ["exec", "laysi-demo-api", ...args]);

function fail(message: string): never {
  console.error(message);
  process.exit(1);
}

async function waitFor(what: string, ready: () => boolean | Promise<boolean>, seconds: number) {
  for (let waited = 0; waited < seconds; waited += 2) {
    if (await ready()) return;
    await Bun.sleep(2000);
  }
  fail(`${what} 在 ${seconds} 秒內沒有就緒。`);
}

/** The books and the documents. Not `secrets`: the setup token in there is
 *  what the bootstrap call below presents, and regenerating the database
 *  password against a volume that survived would lock the database. */
function discardData() {
  compose("down");
  for (const volume of [`${PROJECT}_pgdata`, `${PROJECT}_rustfsdata`]) {
    // Absent on the first run, which is not a failure.
    Bun.spawnSync(["podman", "volume", "rm", volume], { stdout: "ignore", stderr: "ignore" });
  }
}

function start() {
  compose("up", "-d", "secrets");
  compose("up", "-d", "--force-recreate");
}

function healthy() {
  const out = Bun.spawnSync([
    "podman", "ps", "--format", "{{.Names}} {{.Status}}",
  ]).stdout.toString();
  return /laysi-demo-api\s+Up.*healthy/.test(out);
}

/** Written into the secrets volume on first start and unchanged since. */
function setupToken(): string {
  return run("podman", [
    "run", "--rm", "-v", `${PROJECT}_secrets:/secrets:ro`, "alpine:3.24",
    "cat", "/secrets/setup_token",
  ]);
}

/**
 * `admin`, and the licence, in one call.
 *
 * The web bootstrap is the only path that activates a licence without a
 * browser in front of it, and the demo needs one: a fresh database has no
 * grace window, and an unlicensed install is read-only, which is the one
 * thing an evaluation must not be.
 */
async function bootstrap(activationKey: string) {
  const response = await fetch(`${BASE}/v1/auth/bootstrap`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      password: PASSWORD,
      setup_token: setupToken(),
      activation_key: activationKey,
    }),
  });
  if (!response.ok) fail(`建立系統管理員帳號失敗：${response.status} ${await response.text()}`);
}

function seed() {
  for (const { username, role, name } of LOGINS) {
    inApi("laysi-admin", "create-user",
      "--username", username, "--role", role, "--display-name", name, "--password", PASSWORD);
  }
  // Seeds the books, links the boss account to its employee row, and creates
  // the `demo` staff login for the self-service screens.
  inApi("laysi-admin", "seed-demo", "--password", PASSWORD);
}

function freezeSharedLogins() {
  run("podman", ["exec", "-i", "laysi-demo-postgres", "psql", "-U", "laysi", "-d", "laysi", "-v", "ON_ERROR_STOP=1", "-q"], FREEZE);
}

const activationKey = process.env.DEMO_ACTIVATION_KEY?.trim();
if (!activationKey) fail("DEMO_ACTIVATION_KEY 未設定，授權啟用不了，示範站會是唯讀的。");

discardData();
start();
await waitFor("示範站的 api", healthy, 180);
await bootstrap(activationKey);
seed();
freezeSharedLogins();

console.log(`示範站已重建：https://demo.laysi.one　登入 admin／boss／hr／acct／demo，密碼 ${PASSWORD}`);
