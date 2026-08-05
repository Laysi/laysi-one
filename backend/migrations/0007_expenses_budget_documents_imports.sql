-- Laysi One: expense claims, budgets and dimensions, documents, imports
--
-- Two tables here store amounts, and neither is a second set of books:
--
--   budget.amount         the budgeted figure. A plan; the ledger never had
--                         this number in the first place.
--   expense_claim.amount  the claim amount. Before approval it is not a
--                         journal entry, only a document awaiting review.
--
-- No actual is stored: the denominator of budget utilisation is budget and
-- the numerator is always read live from voucher_line. Departmental P&L is
-- the same. Storing an actual offers a second answer, and two answers
-- eventually diverge, the same rule as the financial statements (see 0006
-- and ops/reports.rs).
--
-- There is no OCR engine, so the database cannot even store "recognised".
-- The CHECK on `document_version.ocr_status` accepts two values, `pending`
-- and `not_applicable`; writing `done` is rejected.
--
-- That CHECK is deliberate: connecting an engine will require changing a
-- migration, and whoever does will read this note. Until then no invented
-- percentage can appear on screen looking real.
--
-- File content is not here. `document_version` holds the object key, byte
-- size, MIME type and BLAKE3 only; the content is in RustFS (see storage.rs
-- in laysi-store). The hash is the sole basis for "what came back is what
-- went in", so like the object key it is immutable, enforced by a trigger.

-- Department dimension on journal lines
--
-- 0006 created dimension and dimension_value but never attached them to
-- journal lines, which is why departmental P&L was impossible. This adds
-- voucher_line.dept_id.
--
-- Only the department dimension is attached, not all four: no screen uses
-- projects, cost centres or payment terms yet, and adding four columns now
-- would be guessing four usages. Adding one later follows this same
-- pattern.
--
-- dept_id must point at a value of the department dimension, never at
-- "net 30". A cross-table CHECK cannot express that, but a foreign key can:
-- give dimension_value a unique key on (id, dimension_code), put a
-- generated column fixed to 'department' on voucher_line, and make the two
-- columns the foreign key. When dept_id is NULL the generated column is
-- NULL too and MATCH SIMPLE skips the check, so "this line has no
-- department" stays legal.

ALTER TABLE dimension_value
    ADD CONSTRAINT dimension_value_id_dimension_uk UNIQUE (id, dimension_code);

ALTER TABLE voucher_line
    ADD COLUMN dept_id int,
    ADD COLUMN dept_dimension text
        GENERATED ALWAYS AS (CASE WHEN dept_id IS NULL THEN NULL ELSE 'department' END) STORED;

COMMENT ON COLUMN voucher_line.dept_id IS
    '這一行屬於哪個部門。NULL 代表未指定，部門損益會列成「未指定」而不分攤。';

CREATE INDEX voucher_line_dept_idx ON voucher_line (dept_id);

ALTER TABLE voucher_line
    ADD CONSTRAINT voucher_line_dept_fk
        FOREIGN KEY (dept_id, dept_dimension)
        REFERENCES dimension_value (id, dimension_code);

-- Expense claims and imports are each a voucher origin, and the audit has to
-- tell them apart.
ALTER TABLE voucher DROP CONSTRAINT voucher_source_check;
ALTER TABLE voucher ADD CONSTRAINT voucher_source_check
    CHECK (source IN ('manual','ai_draft','import','payroll','invoice','depreciation','expense'));

-- Documents
--
-- A document has several versions. A revision is a new row, never an
-- overwrite; once uploaded a file is never overwritten and a change is
-- always stored as a new version.

CREATE TABLE document (
    id         serial PRIMARY KEY,
    doc_no     text NOT NULL UNIQUE,
    name       text NOT NULL,
    kind       text NOT NULL CHECK (kind IN (
                   'receipt','vendor_bill','expense_receipt','bank','contract','closing','other')),
    -- Linked record, such as a voucher, a payable bill, a customer or an
    -- accounting period. No foreign key: those four things have no common
    -- primary key.
    link_type  text NOT NULL DEFAULT '' CHECK (link_type IN (
                   '','voucher','ap_bill','ar_invoice','expense_claim','bank_account',
                   'einvoice','import_batch','period','other')),
    link_ref   text NOT NULL DEFAULT '',
    note       text NOT NULL DEFAULT '',
    -- Audit hold: not even the owner can delete it, enforced by the trigger
    -- below.
    archived   boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    created_by text NOT NULL
);

COMMENT ON TABLE document IS
    '文件的身分。內容在 RustFS，版本在 document_version；這張表只回答「這是什麼、綁著哪張單」。';

CREATE INDEX document_link_idx ON document (link_type, link_ref);

CREATE TABLE document_version (
    id           serial PRIMARY KEY,
    document_id  int NOT NULL REFERENCES document(id) ON DELETE CASCADE,
    version_no   int NOT NULL CHECK (version_no > 0),
    -- Key in RustFS. One key means one piece of content and is never
    -- overwritten.
    object_key   text NOT NULL UNIQUE,
    file_name    text NOT NULL,
    byte_size    bigint NOT NULL CHECK (byte_size > 0),
    content_type text NOT NULL,
    -- BLAKE3 hex computed at upload. Recomputed and compared on retrieval;
    -- a mismatch withholds the content.
    checksum     text NOT NULL CHECK (checksum ~ '^[0-9a-f]{64}$'),
    -- There is no OCR engine, so only two values exist here. Writing 'done'
    -- is rejected by the CHECK.
    ocr_status   text NOT NULL DEFAULT 'pending'
                 CHECK (ocr_status IN ('pending','not_applicable')),
    uploaded_at  timestamptz NOT NULL DEFAULT now(),
    uploaded_by  text NOT NULL,
    UNIQUE (document_id, version_no)
);

COMMENT ON COLUMN document_version.checksum IS
    '上傳當時計算的 BLAKE3。取回時重算比對，這是「拿得出當初那一份」的唯一依據。';
COMMENT ON COLUMN document_version.ocr_status IS
    'pending＝還沒辨識（系統沒有 OCR 引擎）；not_applicable＝這種檔不需要辨識。刻意沒有「已辨識」。';

-- A file version is immutable: once written, the object key and hash are the
-- evidence itself, and changing them changes the evidence. To replace the
-- content, upload a new version, the same rule as "a posted voucher can
-- only be corrected by a reversing entry".
CREATE FUNCTION forbid_document_version_mutation() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION '文件版本 %（%）不得刪除。要換內容請上傳新版本。',
                        OLD.version_no, OLD.object_key
            USING ERRCODE = 'check_violation';
    END IF;

    IF (NEW.object_key, NEW.checksum, NEW.byte_size, NEW.document_id, NEW.version_no)
       IS DISTINCT FROM
       (OLD.object_key, OLD.checksum, OLD.byte_size, OLD.document_id, OLD.version_no) THEN
        RAISE EXCEPTION '文件版本 % 已存檔，物件鍵與雜湊不得修改。', OLD.object_key
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER document_version_is_immutable
    BEFORE UPDATE OR DELETE ON document_version
    FOR EACH ROW EXECUTE FUNCTION forbid_document_version_mutation();

-- A held document cannot be deleted. "Not even the owner" has to be decided
-- by the database rather than by omitting a button, because MCP and future
-- batch programs bypass the screen.
CREATE FUNCTION forbid_archived_document_delete() RETURNS trigger AS $$
BEGIN
    IF OLD.archived THEN
        RAISE EXCEPTION '文件 % 已稽核封存，不得刪除。', OLD.doc_no
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER document_archived_is_undeletable
    BEFORE DELETE ON document
    FOR EACH ROW EXECUTE FUNCTION forbid_archived_document_delete();

-- Expense claims
--
-- The amount thresholds are not here and not in Rust; they are in
-- approval_rule from 0006. Which steps a claim must pass is derived by
-- comparing its amount with approval_rule.min_amount, so a different
-- company only changes that table's data.

CREATE TABLE expense_claim (
    id             serial PRIMARY KEY,
    claim_no       text NOT NULL UNIQUE,
    title          text NOT NULL,
    claimant       text NOT NULL,
    claimant_role  text NOT NULL DEFAULT '',
    dept_id        int REFERENCES dimension_value(id),
    spent_on       date NOT NULL,
    amount         numeric(18,2) NOT NULL CHECK (amount > 0),
    -- The account AI suggested from the receipt. A person overriding it is
    -- normal, so both columns are kept: the audit has to show what AI
    -- guessed and what the person decided.
    suggested_account_code text REFERENCES account(code),
    account_code   text REFERENCES account(code),
    -- How the money is paid: reimburse means with the month-end run (credit
    -- 2101); petty_cash means immediately in cash (credit 1101).
    pay_method     text NOT NULL DEFAULT 'reimburse'
                   CHECK (pay_method IN ('reimburse','petty_cash')),
    status         text NOT NULL CHECK (status IN ('submitted','approving','approved','rejected')),
    voucher_no     text REFERENCES voucher(no),
    document_id    int REFERENCES document(id),
    memo           text NOT NULL DEFAULT '',
    created_at     timestamptz NOT NULL DEFAULT now(),
    created_by     text NOT NULL,
    -- The voucher exists only after approval; without one nothing is booked.
    CONSTRAINT expense_claim_approved_has_voucher
        CHECK ((status = 'approved') = (voucher_no IS NOT NULL))
);

COMMENT ON TABLE expense_claim IS
    '費用報支單。要經過哪幾關由 approval_rule 的金額門檻算出來，不寫死在程式裡。';

CREATE TABLE expense_claim_step (
    id          serial PRIMARY KEY,
    claim_id    int NOT NULL REFERENCES expense_claim(id) ON DELETE CASCADE,
    step_no     int NOT NULL REFERENCES approval_rule(step_no),
    decision    text NOT NULL CHECK (decision IN ('approved','rejected')),
    actor       text NOT NULL,
    actor_role  text NOT NULL,
    note        text NOT NULL DEFAULT '',
    decided_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (claim_id, step_no)
);

COMMENT ON TABLE expense_claim_step IS
    '每一關誰簽的、什麼時候簽的。一關只能簽一次（UNIQUE），退回會整張作廢重來。';

-- Budgets
--
-- Budgeted figures are stored and actuals are derived. This table holds
-- only budgets and no actuals column: with one, somebody would eventually
-- UPDATE it and the budget report would part company with the ledger.

CREATE TABLE budget_version (
    code        text PRIMARY KEY,
    label       text NOT NULL,
    fiscal_year int NOT NULL,
    -- Only one current version per year, guaranteed by the partial unique
    -- index below.
    is_current  boolean NOT NULL DEFAULT false,
    note        text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL DEFAULT ''
);

CREATE UNIQUE INDEX budget_version_one_current_per_year
    ON budget_version (fiscal_year) WHERE is_current;

CREATE TABLE budget (
    id           serial PRIMARY KEY,
    version_code text NOT NULL REFERENCES budget_version(code) ON DELETE CASCADE,
    account_code text NOT NULL REFERENCES account(code),
    -- NULL means the company-wide budget for this account, across all
    -- departments.
    dept_id      int REFERENCES dimension_value(id),
    amount       numeric(18,2) NOT NULL CHECK (amount >= 0),
    note         text NOT NULL DEFAULT '',
    updated_at   timestamptz NOT NULL DEFAULT now(),
    updated_by   text NOT NULL DEFAULT '',
    -- NULLS NOT DISTINCT: the company-wide row (dept_id IS NULL) can also
    -- exist only once.
    UNIQUE NULLS NOT DISTINCT (version_code, account_code, dept_id)
);

COMMENT ON TABLE budget IS
    '預算數。實際數不存於此處，也不存於任何地方，一律現查 voucher_line，與三張財務報表同一條原則。';

-- The budget alert threshold is company policy rather than regulation, so
-- the setting key exists but no value is preloaded for any company.
INSERT INTO company_setting (code, label, value, note, editable, sort_order) VALUES
    ('budget_alert_percent', '預算提醒門檻', '',
     '執行率超過這個百分比就在畫面上示警。公司政策，不是法規。', true, 80);

-- Import batches
--
-- An import always starts with a dry run: parse, validate, list the problem
-- with each row, and write only after a person has reviewed it. The preview
-- result is stored in payload so confirming does not require uploading the
-- file again; otherwise "I fixed it and re-uploaded" and "I confirm what I
-- just saw" would be the same button, and they are not the same thing.

CREATE TABLE import_source (
    code       text PRIMARY KEY,
    label      text NOT NULL,
    format     text NOT NULL,
    cadence    text NOT NULL DEFAULT '',
    -- Whether this source works today. false means the screen must show
    -- "not implemented".
    supported  boolean NOT NULL DEFAULT false,
    note       text NOT NULL DEFAULT '',
    sort_order int NOT NULL DEFAULT 0
);

COMMENT ON COLUMN import_source.supported IS
    '是否已實作解析器。目前只有 voucher（歷史傳票 CSV）為 true，其餘在畫面上必須標明尚未接上。';

INSERT INTO import_source (code, label, format, cadence, supported, note, sort_order) VALUES
    ('coa',      '會計科目表',           'Excel', '一次性', false,
     '系統提供基礎科目表，外部科目匯入器尚未實作', 10),
    ('opening',  '期初餘額',             'Excel', '一次性', false,
     '期初餘額匯入器尚未實作',             20),
    ('partner',  '客戶與供應商',         'CSV',   '一次性', false,
     '客戶與供應商還沒有主檔資料表',       30),
    ('voucher',  '歷史傳票',             'CSV',   '隨時',   true,
     '目前唯一已實作解析器的來源：民國年日期會轉換，借貸不平會被擋下', 40),
    ('bank',     '銀行明細',             'OFX',   '每日',   false,
     'OFX 解析器尚未實作', 50),
    ('input_tax','進項發票（財政部下載）', 'XML',  '每期',   false,
     '財政部介面還沒接（與電子發票上傳同一個狀態）', 60);

CREATE TABLE import_batch (
    id          serial PRIMARY KEY,
    batch_no    text NOT NULL UNIQUE,
    source_code text NOT NULL REFERENCES import_source(code),
    file_name   text NOT NULL,
    row_count   int NOT NULL DEFAULT 0,
    ok_count    int NOT NULL DEFAULT 0,
    error_count int NOT NULL DEFAULT 0,
    warn_count  int NOT NULL DEFAULT 0,
    status      text NOT NULL CHECK (status IN ('preview','committed','rolled_back','discarded')),
    -- The voucher drafts parsed by the dry run. Confirming writes exactly
    -- this, without re-parsing.
    payload     jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- The voucher numbers actually written, which rollback relies on.
    voucher_nos text[] NOT NULL DEFAULT '{}',
    document_id int REFERENCES document(id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL,
    settled_at  timestamptz,
    settled_by  text NOT NULL DEFAULT ''
);

COMMENT ON COLUMN import_batch.payload IS
    '預覽的結果。確認匯入寫入的就是這一份，不會重新解析檔案。';

CREATE TABLE import_issue (
    id       serial PRIMARY KEY,
    batch_id int NOT NULL REFERENCES import_batch(id) ON DELETE CASCADE,
    row_no   int NOT NULL,
    -- error blocks the batch; warning only reports (a ROC-era date having
    -- been converted, for instance).
    severity text NOT NULL CHECK (severity IN ('error','warning')),
    issue    text NOT NULL,
    fix      text NOT NULL DEFAULT ''
);

CREATE INDEX import_issue_batch_idx ON import_issue (batch_id, row_no);

-- External integrations
--
-- Whether an integration actually works has to be data rather than static
-- copy, so as with security_policy this makes "is it really connected" a
-- column and leaves the screen no room for ambiguity.

CREATE TABLE integration (
    code       text PRIMARY KEY,
    label      text NOT NULL,
    detail     text NOT NULL DEFAULT '',
    -- Is it actually running? false means the screen must say "not enabled",
    -- never "enabled".
    live       boolean NOT NULL,
    note       text NOT NULL DEFAULT '',
    sort_order int NOT NULL DEFAULT 0
);

INSERT INTO integration (code, label, detail, live, note, sort_order) VALUES
    ('rest',      'REST API',   '/v1/ops/{name}　所有操作同一條路徑', true,
     '與 MCP 共用同一份註冊表', 10),
    ('mcp',       'MCP 伺服器', '/mcp　tools/list 與 tools/call',     true,
     '身分綁定 api_token 資料列，撤銷即時生效；工具開關另由 mcp_tool_grant 管', 20),
    ('objects',   '物件儲存',   'RustFS　文件與附件',                 true,
     '文件管理的檔案確實寫入物件儲存', 30),
    ('webhook',   'Webhook',    '傳票過帳、付款完成',                 false,
     '還沒實作：沒有任何一個地方會送出通知', 40),
    ('einvoice',  '電子發票平台', '財政部 API',                       false,
     '開立與取號已完成，上傳狀態機停在 queued；平台介面尚未接上', 50),
    ('payroll',   '薪資系統',   '每月匯入薪資明細',                   false,
     '人資模組在後續里程碑', 60),
    ('ocr',       'OCR 辨識',   '收據與發票的文字擷取',               false,
     '沒有引擎。文件的 ocr_status 因此只有 pending，不會有假的辨識率', 70),
    ('pos',       '電商 / POS', '訂單與銷貨',                          false,
     '尚未串接', 80);

-- MCP tool grants
--
-- Everything read-only is on. Writes fall into three groups:
--
--   upload_document        on. AI storing a receipt the user provided is
--                          what this system is for, and files are hashed,
--                          audited and undeletable.
--   submit_expense_claim   on. Submitting is only submitting; no money
--                          leaves.
--   approve_expense_claim  off. AI approving its own submission would mean
--                          there is no approval flow.
--   commit_import          off. It writes hundreds of vouchers at once, and
--                          rollback only recovers the unposted ones.
--   rollback_import        on, for the same reason as revoke_api_token: it
--                          moves in the safe direction.
--   set_budget             off. Budgets are the owner's call, and AI must
--                          not change its own denominator.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('list_expense_claims',   true,  'read_only',     '讀費用報支單與它目前關卡'),
    ('submit_expense_claim',  true,  'needs_confirm', '送一張報支單。只是送單，錢不會因此出去。'),
    ('approve_expense_claim', false, 'high_risk',     '簽核報支單，最後一關會產生傳票。對 AI 關閉：送單者自行簽核等同沒有簽核流程。'),
    ('reject_expense_claim',  true,  'needs_confirm', '退回報支單給申請人補資料'),
    ('get_budget_status',     true,  'read_only',     '讀預算執行率。實際數現查總帳，不是存起來的。'),
    ('get_department_pl',     true,  'read_only',     '讀部門損益。沒指定部門的會列成「未指定」，不分攤。'),
    ('set_budget',            false, 'boss_only',     '改預算金額。對 AI 關閉：不應讓 AI 改掉自己的比較基準。'),
    ('list_documents',        true,  'read_only',     '讀文件清單、RustFS 用量與 OCR 現況'),
    ('upload_document',       true,  'needs_confirm', '上傳一份檔案到 RustFS，存下 BLAKE3 雜湊'),
    ('get_document',          true,  'read_only',     '取回檔案內容並比對雜湊。對不上就不交出內容。'),
    ('list_imports',          true,  'read_only',     '讀匯入來源、批次紀錄與對外整合現況'),
    ('preview_import',        true,  'needs_confirm', '匯入預覽：解析、驗證、逐列回報問題。不寫入任何資料。'),
    ('commit_import',         false, 'high_risk',     '寫入一批傳票。對 AI 關閉：回滾只救得回尚未過帳的部分。'),
    ('rollback_import',       true,  'high_risk',     '回滾一批匯入。往安全的方向走，所以對 AI 開著。')
ON CONFLICT (tool_name) DO NOTHING;
