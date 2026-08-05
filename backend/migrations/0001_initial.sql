-- Laysi One: initial schema
--
-- Single tenant: no table carries a company_id (a settled product decision).
--
-- The core of this file is three invariants enforced by the database. They
-- cannot live in Rust alone: MCP tools, importers and future scheduled jobs
-- all write these tables, and no path around the application layer may ever
-- store an unbalanced entry.

-- Chart of accounts

CREATE TABLE account (
    code         text PRIMARY KEY,
    name         text NOT NULL,
    kind         text NOT NULL CHECK (kind IN ('asset','liability','equity','revenue','expense')),
    normal_side  text NOT NULL CHECK (normal_side IN ('debit','credit')),
    -- Non-postable accounts (parent nodes) only aggregate their children and
    -- cannot take entries directly.
    is_postable  boolean NOT NULL DEFAULT true,
    parent_code  text REFERENCES account(code),
    sort_order   int NOT NULL DEFAULT 0
);

COMMENT ON COLUMN account.normal_side IS
    '正常餘額方向。資產費用在借方，負債權益收益在貸方；決定試算表如何呈現餘額。';

-- Bank accounts
-- Two accounts may share one GL account (both 1102), so they have to be
-- tracked separately. Balances are not stored here; they are always derived
-- from journal lines, because an account balance has one source of truth.

CREATE TABLE bank_account (
    id          serial PRIMARY KEY,
    name        text NOT NULL,
    account_no  text NOT NULL,
    gl_code     text NOT NULL REFERENCES account(code),
    sort_order  int NOT NULL DEFAULT 0
);

-- Vouchers

CREATE TABLE voucher (
    id           uuid PRIMARY KEY,
    no           text NOT NULL UNIQUE,
    voucher_date date NOT NULL,
    memo         text NOT NULL DEFAULT '',
    status       text NOT NULL CHECK (status IN ('draft','pending','posted','void')),
    source       text NOT NULL CHECK (source IN ('manual','ai_draft','import','payroll','invoice')),
    created_by   text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    posted_at    timestamptz,
    ai_audit_id  bigint
);

COMMENT ON COLUMN voucher.status IS
    'draft=AI 草擬待人確認；pending=已存待過帳；posted=已過帳；void=已作廢';
COMMENT ON COLUMN voucher.ai_audit_id IS
    '若這張傳票源自 MCP 工具呼叫，指回稽核紀錄。稽核要能雙向追。';

CREATE INDEX voucher_status_date_idx ON voucher (status, voucher_date DESC);

CREATE TABLE voucher_line (
    id              bigserial PRIMARY KEY,
    voucher_id      uuid NOT NULL REFERENCES voucher(id) ON DELETE CASCADE,
    line_no         int NOT NULL,
    account_code    text NOT NULL REFERENCES account(code),
    debit           numeric(18,2) NOT NULL DEFAULT 0,
    credit          numeric(18,2) NOT NULL DEFAULT 0,
    memo            text,
    bank_account_id int REFERENCES bank_account(id),

    -- Invariant 1: amounts are non-negative and a line cannot carry both a
    -- debit and a credit.
    CONSTRAINT line_amounts_non_negative CHECK (debit >= 0 AND credit >= 0),
    CONSTRAINT line_is_one_sided CHECK (NOT (debit > 0 AND credit > 0)),
    CONSTRAINT line_is_not_empty CHECK (debit > 0 OR credit > 0),
    UNIQUE (voucher_id, line_no)
);

CREATE INDEX voucher_line_voucher_idx ON voucher_line (voucher_id);
CREATE INDEX voucher_line_account_idx ON voucher_line (account_code);

-- Invariant 2: every voucher balances and has at least two lines.
--
-- It must be DEFERRABLE INITIALLY DEFERRED: lines are inserted one at a
-- time, so after the first line the voucher is necessarily unbalanced. The
-- check is only meaningful at the end of the transaction.
CREATE FUNCTION check_voucher_balanced() RETURNS trigger AS $$
DECLARE
    v_id     uuid;
    v_no     text;
    v_debit  numeric(18,2);
    v_credit numeric(18,2);
    v_lines  int;
BEGIN
    IF TG_TABLE_NAME = 'voucher' THEN
        v_id := NEW.id;
    ELSIF TG_OP = 'DELETE' THEN
        v_id := OLD.voucher_id;
    ELSE
        v_id := NEW.voucher_id;
    END IF;

    -- When the whole voucher is deleted its lines go with it, and no check is
    -- needed.
    SELECT no INTO v_no FROM voucher WHERE id = v_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    SELECT COALESCE(SUM(debit), 0), COALESCE(SUM(credit), 0), COUNT(*)
      INTO v_debit, v_credit, v_lines
      FROM voucher_line WHERE voucher_id = v_id;

    IF v_lines < 2 THEN
        RAISE EXCEPTION '傳票 % 只有 % 行分錄，複式記帳至少需兩行分錄', v_no, v_lines
            USING ERRCODE = 'check_violation';
    END IF;

    IF v_debit <> v_credit THEN
        RAISE EXCEPTION '傳票 % 借貸不相等：借方 %，貸方 %，差 % 元',
                        v_no, v_debit, v_credit, abs(v_debit - v_credit)
            USING ERRCODE = 'check_violation';
    END IF;

    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE CONSTRAINT TRIGGER voucher_line_must_balance
    AFTER INSERT OR UPDATE OR DELETE ON voucher_line
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION check_voucher_balanced();

CREATE CONSTRAINT TRIGGER voucher_must_balance
    AFTER INSERT OR UPDATE ON voucher
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION check_voucher_balanced();

-- Invariant 3: a posted voucher is immutable. Corrections always go through
-- a reversing entry.
--
-- This is a basic accounting requirement and the premise of an audit: if a
-- posted entry could be edited, the audit trail would mean nothing. The one
-- permitted transition is posted to void.
CREATE FUNCTION forbid_posted_voucher_mutation() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.status = 'posted' THEN
            RAISE EXCEPTION '傳票 % 已過帳，不得刪除。更正請開立反向分錄。', OLD.no
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN OLD;
    END IF;

    IF OLD.status = 'posted' THEN
        IF NEW.status = 'void' THEN
            RETURN NEW;  -- 允許作廢
        END IF;
        IF (NEW.no, NEW.voucher_date, NEW.memo, NEW.status)
           IS DISTINCT FROM (OLD.no, OLD.voucher_date, OLD.memo, OLD.status) THEN
            RAISE EXCEPTION '傳票 % 已過帳，不得修改。更正請開立反向分錄。', OLD.no
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER voucher_posted_is_immutable
    BEFORE UPDATE OR DELETE ON voucher
    FOR EACH ROW EXECUTE FUNCTION forbid_posted_voucher_mutation();

CREATE FUNCTION forbid_posted_line_mutation() RETURNS trigger AS $$
DECLARE
    v_status text;
    v_no     text;
    v_id     uuid;
BEGIN
    IF TG_OP = 'DELETE' THEN v_id := OLD.voucher_id; ELSE v_id := NEW.voucher_id; END IF;

    SELECT status, no INTO v_status, v_no FROM voucher WHERE id = v_id;
    IF FOUND AND v_status = 'posted' THEN
        RAISE EXCEPTION '傳票 % 已過帳，分錄不得異動。更正請開立反向分錄。', v_no
            USING ERRCODE = 'check_violation';
    END IF;

    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER voucher_line_posted_is_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON voucher_line
    FOR EACH ROW EXECUTE FUNCTION forbid_posted_line_mutation();

-- Document numbering
-- Format V{yy}{mm}-{nnn}, so the 18th voucher of 2026/07 is V2607-018.

CREATE TABLE doc_sequence (
    prefix   text PRIMARY KEY,
    period   text NOT NULL,
    next_no  int NOT NULL CHECK (next_no > 0)
);

-- Regulation
-- The authority for the rules is the TOML under backend/regulation/,
-- embedded at compile time. This table is a projection of it, so SQL
-- queries, audit reports and "which version produced this figure" can find
-- them.

CREATE TABLE reg_ruleset (
    id             serial PRIMARY KEY,
    domain         text NOT NULL,
    version        text NOT NULL,
    effective_from date NOT NULL,
    effective_to   date,
    source_note    text NOT NULL,
    payload        jsonb NOT NULL,
    checksum       text NOT NULL,
    UNIQUE (domain, version)
);

CREATE INDEX reg_ruleset_lookup_idx ON reg_ruleset (domain, effective_from DESC);

-- MCP tool grants
-- Every operation is an MCP tool and every tool can be switched off
-- individually. Once off, AI cannot even read through it.

CREATE TABLE mcp_tool_grant (
    tool_name  text PRIMARY KEY,
    enabled    boolean NOT NULL,
    risk       text NOT NULL CHECK (risk IN ('read_only','needs_confirm','high_risk','boss_only')),
    note       text NOT NULL DEFAULT '',
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- AI audit trail
-- Every tool call is recorded, read-only operations included: "the AI read
-- my payroll" is itself a fact that must be visible. Calls blocked by the
-- permission check are recorded too, and those matter most.

CREATE TABLE ai_audit_log (
    id             bigserial PRIMARY KEY,
    at             timestamptz NOT NULL DEFAULT now(),
    actor          text NOT NULL,
    actor_role     text NOT NULL,
    tool_name      text NOT NULL,
    risk           text NOT NULL,
    -- What the user actually said to the AI. Without it the record shows the
    -- action but not the intent.
    prompt         text,
    params         jsonb NOT NULL DEFAULT '{}'::jsonb,
    result_summary text NOT NULL DEFAULT '',
    outcome        text NOT NULL CHECK (outcome IN ('ok','blocked','error')),
    voucher_no     text,
    reverted       boolean NOT NULL DEFAULT false
);

CREATE INDEX ai_audit_log_at_idx ON ai_audit_log (at DESC);

ALTER TABLE voucher
    ADD CONSTRAINT voucher_ai_audit_fk FOREIGN KEY (ai_audit_id) REFERENCES ai_audit_log(id);
