-- Laysi One: e-invoices, VAT and withholding, fixed assets and depreciation
--
-- The theme of this module: not one regulation value enters the database.
--
-- No tax rate, useful life, withholding rate or supplementary premium rate
-- appears in this file. They live only in
-- `backend/regulation/<year>/*.toml` and are resolved by
-- `laysi-regulation` from an effective date. What is stored here are the
-- computed results (the tax on an invoice, a month's depreciation), each
-- carrying which version of the rules produced it:
--
--   einvoice.vat_version                  VAT rules used for this invoice
--   depreciation_run.ruleset_version      rules used for this run
--   depreciation_entry.useful_life_years  the life resolved at the time
--
-- Storing results is not a second copy of the rules; the rules exist only in
-- the TOML. They are stored to answer the question a retroactive amendment
-- raises: which rows were computed under the old version and need redoing.
-- Without these columns an update could only be applied by rerunning
-- everything, which would overwrite figures already filed.
--
-- The ledger is still the only source of truth:
--
--   issuing an invoice -> voucher (Dr 1131, Cr 4111 and 2211)
--   depreciation       -> voucher (Dr 6152, Cr 1602)
--
-- The detail tables (einvoice, fixed_asset) answer whose it is, which
-- document, which asset; balances always come from the ledger. Hence:
--
--   1. Output tax on the VAT return is the period movement of 2211, not a
--      sum over einvoice.
--   2. Net book value is 1601 minus 1602, not a sum over the asset register.
--
-- The operation layer computes both sides and compares, surfacing any
-- mismatch on screen, the same approach as receivables and payables (see
-- 0004 and ops/receivables.rs).

-- Accounts: accumulated depreciation and input tax
--
-- 1602 accumulated depreciation is a contra asset with a credit balance.
-- Depreciation is not netted straight against 1601 because tax treatment
-- needs the original cost visible: disposal and sale are computed on cost,
-- not net book value.
--
-- 1284 input tax is the counterpart of 2211 output tax. The input tax on the
-- VAT return needs a ledger source, or "how much is creditable" is just an
-- unverifiable number.

INSERT INTO account (code, name, kind, normal_side, sort_order) VALUES
    ('1284', '進項稅額',   'asset', 'debit',  45),
    ('1602', '累計折舊',   'asset', 'credit', 65);

COMMENT ON TABLE account IS
    '會計科目表。1602 累計折舊屬資產類但正常餘額在貸方，為 1601 的抵減科目。';

-- The origin of a depreciation voucher must be visible: not entered by a
-- person, not drafted by AI, but charged by the system from the rules.
ALTER TABLE voucher DROP CONSTRAINT voucher_source_check;
ALTER TABLE voucher ADD CONSTRAINT voucher_source_check
    CHECK (source IN ('manual','ai_draft','import','payroll','invoice','depreciation'));

-- E-invoices
--
-- A track is a number range allocated by the tax bureau; once used up no
-- more can be issued. next_no is stored rather than derived from
-- MAX(serial_no)+1, because voided numbers are consumed too and a derived
-- value would reissue them.

CREATE TABLE einvoice_track (
    track      text PRIMARY KEY,
    -- Filing period, two months each; '2607' is the Jul-Aug period of 2026.
    period     text NOT NULL,
    range_from int  NOT NULL,
    range_to   int  NOT NULL,
    next_no    int  NOT NULL,

    -- next_no = range_to + 1 means the track is exhausted.
    CONSTRAINT einvoice_track_next_within_range
        CHECK (range_from <= next_no AND next_no <= range_to + 1),
    CONSTRAINT einvoice_track_range_is_forward CHECK (range_from <= range_to)
);

COMMENT ON TABLE einvoice_track IS
    '國稅局配發的發票字軌與號碼區間。開一張用掉一號，作廢也算用掉。';

CREATE TABLE einvoice (
    id            serial PRIMARY KEY,
    track         text NOT NULL,
    serial_no     int  NOT NULL,
    -- Full number for display, e.g. AB-12345678.
    invoice_no    text NOT NULL UNIQUE,
    buyer_tax_id  text NOT NULL DEFAULT '',
    buyer         text NOT NULL,
    item          text NOT NULL DEFAULT '',
    issued_on     date NOT NULL,
    net_amount    numeric(18,2) NOT NULL CHECK (net_amount > 0),
    tax_amount    numeric(18,2) NOT NULL CHECK (tax_amount >= 0),
    total_amount  numeric(18,2) NOT NULL,

    -- Which version of the VAT rules produced this tax. A retroactive
    -- amendment uses this column to find what needs recomputing.
    vat_version   text NOT NULL,

    -- queued   issued, not yet submitted (the tax bureau MIG interface is not
    --          connected yet)
    -- uploaded accepted by the platform
    -- failed   rejected after submission and can be retried
    -- void     voided; the number is still consumed
    upload_status   text NOT NULL DEFAULT 'queued'
        CHECK (upload_status IN ('queued','uploaded','failed','void')),
    upload_attempts int  NOT NULL DEFAULT 0 CHECK (upload_attempts >= 0),
    uploaded_at     timestamptz,
    last_error      text,

    -- The voucher that raised this invoice. NULL means an opening import,
    -- where the tax is already inside the opening balance.
    voucher_no    text REFERENCES voucher(no),
    -- The corresponding row in the receivables detail.
    ar_invoice_no text REFERENCES ar_invoice(invoice_no),
    created_by    text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT einvoice_total_is_the_sum CHECK (total_amount = net_amount + tax_amount),
    CONSTRAINT einvoice_serial_used_once  UNIQUE (track, serial_no)
);

COMMENT ON COLUMN einvoice.vat_version IS
    '開立當時適用的營業稅規則版本。稅額不是重算出來的，是當初算好存下來的。';
COMMENT ON COLUMN einvoice.upload_status IS
    'queued 已開立待送出；uploaded 平台已接收；failed 被退回可重送；void 已作廢。';

CREATE INDEX einvoice_issued_idx ON einvoice (issued_on DESC, serial_no DESC);
CREATE INDEX einvoice_failed_idx ON einvoice (upload_status) WHERE upload_status = 'failed';

-- Withholding on various income
--
-- This table stores the fact of payment: to whom, what kind of income, how
-- much. How much to withhold is not stored: the rate and threshold are
-- regulation values, always asked of the withholding ruleset as of the
-- payment date.
--
-- Salary is the one exception: the Income Tax Act allows the withholding
-- table to be applied per person per bracket, and that table belongs to the
-- payroll module rather than being a single rate. Such records are marked
-- basis = 'table' and carry the tax computed by the source system, which is
-- more honest than pretending it came from a rate.

CREATE TABLE withholding_record (
    id           serial PRIMARY KEY,
    paid_on      date NOT NULL,
    payee        text NOT NULL,
    payee_id     text NOT NULL DEFAULT '',
    -- Income code on the withholding statement, matching
    -- payload.codes[].code in the withholding ruleset.
    income_code  text NOT NULL,
    gross_amount numeric(18,2) NOT NULL CHECK (gross_amount > 0),

    basis text NOT NULL CHECK (basis IN ('rate','table')),
    -- NULL when basis = 'rate': the tax is derived from the ruleset, and
    -- storing it would be a second copy.
    withheld_amount numeric(18,2) CHECK (withheld_amount >= 0),

    -- Whether this payment falls in a chargeable category for the
    -- second-generation health insurance premium. This classifies the
    -- payment and is decided by whoever raises it; the rate is in the
    -- ruleset.
    supplementary_base boolean NOT NULL DEFAULT false,

    memo       text NOT NULL DEFAULT '',
    -- Payment voucher. May be NULL until payroll or expenses creates one.
    voucher_no text REFERENCES voucher(no),
    created_at timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT withholding_table_basis_carries_its_own_tax
        CHECK (basis = 'rate' OR withheld_amount IS NOT NULL)
);

COMMENT ON TABLE withholding_record IS
    '各類所得給付紀錄。應扣繳稅款由 withholding ruleset 依給付日現算，不存欄位。';

CREATE INDEX withholding_paid_idx ON withholding_record (paid_on DESC);

-- Fixed assets
--
-- The useful life is not in this table. It is fixed by the schedule in
-- article 51 of the Income Tax Act, which is regulation and may live only
-- in TOML; only the category is stored here, and the life is asked of the
-- ruleset. When the schedule is amended, assets not yet fully depreciated
-- follow automatically while charges already made stand.

CREATE TABLE fixed_asset (
    id          serial PRIMARY KEY,
    code        text NOT NULL UNIQUE,
    name        text NOT NULL,
    -- Key into payload.useful_lives of the depreciation ruleset.
    category    text NOT NULL,
    acquired_on date NOT NULL,
    cost        numeric(18,2) NOT NULL CHECK (cost > 0),
    method      text NOT NULL DEFAULT 'straight_line',
    -- Which accounts carry the cost and the accumulated depreciation. Net
    -- book value must equal the difference of their ledger balances.
    gl_code       text NOT NULL REFERENCES account(code),
    accum_gl_code text NOT NULL REFERENCES account(code),
    -- The acquisition voucher. Until it is posted the asset is not in the
    -- ledger and is excluded from the cost total.
    origin_voucher_no text REFERENCES voucher(no),
    status      text NOT NULL DEFAULT 'in_use' CHECK (status IN ('in_use','disposed')),
    disposed_on date,
    memo        text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT fixed_asset_disposal_is_whole
        CHECK ((status = 'disposed') = (disposed_on IS NOT NULL))
);

COMMENT ON COLUMN fixed_asset.category IS
    '耐用年數的查表鍵，對應 depreciation ruleset 的 useful_lives。年數本身不存。';

-- One period can be charged only once: period is UNIQUE. A duplicate run
-- would add a month of depreciation out of nowhere, an error that would not
-- surface until the asset was fully depreciated.
CREATE TABLE depreciation_run (
    id       serial PRIMARY KEY,
    period   text NOT NULL UNIQUE,
    kind     text NOT NULL CHECK (kind IN ('opening','monthly')),
    run_on   date NOT NULL,
    voucher_no text NOT NULL REFERENCES voucher(no),

    -- Which version of the depreciation rules this run used. An opening entry
    -- need not record one.
    ruleset_version text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT depreciation_run_monthly_records_its_ruleset
        CHECK (kind = 'opening' OR ruleset_version IS NOT NULL)
);

COMMENT ON TABLE depreciation_run IS
    '一次折舊提列。period 唯一，同一個月不可提列兩次。';

CREATE TABLE depreciation_entry (
    id       bigserial PRIMARY KEY,
    run_id   int NOT NULL REFERENCES depreciation_run(id) ON DELETE CASCADE,
    asset_id int NOT NULL REFERENCES fixed_asset(id),
    amount   numeric(18,2) NOT NULL CHECK (amount > 0),
    -- The useful life resolved from the ruleset for this entry. The result is
    -- stored, not the rule.
    useful_life_years int CHECK (useful_life_years > 0),

    UNIQUE (run_id, asset_id)
);

CREATE INDEX depreciation_entry_asset_idx ON depreciation_entry (asset_id);

-- MCP tool grants
--
-- Read-only tools are simply on. Issuing an invoice and charging
-- depreciation both create unposted vouchers that a person still has to
-- post, the same tier as record_receipt (needs_confirm), so they are on
-- too. Retrying an upload only changes the upload status and touches no
-- books, but it resubmits a statutory document, so it still needs a person.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('list_einvoices',        true, 'read_only',     '讀電子發票開立與上傳狀態，含字軌剩餘號碼'),
    ('issue_einvoice',        true, 'needs_confirm', '開立電子發票：稅額依營業稅 ruleset 算，同時產生待過帳的應收傳票'),
    ('retry_einvoice_upload', true, 'needs_confirm', '把上傳失敗的電子發票重新排入上傳佇列'),
    ('get_vat_return',        true, 'read_only',     '試算 401 營業稅申報，銷項稅額與 2211 代收營業稅對帳'),
    ('list_withholding',      true, 'read_only',     '讀各類所得扣繳明細，扣繳率與補充保費依 ruleset 現算'),
    ('compute_withholding',   true, 'read_only',     '試算一筆給付要扣繳多少稅與二代健保補充保費'),
    ('list_fixed_assets',     true, 'read_only',     '讀固定資產：成本、累計折舊、淨額，並與 1601／1602 總帳對帳'),
    ('preview_depreciation',  true, 'read_only',     '試算某個月的折舊，逐筆列出用的耐用年數與規則版本'),
    ('run_depreciation',      true, 'needs_confirm', '提列某個月的折舊，產生待過帳傳票（借 6152 折舊費用／貸 1602 累計折舊）')
ON CONFLICT (tool_name) DO NOTHING;
