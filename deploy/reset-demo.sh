#!/bin/sh
# Rebuilds demo.laysi.one from nothing, on the host that serves it.
#
#   sh reset-demo.sh
#
# A shared evaluation install has no other way to stay usable: the first
# visitor to post a voucher, approve a leave request or edit the company name
# changes what the second one sees. Throwing the books away every night is
# cheaper than deciding which of yesterday's changes were the demonstration
# and which were somebody experimenting.
#
# POSIX sh rather than the bun+ts every other script here is written in: this
# one runs on the deployment host, where podman and curl are the only tools
# the stack already needs. Adding a runtime to a host in order to reset a demo
# is a dependency that has to be installed, upgraded and remembered.
#
# The signing key, the database password and the setup token live in the
# `secrets` volume and are left alone. Only the books and the documents go.
#
# Run by laysi-demo-reset.timer; see deploy/README.md.

set -eu

# A systemd user unit inherits the user manager's PATH, and an ssh command a
# non-login shell's; neither has ~/bin, where this host keeps the podman and
# podman-compose it calls below. Without this the run ends at the first one
# with `not found`, and the timer reports it at four in the morning.
PATH=$HOME/bin:$PATH
export PATH

DIR=${DEMO_DIR:-$HOME/laysi-one-demo}
PROJECT=laysi-one-demo
BASE=http://127.0.0.1:8091

# The sign-ins handed out with the invitation, all five sharing one password.
# Fixed rather than generated: the invitation is written once and the install
# is rebuilt every night, so a password that changed with the rebuild would be
# wrong by morning. They protect nothing -- everything behind them is
# fictional and gone by tomorrow.
PASSWORD=${DEMO_PASSWORD:-laysi-demo-2026}

die() { echo "$*" >&2; exit 1; }

compose() { podman-compose -p "$PROJECT" -f docker-compose.prod.yml -f docker-compose.demo.yml "$@"; }

[ -n "${DEMO_ACTIVATION_KEY:-}" ] || die "DEMO_ACTIVATION_KEY 未設定，授權啟用不了，示範站會是唯讀的。"
command -v curl >/dev/null || die "找不到 curl。"
# The password is interpolated into a JSON string below rather than encoded,
# so a quote or a backslash in it would produce a body the server cannot read.
case $PASSWORD in *[\"\\]*) die "DEMO_PASSWORD 不能含有引號或反斜線。" ;; esac
cd "$DIR" || die "找不到 $DIR。"

# The books and the documents. Not `secrets`: the setup token in there is what
# the bootstrap call below presents, and regenerating the database password
# against a pgdata volume that survived would lock the database.
compose down
podman volume rm "${PROJECT}_pgdata" >/dev/null 2>&1 || true
podman volume rm "${PROJECT}_rustfsdata" >/dev/null 2>&1 || true

compose up -d secrets
compose up -d --force-recreate

waited=0
until podman ps --format '{{.Names}} {{.Status}}' | grep -q 'laysi-demo-api Up.*healthy'; do
    waited=$((waited + 5))
    [ "$waited" -lt 300 ] || die "示範站的 api 在 300 秒內沒有就緒。"
    sleep 5
done

# Written into the secrets volume on first start and unchanged since.
token=$(podman run --rm -v "${PROJECT}_secrets:/secrets:ro" alpine:3.24 cat /secrets/setup_token)

# `admin`, and the licence, in one call. The web bootstrap is the only path
# that activates a licence without a browser in front of it, and the demo
# needs one: a fresh database has no grace window, and an unlicensed install
# is read-only, which is the one thing an evaluation must not be.
code=$(printf '{"password":"%s","setup_token":"%s","activation_key":"%s"}' \
        "$PASSWORD" "$token" "$DEMO_ACTIVATION_KEY" |
    curl -sS -o /tmp/laysi-demo-bootstrap.json -w '%{http_code}' \
        -X POST "$BASE/v1/auth/bootstrap" -H 'content-type: application/json' --data @-)
case $code in
    2*) ;;
    *) die "建立系統管理員帳號失敗：$code $(cat /tmp/laysi-demo-bootstrap.json)" ;;
esac

admin() { podman exec laysi-demo-api laysi-admin "$@" >/dev/null; }
admin create-user --username boss --role boss --display-name 陳文彬 --password "$PASSWORD"
admin create-user --username hr   --role hr   --display-name 林淑芬 --password "$PASSWORD"
admin create-user --username acct --role acct --display-name 黃佩儀 --password "$PASSWORD"
# Seeds the books, links the boss account to its employee row, and creates the
# `demo` staff login for the self-service screens.
admin seed-demo --password "$PASSWORD"

# Restores the five shared logins on every update, and refuses their deletion.
#
# Without it one visitor changes the demonstration password at ten in the
# morning and everybody invited that day is locked out until the nightly
# rebuild. Scoped to those usernames on purpose: an account a visitor creates
# themselves still changes its password, gets suspended and gets deleted like
# anywhere else, so the screens that do those things still tell the truth.
podman exec -i laysi-demo-postgres psql -U laysi -d laysi -v ON_ERROR_STOP=1 -q <<'SQL'
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
SQL

# The last thing checked is the thing visitors actually reach. Everything
# above can succeed while the site is down: `web` is the only container the
# tunnel talks to, and it is the one the api's health says nothing about.
waited=0
until curl -fsS -o /dev/null "$BASE/v1/auth/me" -w '' 2>/dev/null ||
      [ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/v1/auth/me")" = 401 ]; do
    waited=$((waited + 5))
    [ "$waited" -lt 60 ] || die "api 起來了但沒有回應，示範站沒有重建完成。"
    sleep 5
done
podman ps --format '{{.Names}}' | grep -qx laysi-demo-web ||
    die "laysi-demo-web 沒有在執行，示範站對外會是 502。"

echo "示範站已重建：https://demo.laysi.one　登入 admin／boss／hr／acct／demo，密碼 $PASSWORD"
