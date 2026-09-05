-- Laysi 授權伺服器 schema.
--
-- Applied once at start-up by src/db.rs. Every statement is idempotent, so a
-- restart against an existing database is a no-op and there is no migration
-- runner to keep in step.
--
-- Six tables. The installation a licence is bound to lives on `license`
-- itself rather than in a table of its own: it is one row per licence either
-- way, and unbinding is then one UPDATE instead of a join.

CREATE TABLE IF NOT EXISTS partner (
    id serial PRIMARY KEY,
    partner_no text UNIQUE NOT NULL,
    name text NOT NULL,
    tax_id text NOT NULL DEFAULT '',
    -- What one licence-year costs this partner. Copied onto every term as it
    -- is sold, so a later price change does not rewrite what was invoiced.
    unit_price numeric(12, 0) NOT NULL DEFAULT 0,
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended')),
    note text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE partner IS '經銷商一家一列。每一家只看得到自己開立的授權。';

CREATE TABLE IF NOT EXISTS partner_user (
    id serial PRIMARY KEY,
    partner_id integer NOT NULL REFERENCES partner(id),
    email text UNIQUE NOT NULL,
    password_hash text NOT NULL,
    name text NOT NULL DEFAULT '',
    -- The vendor's own staff: sees every partner and may create them.
    is_vendor boolean NOT NULL DEFAULT false,
    -- Adds, suspends, demotes and resets the other members of this partner.
    -- The account created alongside the partner carries it.
    is_admin boolean NOT NULL DEFAULT false,
    -- Set on every generated password. The next sign-in lands on the change
    -- form and goes nowhere else until it is cleared.
    must_change_password boolean NOT NULL DEFAULT false,
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS partner_session (
    token_hash text PRIMARY KEY,
    user_id integer NOT NULL REFERENCES partner_user(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE TABLE IF NOT EXISTS license (
    id serial PRIMARY KEY,
    license_no text UNIQUE NOT NULL,
    partner_id integer NOT NULL REFERENCES partner(id),
    licensee text NOT NULL,
    licensee_tax_id text NOT NULL DEFAULT '',
    -- The activation key is shown once, at issue. Only the SHA-256 and the
    -- first characters are kept: enough to find the row and enough for a
    -- person to recognise it, not enough for whoever copies the database.
    key_hash text UNIQUE NOT NULL,
    key_prefix text NOT NULL,
    -- Always set, and chosen by the partner at issue and at every renewal:
    -- when the licence year ends is what they are invoicing, so the date is
    -- theirs rather than a term this server computes.
    expires_on date NOT NULL,
    status text NOT NULL DEFAULT 'issued'
        CHECK (status IN ('issued', 'active', 'suspended', 'revoked')),
    note text NOT NULL DEFAULT '',
    issued_at timestamptz NOT NULL DEFAULT now(),
    created_by integer REFERENCES partner_user(id),

    -- The installation this key is bound to, from the first check onwards.
    -- A different fingerprint is refused rather than rebound: rebinding on
    -- sight is the same as not binding at all.
    fingerprint text,
    install_id uuid,
    bound_at timestamptz,
    -- Rolling proof of continuity. Each answer carries a fresh nonce and the
    -- next check has to present the one before it, so two copies of the same
    -- database keep invalidating each other's. Counted, never blocked: a
    -- restore from backup looks the same as a clone, and locking a customer
    -- out on a heuristic is worse than telling a person about it.
    nonce text,
    conflicts integer NOT NULL DEFAULT 0,
    last_conflict_at timestamptz,
    last_seen_at timestamptz,
    last_ip text NOT NULL DEFAULT '',
    version text NOT NULL DEFAULT ''
);
COMMENT ON COLUMN license.nonce IS
    '上一次發出的續期識別字；下一次檢查必須帶著它回來，兩份複本會互相沖銷。';

CREATE INDEX IF NOT EXISTS license_partner_idx ON license (partner_id, issued_at DESC);

CREATE TABLE IF NOT EXISTS license_term (
    id serial PRIMARY KEY,
    license_id integer NOT NULL REFERENCES license(id) ON DELETE CASCADE,
    from_on date NOT NULL,
    to_on date NOT NULL,
    -- Which month this is invoiced in. Separate from from_on so that a term
    -- sold in advance is billed when it was sold.
    billed_on date NOT NULL DEFAULT current_date,
    -- The partner's unit price at the moment of sale, not a live lookup.
    amount numeric(12, 0) NOT NULL DEFAULT 0,
    created_by integer REFERENCES partner_user(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT license_term_range_check CHECK (to_on > from_on)
);
COMMENT ON TABLE license_term IS
    '開立與續約各一筆，期間由經銷商自己填；月結對帳讀的就是這張表。';

CREATE INDEX IF NOT EXISTS license_term_billing_idx ON license_term (billed_on);

CREATE TABLE IF NOT EXISTS license_denial (
    id serial PRIMARY KEY,
    license_id integer NOT NULL REFERENCES license(id) ON DELETE CASCADE,
    at timestamptz NOT NULL DEFAULT now(),
    fingerprint text NOT NULL DEFAULT '',
    ip text NOT NULL DEFAULT '',
    reason text NOT NULL
);
COMMENT ON TABLE license_denial IS
    '被拒絕的檢查，經銷商在畫面上看得到。擋下來而沒有人知道，等於沒有擋。';

CREATE INDEX IF NOT EXISTS license_denial_idx ON license_denial (license_id, at DESC);

