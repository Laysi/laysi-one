-- Laysi One: accounts receivable, accounts payable, bank transactions
--
-- These three tables are subsidiary ledgers, not a second set of balances.
--
-- `ar_invoice` and `ap_bill` record who owes or is owed, against which
-- document, and when it falls due. The only balance of record remains the
-- general ledger: total outstanding receivables must equal the posted
-- balance of 1131, and total outstanding payables that of 2101.
--
-- For that identity to hold, the subsidiary ledger must not have its own
-- "amount settled" column for the application to edit, which would be a
-- second set of books. Therefore:
--
--   1. Amounts settled are derived from settlement vouchers (`ar_receipt`
--      and `ap_payment_line` point at vouchers, and only `status = 'posted'`
--      counts). While a voucher is unposted the ledger has not moved, so the
--      subsidiary ledger may not move either.
--   2. If an invoice or bill was created by a voucher (`origin_voucher_no`),
--      it is excluded from the outstanding totals until that voucher is
--      posted. `NULL` is available for opening balances imported without an
--      individual voucher.
--
-- Together, every unit of currency in the subsidiary ledger points back at a
-- posted voucher. `get_ar_aging` and `list_payables` return the ledger
-- balance alongside and compare, surfacing any mismatch on screen.
--
-- Bank reconciliation: `bank_txn` holds transactions imported from the bank,
-- and reconciling links one of them to a voucher. That is not a bookkeeping
-- action (no journal entry) but an assertion that what the bank reported is
-- what the books recorded. A reconciled transaction cannot be reconciled
-- again, and a voucher cannot be claimed by two transactions; SQL enforces
-- both.

-- Receivables detail

CREATE TABLE ar_invoice (
    id                serial PRIMARY KEY,
    invoice_no        text NOT NULL UNIQUE,
    customer          text NOT NULL,
    issued_on         date NOT NULL,
    due_on            date NOT NULL,
    amount            numeric(18,2) NOT NULL CHECK (amount > 0),
    memo              text NOT NULL DEFAULT '',
    -- The voucher that raised this invoice. NULL means it came from an
    -- opening balance import and counts as posted.
    origin_voucher_no text REFERENCES voucher(no),
    status            text NOT NULL DEFAULT 'open' CHECK (status IN ('open','void')),
    created_at        timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT ar_invoice_due_after_issue CHECK (due_on >= issued_on)
);

COMMENT ON TABLE ar_invoice IS
    '應收帳款明細帳。未收餘額由 amount 扣掉已過帳的 ar_receipt 推導，不另存欄位。';
COMMENT ON COLUMN ar_invoice.origin_voucher_no IS
    '開票傳票；未過帳時這筆尚未進總帳，不列入未收合計。NULL = 期初導入。';

CREATE INDEX ar_invoice_due_idx ON ar_invoice (status, due_on);

CREATE TABLE ar_receipt (
    id          bigserial PRIMARY KEY,
    invoice_id  int  NOT NULL REFERENCES ar_invoice(id),
    received_on date NOT NULL,
    amount      numeric(18,2) NOT NULL CHECK (amount > 0),
    -- A receipt always has a voucher. A receipt without one does not count:
    -- the ledger is the only balance of record.
    voucher_no  text NOT NULL REFERENCES voucher(no),
    created_by  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX ar_receipt_invoice_idx ON ar_receipt (invoice_id);

-- Payables detail

CREATE TABLE ap_bill (
    id                serial PRIMARY KEY,
    bill_no           text NOT NULL UNIQUE,
    vendor            text NOT NULL,
    memo              text NOT NULL DEFAULT '',
    billed_on         date NOT NULL,
    due_on            date NOT NULL,
    amount            numeric(18,2) NOT NULL CHECK (amount > 0),
    origin_voucher_no text REFERENCES voucher(no),
    status            text NOT NULL DEFAULT 'open' CHECK (status IN ('open','void')),
    created_at        timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT ap_bill_due_after_billed CHECK (due_on >= billed_on)
);

COMMENT ON TABLE ap_bill IS
    '應付帳款明細帳。未付餘額由 amount 扣掉已過帳的 ap_payment_line 推導。';

CREATE INDEX ap_bill_due_idx ON ap_bill (status, due_on);

-- Payment batch: accounting selects a set of bills and submits them, and the
-- cashier executes the transfer. One batch is one voucher (debit 2101,
-- credit 1102).
CREATE TABLE ap_payment_batch (
    id              serial PRIMARY KEY,
    batch_no        text NOT NULL UNIQUE,
    pay_on          date NOT NULL,
    voucher_no      text NOT NULL REFERENCES voucher(no),
    bank_account_id int  NOT NULL REFERENCES bank_account(id),
    created_by      text NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now()
);

COMMENT ON COLUMN ap_payment_batch.batch_no IS
    '付款單號 P{西元年末兩碼}{月}-{三碼流水}，與傳票同用 doc_sequence 取號。';

CREATE TABLE ap_payment_line (
    id       bigserial PRIMARY KEY,
    batch_id int NOT NULL REFERENCES ap_payment_batch(id) ON DELETE CASCADE,
    bill_id  int NOT NULL REFERENCES ap_bill(id),
    amount   numeric(18,2) NOT NULL CHECK (amount > 0),

    -- A bill may appear only once in a batch, or the total would double
    -- count.
    UNIQUE (batch_id, bill_id)
);

CREATE INDEX ap_payment_line_bill_idx ON ap_payment_line (bill_id);

-- Bank transactions and reconciliation

CREATE TABLE bank_txn (
    id                 serial PRIMARY KEY,
    bank_account_id    int  NOT NULL REFERENCES bank_account(id),
    txn_date           date NOT NULL,
    memo               text NOT NULL,
    -- Positive for money in, negative for money out, the way a bank statement
    -- reads. It is not split into debit and credit columns because this is
    -- what the bank reported, not our journal.
    amount             numeric(18,2) NOT NULL CHECK (amount <> 0),
    kind               text NOT NULL DEFAULT '',
    -- The bank's own identifier, used to deduplicate re-imports.
    external_ref       text UNIQUE,
    matched_voucher_id uuid REFERENCES voucher(id),
    matched_at         timestamptz,
    matched_by         text,
    imported_at        timestamptz NOT NULL DEFAULT now(),

    -- Reconciliation is a group of columns and cannot be half filled.
    CONSTRAINT bank_txn_match_is_whole
        CHECK ((matched_voucher_id IS NULL) = (matched_at IS NULL)
           AND (matched_voucher_id IS NULL) = (matched_by IS NULL)),

    -- A voucher may be claimed by only one transaction. NULLs do not conflict
    -- under a Postgres UNIQUE, so unreconciled rows are unaffected.
    CONSTRAINT bank_txn_one_voucher_once UNIQUE (matched_voucher_id)
);

COMMENT ON TABLE bank_txn IS
    '銀行匯入的流水。核銷是把一筆流水對到一張傳票，不產生分錄。';

CREATE INDEX bank_txn_unmatched_idx ON bank_txn (bank_account_id, txn_date DESC)
    WHERE matched_voucher_id IS NULL;

-- MCP tool grants
-- Read-only tools are simply on. Tools that create a voucher (receipts,
-- reconciliation) need confirmation but stay on: what they create is an
-- unposted voucher that a person still has to post.
-- create_payment_batch is off by default: it is the first step of actually
-- sending money and must not be AI's decision. Accounting staff on screen
-- use the ui channel and are unaffected (see Channel in laysi-ops).

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('get_ar_aging',            true,  'read_only',     '讀應收帳齡，並與 1131 總帳餘額對帳'),
    ('list_receivables',        true,  'read_only',     '讀應收明細：客戶、發票、到期日、未收金額'),
    ('record_receipt',          true,  'needs_confirm', '登記收款，產生待過帳的收款傳票'),
    ('list_payables',           true,  'read_only',     '讀應付明細，並與 2101 總帳餘額對帳'),
    ('create_payment_batch',    false, 'high_risk',     '送出付款單並產生付款傳票。會產生實際付款，預設關閉。'),
    ('list_bank_transactions',  true,  'read_only',     '讀銀行流水與核銷狀態'),
    ('suggest_bank_matches',    true,  'read_only',     '替一筆銀行流水找出可能對得上的傳票'),
    ('match_bank_transaction',  true,  'needs_confirm', '把一筆銀行流水核銷到一張傳票')
ON CONFLICT (tool_name) DO NOTHING;
