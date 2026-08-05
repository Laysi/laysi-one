-- Laysi One: user accounts, external identities and login sessions
--
-- An account and an employee are different identities. `user_account` is not
-- merged into `employee`: an external accountant can use the system with
-- `actor_role = 'accountant'` without an employee row, because someone at a
-- practice was never on the company's payroll. Conversely an employee on the
-- roster does not necessarily need a login. `employee_id` is therefore
-- nullable and is linked only when the account really corresponds to an
-- employee.
--
-- External identities key on the IdP's subject. OAuth/OIDC login links an
-- account by the `sub` the identity provider issues, never by email. An
-- email address can be renamed and can be reassigned to someone else after a
-- departure; the `sub` under one issuer is the stable identifier that does
-- not follow mailbox changes. `email_at_link` is kept only as an audit trace
-- of the link and plays no part in later login matching.
--
-- Not every account needs a password. `password_hash IS NULL` means the
-- account can sign in only through OAuth/OIDC; it does not mean an empty
-- password. This migration lays the authorisation groundwork and seeds no
-- users; accounts are created by the laysi-admin CLI, so a deployment never
-- ships a default credential everybody knows.
--
-- Sessions store no plaintext token. `user_session` keeps only the hash, for
-- the same reason as `api_token`: if the database is copied, an attacker
-- cannot take a row and log in with it. The plaintext exists only at
-- issuance and on the client; the server hashes what it receives before
-- looking it up. A lost session is revoked and reissued, never read back out
-- of the database.

CREATE TABLE user_account (
    id                   serial PRIMARY KEY,
    username             text NOT NULL UNIQUE,
    display_name         text NOT NULL,
    email                text NOT NULL DEFAULT '',
    -- NULL means external identity login only, not an empty password.
    password_hash        text,
    role_code            text NOT NULL REFERENCES role(code),
    -- A practice account may have no employee record, and an employee may
    -- have no account.
    employee_id          int REFERENCES employee(id),
    status               text NOT NULL CHECK (status IN ('active','suspended')),
    must_change_password boolean NOT NULL DEFAULT false,
    created_at           timestamptz NOT NULL DEFAULT now(),
    created_by           text NOT NULL DEFAULT '',
    last_login_at        timestamptz
);

COMMENT ON TABLE user_account IS
    '登入帳號獨立於員工名冊；employee_id 為 NULL 時仍可代表事務所等外部使用者。';
COMMENT ON COLUMN user_account.password_hash IS
    'NULL 代表只能用 OAuth／OIDC 登入；本欄永不保存明文密碼。';

CREATE INDEX user_account_role_status_idx ON user_account (role_code, status);
CREATE INDEX user_account_employee_idx ON user_account (employee_id)
    WHERE employee_id IS NOT NULL;

CREATE TABLE user_identity (
    id            serial PRIMARY KEY,
    user_id       int NOT NULL REFERENCES user_account(id) ON DELETE CASCADE,
    provider      text NOT NULL,
    subject       text NOT NULL,
    email_at_link text NOT NULL DEFAULT '',
    linked_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);

COMMENT ON TABLE user_identity IS
    '外部登入身分以 IdP 的 provider 與穩定 subject 唯一識別；email 只留作連結當下的稽核線索。';

CREATE INDEX user_identity_user_idx ON user_identity (user_id);

CREATE TABLE user_session (
    id           serial PRIMARY KEY,
    token_hash   text NOT NULL UNIQUE,
    user_id      int NOT NULL REFERENCES user_account(id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    revoked_at   timestamptz,
    revoked_by   text NOT NULL DEFAULT '',
    user_agent   text NOT NULL DEFAULT '',
    ip           text NOT NULL DEFAULT ''
);

COMMENT ON TABLE user_session IS
    '登入工作階段只保存 token 雜湊；明文 token 不落地，撤銷後也不能復原。';
COMMENT ON COLUMN user_session.token_hash IS
    '登入 token 的雜湊。伺服器收到明文後先雜湊再比對，資料庫無法還原明文。';

CREATE INDEX user_session_user_idx ON user_session (user_id, created_at DESC);
CREATE INDEX user_session_live_idx ON user_session (expires_at)
    WHERE revoked_at IS NULL;
