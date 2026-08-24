-- Laysi One initial schema.
--
-- One file. It replaces the 63 migrations that built this schema between the
-- first commit and launch; those files, and the reasoning for each individual
-- change, remain in the git history and are referenced by number from
-- docs/architecture.md.
--
-- Applied by sqlx::migrate!() at start-up. Queries are run through the
-- run-time sqlx::query rather than the macro, so compiling needs no database.

-- Functions used by the triggers below --------------------------------------

CREATE OR REPLACE FUNCTION public.check_voucher_balanced()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
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
$function$
;

CREATE OR REPLACE FUNCTION public.forbid_posted_voucher_mutation()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
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
$function$
;

CREATE OR REPLACE FUNCTION public.forbid_posted_line_mutation()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
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
$function$
;

CREATE OR REPLACE FUNCTION public.forbid_document_version_mutation()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
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
$function$
;

CREATE OR REPLACE FUNCTION public.forbid_archived_document_delete()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF OLD.archived THEN
        RAISE EXCEPTION '文件 % 已稽核封存，不得刪除。', OLD.doc_no
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN OLD;
END;
$function$
;

-- 分類 ------------------------------------------------------------------------
--
-- Only 'department' exists. It is reachable from vouchers, employees, claims
-- and budget lines, and the composite foreign keys pin dept_id to that dimension
-- so it can never point at another one.

CREATE TABLE dimension (
    code text NOT NULL,
    label text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    CONSTRAINT dimension_pkey PRIMARY KEY (code)
);

CREATE TABLE dimension_value (
    id serial,
    dimension_code text NOT NULL,
    code text NOT NULL,
    label text NOT NULL,
    active boolean DEFAULT true NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    CONSTRAINT dimension_value_pkey PRIMARY KEY (id),
    CONSTRAINT dimension_value_dimension_code_code_key UNIQUE (dimension_code, code),
    CONSTRAINT dimension_value_id_dimension_uk UNIQUE (id, dimension_code),
    CONSTRAINT dimension_value_dimension_code_fkey FOREIGN KEY (dimension_code) REFERENCES dimension(code) ON DELETE CASCADE
);

-- 總帳 ------------------------------------------------------------------------
--
-- Vouchers, entries and the chart of accounts. Three invariants live here
-- rather than in the application, because MCP and any future importer can reach
-- the database without passing through it:
--
--   1. A line is non-negative and cannot be both a debit and a credit (CHECK).
--   2. A voucher balances and has at least two lines (a DEFERRABLE constraint
--      trigger, so a half-written voucher inside a transaction is allowed and
--      the check happens at COMMIT).
--   3. A posted voucher and its lines cannot be changed or deleted; the only
--      transition out is posted -> void.
--
-- The consequence for callers: create the voucher as 'pending', write the lines,
-- then update it to 'posted'. Inserting a voucher that is already posted is
-- refused by the trigger.

CREATE TABLE ai_audit_log (
    id bigserial,
    at timestamp with time zone DEFAULT now() NOT NULL,
    actor text NOT NULL,
    actor_role text NOT NULL,
    tool_name text NOT NULL,
    risk text NOT NULL,
    prompt text,
    params jsonb DEFAULT '{}'::jsonb NOT NULL,
    result_summary text DEFAULT ''::text NOT NULL,
    outcome text NOT NULL,
    voucher_no text,
    reverted boolean DEFAULT false NOT NULL,
    channel text DEFAULT 'mcp'::text NOT NULL,
    CONSTRAINT ai_audit_log_pkey PRIMARY KEY (id),
    CONSTRAINT ai_audit_log_channel_check CHECK ((channel = ANY (ARRAY['ui'::text, 'mcp'::text]))),
    CONSTRAINT ai_audit_log_outcome_check CHECK ((outcome = ANY (ARRAY['ok'::text, 'blocked'::text, 'error'::text])))
);
CREATE INDEX ai_audit_log_at_idx ON public.ai_audit_log USING btree (at DESC);
COMMENT ON COLUMN ai_audit_log.channel IS
    'ui=已登入的人在畫面上操作；mcp=AI 透過 MCP 呼叫。只有 mcp 受 mcp_tool_grant 管制。';

CREATE TABLE account (
    code text NOT NULL,
    name text NOT NULL,
    kind text NOT NULL,
    normal_side text NOT NULL,
    is_postable boolean DEFAULT true NOT NULL,
    parent_code text,
    sort_order integer DEFAULT 0 NOT NULL,
    report_group text NOT NULL,
    cash_flow_section text,
    is_active boolean DEFAULT true NOT NULL,
    CONSTRAINT account_pkey PRIMARY KEY (code),
    CONSTRAINT account_parent_code_fkey FOREIGN KEY (parent_code) REFERENCES account(code),
    CONSTRAINT account_cash_flow_section_matches_kind CHECK (((kind = ANY (ARRAY['asset'::text, 'liability'::text, 'equity'::text])) = (cash_flow_section IS NOT NULL))),
    CONSTRAINT account_cash_flow_section_values CHECK (((cash_flow_section IS NULL) OR (cash_flow_section = ANY (ARRAY['cash'::text, 'operating'::text, 'investing'::text, 'financing'::text])))),
    CONSTRAINT account_kind_check CHECK ((kind = ANY (ARRAY['asset'::text, 'liability'::text, 'equity'::text, 'revenue'::text, 'expense'::text]))),
    CONSTRAINT account_normal_side_check CHECK ((normal_side = ANY (ARRAY['debit'::text, 'credit'::text]))),
    CONSTRAINT account_only_assets_can_be_cash CHECK (((cash_flow_section IS DISTINCT FROM 'cash'::text) OR (kind = 'asset'::text))),
    CONSTRAINT account_report_group_matches_kind CHECK ((((kind = 'asset'::text) AND (report_group = ANY (ARRAY['current_asset'::text, 'non_current_asset'::text]))) OR ((kind = 'liability'::text) AND (report_group = ANY (ARRAY['current_liability'::text, 'non_current_liability'::text]))) OR ((kind = 'equity'::text) AND (report_group = 'equity'::text)) OR ((kind = 'revenue'::text) AND (report_group = ANY (ARRAY['revenue'::text, 'other_income'::text]))) OR ((kind = 'expense'::text) AND (report_group = ANY (ARRAY['cogs'::text, 'operating_expense'::text, 'other_expense'::text])))))
);
COMMENT ON TABLE account IS
    '會計科目表。1602 累計折舊屬資產類但正常餘額在貸方，為 1601 的抵減科目。';
COMMENT ON COLUMN account.normal_side IS
    '正常餘額方向。資產費用在借方，負債權益收益在貸方；決定試算表如何呈現餘額。';
COMMENT ON COLUMN account.report_group IS
    '這個科目在資產負債表／損益表的哪一段。分組是設定，金額一律由 voucher_line 推導。';
COMMENT ON COLUMN account.cash_flow_section IS
    '現金流量表的活動別。cash 代表它本身就是現金；損益科目為 NULL（由本期損益代表）。';
COMMENT ON COLUMN account.is_active IS
    'false 代表不可再用於新分錄；既有分錄與報表仍保留，科目不提供刪除。';

CREATE TABLE bank_account (
    id serial,
    name text NOT NULL,
    account_no text NOT NULL,
    gl_code text NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    CONSTRAINT bank_account_pkey PRIMARY KEY (id),
    CONSTRAINT bank_account_gl_code_fkey FOREIGN KEY (gl_code) REFERENCES account(code)
);

CREATE TABLE voucher (
    id uuid NOT NULL,
    no text NOT NULL,
    voucher_date date NOT NULL,
    memo text DEFAULT ''::text NOT NULL,
    status text NOT NULL,
    source text NOT NULL,
    created_by text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    posted_at timestamp with time zone,
    ai_audit_id bigint,
    reverses uuid,
    CONSTRAINT voucher_pkey PRIMARY KEY (id),
    CONSTRAINT voucher_no_key UNIQUE (no),
    CONSTRAINT voucher_reverses_once UNIQUE (reverses),
    CONSTRAINT voucher_ai_audit_fk FOREIGN KEY (ai_audit_id) REFERENCES ai_audit_log(id),
    CONSTRAINT voucher_reverses_fkey FOREIGN KEY (reverses) REFERENCES voucher(id),
    CONSTRAINT voucher_cannot_reverse_itself CHECK (((reverses IS NULL) OR (reverses <> id))),
    CONSTRAINT voucher_source_check CHECK ((source = ANY (ARRAY['manual'::text, 'ai_draft'::text, 'import'::text, 'payroll'::text, 'invoice'::text, 'depreciation'::text, 'expense'::text, 'reversal'::text]))),
    CONSTRAINT voucher_status_check CHECK ((status = ANY (ARRAY['draft'::text, 'pending'::text, 'posted'::text, 'void'::text])))
);
CREATE INDEX voucher_status_date_idx ON public.voucher USING btree (status, voucher_date DESC);
COMMENT ON COLUMN voucher.status IS
    'draft=AI 草擬待人確認；pending=已存待過帳；posted=已過帳；void=已作廢';
COMMENT ON COLUMN voucher.ai_audit_id IS
    '若這張傳票源自 MCP 工具呼叫，指回稽核紀錄。稽核要能雙向追。';
COMMENT ON COLUMN voucher.reverses IS
    '這張傳票沖銷的原傳票；NULL 代表不是沖銷傳票。UNIQUE 保證同一張原傳票只會被沖銷一次。';

CREATE TABLE voucher_line (
    id bigserial,
    voucher_id uuid NOT NULL,
    line_no integer NOT NULL,
    account_code text NOT NULL,
    debit numeric(18,2) DEFAULT 0 NOT NULL,
    credit numeric(18,2) DEFAULT 0 NOT NULL,
    memo text,
    bank_account_id integer,
    dept_id integer,
    dept_dimension text GENERATED ALWAYS AS ( CASE WHEN (dept_id IS NULL) THEN NULL::text ELSE 'department'::text END) STORED,
    CONSTRAINT voucher_line_pkey PRIMARY KEY (id),
    CONSTRAINT voucher_line_voucher_id_line_no_key UNIQUE (voucher_id, line_no),
    CONSTRAINT voucher_line_account_code_fkey FOREIGN KEY (account_code) REFERENCES account(code),
    CONSTRAINT voucher_line_bank_account_id_fkey FOREIGN KEY (bank_account_id) REFERENCES bank_account(id),
    CONSTRAINT voucher_line_dept_fk FOREIGN KEY (dept_id, dept_dimension) REFERENCES dimension_value(id, dimension_code),
    CONSTRAINT voucher_line_voucher_id_fkey FOREIGN KEY (voucher_id) REFERENCES voucher(id) ON DELETE CASCADE,
    CONSTRAINT line_amounts_non_negative CHECK (((debit >= (0)::numeric) AND (credit >= (0)::numeric))),
    CONSTRAINT line_is_not_empty CHECK (((debit > (0)::numeric) OR (credit > (0)::numeric))),
    CONSTRAINT line_is_one_sided CHECK ((NOT ((debit > (0)::numeric) AND (credit > (0)::numeric))))
);
CREATE INDEX voucher_line_account_idx ON public.voucher_line USING btree (account_code);
CREATE INDEX voucher_line_dept_idx ON public.voucher_line USING btree (dept_id);
CREATE INDEX voucher_line_voucher_idx ON public.voucher_line USING btree (voucher_id);
COMMENT ON COLUMN voucher_line.dept_id IS
    '這一行屬於哪個部門。NULL 代表未指定，部門損益會列成「未指定」而不分攤。';

CREATE TABLE doc_sequence (
    prefix text NOT NULL,
    period text NOT NULL,
    next_no integer NOT NULL,
    CONSTRAINT doc_sequence_pkey PRIMARY KEY (prefix, period),
    CONSTRAINT doc_sequence_next_no_check CHECK ((next_no > 0))
);

CREATE TABLE reg_ruleset (
    id serial,
    domain text NOT NULL,
    version text NOT NULL,
    effective_from date NOT NULL,
    effective_to date,
    source_note text NOT NULL,
    payload jsonb NOT NULL,
    checksum text NOT NULL,
    CONSTRAINT reg_ruleset_pkey PRIMARY KEY (id),
    CONSTRAINT reg_ruleset_domain_version_key UNIQUE (domain, version)
);
CREATE INDEX reg_ruleset_lookup_idx ON public.reg_ruleset USING btree (domain, effective_from DESC);

-- 應收、應付與銀行 ------------------------------------------------------------------
--
-- Invoices and bills always carry the VAT split: the 401 return reads the period
-- movement of 2211 and 1284, so an undivided document understates the return by
-- exactly that tax. The rate itself is never in the database or in Rust; it is
-- resolved from the rule library by document date.

CREATE TABLE ar_invoice (
    id serial,
    invoice_no text NOT NULL,
    customer text NOT NULL,
    issued_on date NOT NULL,
    due_on date NOT NULL,
    amount numeric(18,2) NOT NULL,
    memo text DEFAULT ''::text NOT NULL,
    origin_voucher_no text,
    status text DEFAULT 'open'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ar_invoice_pkey PRIMARY KEY (id),
    CONSTRAINT ar_invoice_invoice_no_key UNIQUE (invoice_no),
    CONSTRAINT ar_invoice_origin_voucher_no_fkey FOREIGN KEY (origin_voucher_no) REFERENCES voucher(no),
    CONSTRAINT ar_invoice_amount_check CHECK ((amount > (0)::numeric)),
    CONSTRAINT ar_invoice_due_after_issue CHECK ((due_on >= issued_on)),
    CONSTRAINT ar_invoice_status_check CHECK ((status = ANY (ARRAY['open'::text, 'void'::text])))
);
CREATE INDEX ar_invoice_due_idx ON public.ar_invoice USING btree (status, due_on);
COMMENT ON TABLE ar_invoice IS
    '應收帳款明細帳。未收餘額由 amount 扣掉已過帳的 ar_receipt 推導，不另存欄位。';
COMMENT ON COLUMN ar_invoice.origin_voucher_no IS
    '開票傳票；未過帳時這筆尚未進總帳，不列入未收合計。NULL = 期初導入。';

CREATE TABLE ar_receipt (
    id bigserial,
    invoice_id integer NOT NULL,
    received_on date NOT NULL,
    amount numeric(18,2) NOT NULL,
    voucher_no text NOT NULL,
    created_by text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ar_receipt_pkey PRIMARY KEY (id),
    CONSTRAINT ar_receipt_invoice_id_fkey FOREIGN KEY (invoice_id) REFERENCES ar_invoice(id),
    CONSTRAINT ar_receipt_voucher_no_fkey FOREIGN KEY (voucher_no) REFERENCES voucher(no),
    CONSTRAINT ar_receipt_amount_check CHECK ((amount > (0)::numeric))
);
CREATE INDEX ar_receipt_invoice_idx ON public.ar_receipt USING btree (invoice_id);

CREATE TABLE ap_bill (
    id serial,
    bill_no text NOT NULL,
    vendor text NOT NULL,
    memo text DEFAULT ''::text NOT NULL,
    billed_on date NOT NULL,
    due_on date NOT NULL,
    amount numeric(18,2) NOT NULL,
    origin_voucher_no text,
    status text DEFAULT 'open'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ap_bill_pkey PRIMARY KEY (id),
    CONSTRAINT ap_bill_bill_no_key UNIQUE (bill_no),
    CONSTRAINT ap_bill_origin_voucher_no_fkey FOREIGN KEY (origin_voucher_no) REFERENCES voucher(no),
    CONSTRAINT ap_bill_amount_check CHECK ((amount > (0)::numeric)),
    CONSTRAINT ap_bill_due_after_billed CHECK ((due_on >= billed_on)),
    CONSTRAINT ap_bill_status_check CHECK ((status = ANY (ARRAY['open'::text, 'void'::text])))
);
CREATE INDEX ap_bill_due_idx ON public.ap_bill USING btree (status, due_on);
COMMENT ON TABLE ap_bill IS
    '應付帳款明細帳。未付餘額由 amount 扣掉已過帳的 ap_payment_line 推導。';

CREATE TABLE ap_payment_batch (
    id serial,
    batch_no text NOT NULL,
    pay_on date NOT NULL,
    voucher_no text NOT NULL,
    bank_account_id integer NOT NULL,
    created_by text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ap_payment_batch_pkey PRIMARY KEY (id),
    CONSTRAINT ap_payment_batch_batch_no_key UNIQUE (batch_no),
    CONSTRAINT ap_payment_batch_bank_account_id_fkey FOREIGN KEY (bank_account_id) REFERENCES bank_account(id),
    CONSTRAINT ap_payment_batch_voucher_no_fkey FOREIGN KEY (voucher_no) REFERENCES voucher(no)
);
COMMENT ON COLUMN ap_payment_batch.batch_no IS
    '付款單號 P{西元年末兩碼}{月}-{三碼流水}，與傳票同用 doc_sequence 取號。';

CREATE TABLE ap_payment_line (
    id bigserial,
    batch_id integer NOT NULL,
    bill_id integer NOT NULL,
    amount numeric(18,2) NOT NULL,
    CONSTRAINT ap_payment_line_pkey PRIMARY KEY (id),
    CONSTRAINT ap_payment_line_batch_id_bill_id_key UNIQUE (batch_id, bill_id),
    CONSTRAINT ap_payment_line_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES ap_payment_batch(id) ON DELETE CASCADE,
    CONSTRAINT ap_payment_line_bill_id_fkey FOREIGN KEY (bill_id) REFERENCES ap_bill(id),
    CONSTRAINT ap_payment_line_amount_check CHECK ((amount > (0)::numeric))
);
CREATE INDEX ap_payment_line_bill_idx ON public.ap_payment_line USING btree (bill_id);

CREATE TABLE bank_txn (
    id serial,
    bank_account_id integer NOT NULL,
    txn_date date NOT NULL,
    memo text NOT NULL,
    amount numeric(18,2) NOT NULL,
    kind text DEFAULT ''::text NOT NULL,
    external_ref text,
    matched_voucher_id uuid,
    matched_at timestamp with time zone,
    matched_by text,
    imported_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT bank_txn_pkey PRIMARY KEY (id),
    CONSTRAINT bank_txn_external_ref_key UNIQUE (external_ref),
    CONSTRAINT bank_txn_one_voucher_once UNIQUE (matched_voucher_id),
    CONSTRAINT bank_txn_bank_account_id_fkey FOREIGN KEY (bank_account_id) REFERENCES bank_account(id),
    CONSTRAINT bank_txn_matched_voucher_id_fkey FOREIGN KEY (matched_voucher_id) REFERENCES voucher(id),
    CONSTRAINT bank_txn_amount_check CHECK ((amount <> (0)::numeric)),
    CONSTRAINT bank_txn_match_is_whole CHECK ((((matched_voucher_id IS NULL) = (matched_at IS NULL)) AND ((matched_voucher_id IS NULL) = (matched_by IS NULL))))
);
CREATE INDEX bank_txn_unmatched_idx ON public.bank_txn USING btree (bank_account_id, txn_date DESC) WHERE (matched_voucher_id IS NULL);
COMMENT ON TABLE bank_txn IS
    '銀行匯入的流水。核銷是把一筆流水對到一張傳票，不產生分錄。';

CREATE TABLE withholding_record (
    id serial,
    paid_on date NOT NULL,
    payee text NOT NULL,
    payee_id text DEFAULT ''::text NOT NULL,
    income_code text NOT NULL,
    gross_amount numeric(18,2) NOT NULL,
    basis text NOT NULL,
    withheld_amount numeric(18,2),
    supplementary_base boolean DEFAULT false NOT NULL,
    memo text DEFAULT ''::text NOT NULL,
    voucher_no text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT withholding_record_pkey PRIMARY KEY (id),
    CONSTRAINT withholding_record_voucher_no_fkey FOREIGN KEY (voucher_no) REFERENCES voucher(no),
    CONSTRAINT withholding_record_basis_check CHECK ((basis = ANY (ARRAY['rate'::text, 'table'::text]))),
    CONSTRAINT withholding_record_gross_amount_check CHECK ((gross_amount > (0)::numeric)),
    CONSTRAINT withholding_record_withheld_amount_check CHECK ((withheld_amount >= (0)::numeric)),
    CONSTRAINT withholding_table_basis_carries_its_own_tax CHECK (((basis = 'rate'::text) OR (withheld_amount IS NOT NULL)))
);
CREATE INDEX withholding_paid_idx ON public.withholding_record USING btree (paid_on DESC);
COMMENT ON TABLE withholding_record IS
    '各類所得給付紀錄。應扣繳稅款由 withholding ruleset 依給付日現算，不存欄位。';

-- 固定資產 ----------------------------------------------------------------------
--
-- Every asset points at the voucher that acquired it, so the register and the
-- ledger can be reconciled. Depreciation credits the accumulated account the
-- asset actually uses, one line per account.

CREATE TABLE fixed_asset (
    id serial,
    code text NOT NULL,
    name text NOT NULL,
    category text NOT NULL,
    acquired_on date NOT NULL,
    cost numeric(18,2) NOT NULL,
    method text DEFAULT 'straight_line'::text NOT NULL,
    gl_code text NOT NULL,
    accum_gl_code text NOT NULL,
    origin_voucher_no text,
    status text DEFAULT 'in_use'::text NOT NULL,
    disposed_on date,
    memo text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT fixed_asset_pkey PRIMARY KEY (id),
    CONSTRAINT fixed_asset_code_key UNIQUE (code),
    CONSTRAINT fixed_asset_accum_gl_code_fkey FOREIGN KEY (accum_gl_code) REFERENCES account(code),
    CONSTRAINT fixed_asset_gl_code_fkey FOREIGN KEY (gl_code) REFERENCES account(code),
    CONSTRAINT fixed_asset_origin_voucher_no_fkey FOREIGN KEY (origin_voucher_no) REFERENCES voucher(no),
    CONSTRAINT fixed_asset_cost_check CHECK ((cost > (0)::numeric)),
    CONSTRAINT fixed_asset_disposal_is_whole CHECK (((status = 'disposed'::text) = (disposed_on IS NOT NULL))),
    CONSTRAINT fixed_asset_status_check CHECK ((status = ANY (ARRAY['in_use'::text, 'disposed'::text])))
);
COMMENT ON COLUMN fixed_asset.category IS
    '耐用年數的查表鍵，對應 depreciation ruleset 的 useful_lives。年數本身不存。';

CREATE TABLE depreciation_run (
    id serial,
    period text NOT NULL,
    kind text NOT NULL,
    run_on date NOT NULL,
    voucher_no text NOT NULL,
    ruleset_version text,
    created_by text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT depreciation_run_pkey PRIMARY KEY (id),
    CONSTRAINT depreciation_run_period_key UNIQUE (period),
    CONSTRAINT depreciation_run_voucher_no_fkey FOREIGN KEY (voucher_no) REFERENCES voucher(no),
    CONSTRAINT depreciation_run_kind_check CHECK ((kind = ANY (ARRAY['opening'::text, 'monthly'::text]))),
    CONSTRAINT depreciation_run_monthly_records_its_ruleset CHECK (((kind = 'opening'::text) OR (ruleset_version IS NOT NULL)))
);
COMMENT ON TABLE depreciation_run IS
    '一次折舊提列。period 唯一，同一個月不可提列兩次。';

CREATE TABLE depreciation_entry (
    id bigserial,
    run_id integer NOT NULL,
    asset_id integer NOT NULL,
    amount numeric(18,2) NOT NULL,
    useful_life_years integer,
    CONSTRAINT depreciation_entry_pkey PRIMARY KEY (id),
    CONSTRAINT depreciation_entry_run_id_asset_id_key UNIQUE (run_id, asset_id),
    CONSTRAINT depreciation_entry_asset_id_fkey FOREIGN KEY (asset_id) REFERENCES fixed_asset(id),
    CONSTRAINT depreciation_entry_run_id_fkey FOREIGN KEY (run_id) REFERENCES depreciation_run(id) ON DELETE CASCADE,
    CONSTRAINT depreciation_entry_amount_check CHECK ((amount > (0)::numeric)),
    CONSTRAINT depreciation_entry_useful_life_years_check CHECK ((useful_life_years > 0))
);
CREATE INDEX depreciation_entry_asset_idx ON public.depreciation_entry USING btree (asset_id);

-- 人資 ------------------------------------------------------------------------
--
-- No rate appears here. Insurance, pension and withholding are resolved from the
-- rule library by date — premiums on the period end, withholding on the payment
-- date, overtime on the day worked — and no computed amount that a change in the
-- law could revise is stored.
--
-- Punches are the record of a month; there is no imported monthly summary. Times
-- are kept to the second, and the local date is derived with the business time
-- zone from company_setting, which is why changing it is refused once any punch
-- exists.

CREATE TABLE employee (
    id serial,
    emp_no text NOT NULL,
    name text NOT NULL,
    dept_id integer,
    dept_dimension text GENERATED ALWAYS AS ( CASE WHEN (dept_id IS NULL) THEN NULL::text ELSE 'department'::text END) STORED,
    title text DEFAULT ''::text NOT NULL,
    employment_type text DEFAULT 'full_time'::text NOT NULL,
    hired_on date NOT NULL,
    left_on date,
    status text NOT NULL,
    base_salary numeric(18,2) NOT NULL,
    labor_insured_salary numeric(18,2) NOT NULL,
    health_insured_salary numeric(18,2) NOT NULL,
    pension_wage numeric(18,2) NOT NULL,
    id_no text DEFAULT ''::text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT employee_pkey PRIMARY KEY (id),
    CONSTRAINT employee_emp_no_key UNIQUE (emp_no),
    CONSTRAINT employee_dept_id_dept_dimension_fkey FOREIGN KEY (dept_id, dept_dimension) REFERENCES dimension_value(id, dimension_code),
    CONSTRAINT employee_base_salary_check CHECK ((base_salary >= (0)::numeric)),
    CONSTRAINT employee_employment_type_check CHECK ((employment_type = ANY (ARRAY['full_time'::text, 'part_time'::text, 'contract'::text]))),
    CONSTRAINT employee_health_insured_salary_check CHECK ((health_insured_salary >= (0)::numeric)),
    CONSTRAINT employee_labor_insured_salary_check CHECK ((labor_insured_salary >= (0)::numeric)),
    CONSTRAINT employee_leaving_has_a_date CHECK (((status <> 'leaving'::text) OR (left_on IS NOT NULL))),
    CONSTRAINT employee_left_after_hired CHECK (((left_on IS NULL) OR (left_on >= hired_on))),
    CONSTRAINT employee_left_has_a_date CHECK (((status <> 'left'::text) OR (left_on IS NOT NULL))),
    CONSTRAINT employee_pension_wage_check CHECK ((pension_wage >= (0)::numeric)),
    CONSTRAINT employee_status_check CHECK ((status = ANY (ARRAY['active'::text, 'leaving'::text, 'left'::text])))
);
CREATE INDEX employee_status_idx ON public.employee USING btree (status, emp_no);
COMMENT ON TABLE employee IS
    '員工主檔。三個投保級距是向主管機關申報的事實；費率與上限來自 backend/regulation/，不在這裡。';
COMMENT ON COLUMN employee.base_salary IS
    '月底薪。0 表示尚未設定，薪資作業會擋下這樣的員工而不是發出 0 元薪資單。';
COMMENT ON COLUMN employee.id_no IS
    '身分證字號，勞保申報與扣繳憑單都要用到。寫入時驗檢查碼；畫面預設遮罩顯示，稽核紀錄不留此欄的值。';

CREATE TABLE employee_bank_account (
    id serial,
    employee_id integer NOT NULL,
    bank_name text DEFAULT ''::text NOT NULL,
    account_no text NOT NULL,
    is_payroll boolean DEFAULT false NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT employee_bank_account_pkey PRIMARY KEY (id),
    CONSTRAINT employee_bank_account_employee_id_bank_name_account_no_key UNIQUE (employee_id, bank_name, account_no),
    CONSTRAINT employee_bank_account_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE,
    CONSTRAINT employee_bank_account_account_no_check CHECK ((btrim(account_no) <> ''::text))
);
CREATE UNIQUE INDEX employee_payroll_account_idx ON public.employee_bank_account USING btree (employee_id) WHERE is_payroll;
COMMENT ON TABLE employee_bank_account IS
    '員工的銀行帳戶，一人可有多筆；標為薪轉帳戶的至多一筆。';

CREATE TABLE attendance_punch (
    id serial,
    employee_id integer NOT NULL,
    punched_on date NOT NULL,
    punched_at timestamp with time zone NOT NULL,
    kind text NOT NULL,
    source text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    CONSTRAINT attendance_punch_pkey PRIMARY KEY (id),
    CONSTRAINT attendance_punch_employee_id_punched_on_kind_key UNIQUE (employee_id, punched_on, kind),
    CONSTRAINT attendance_punch_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE,
    CONSTRAINT attendance_punch_day_matches_time CHECK ((punched_on = ((punched_at AT TIME ZONE '08:00:00'::interval))::date)),
    CONSTRAINT attendance_punch_kind_check CHECK ((kind = ANY (ARRAY['in'::text, 'out'::text]))),
    CONSTRAINT attendance_punch_source_check CHECK ((source = ANY (ARRAY['self'::text, 'manual'::text])))
);
CREATE INDEX attendance_punch_by_day ON public.attendance_punch USING btree (punched_on, employee_id);
COMMENT ON TABLE attendance_punch IS
    '打卡原始紀錄。一天一組上下班，時數與遲到由此推導，不覆蓋 attendance 匯入的月統計。';

CREATE TABLE punch_correction (
    id serial,
    employee_id integer NOT NULL,
    punched_on date NOT NULL,
    kind text NOT NULL,
    punched_at timestamp with time zone NOT NULL,
    reason text DEFAULT ''::text NOT NULL,
    status text NOT NULL,
    decided_by text DEFAULT ''::text NOT NULL,
    decided_role text DEFAULT ''::text NOT NULL,
    decided_at timestamp with time zone,
    decision_note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    CONSTRAINT punch_correction_pkey PRIMARY KEY (id),
    CONSTRAINT punch_correction_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE,
    CONSTRAINT punch_correction_day_matches_time CHECK ((punched_on = ((punched_at AT TIME ZONE '08:00:00'::interval))::date)),
    CONSTRAINT punch_correction_decided_iff_not_submitted CHECK (((status = 'submitted'::text) = (decided_at IS NULL))),
    CONSTRAINT punch_correction_kind_check CHECK ((kind = ANY (ARRAY['in'::text, 'out'::text]))),
    CONSTRAINT punch_correction_status_check CHECK ((status = ANY (ARRAY['submitted'::text, 'approved'::text, 'rejected'::text])))
);
CREATE UNIQUE INDEX punch_correction_one_open_per_slot ON public.punch_correction USING btree (employee_id, punched_on, kind) WHERE (status = 'submitted'::text);
CREATE INDEX punch_correction_open ON public.punch_correction USING btree (status, punched_on);
COMMENT ON TABLE punch_correction IS
    '員工送出的打卡補登申請。核准後才寫進 attendance_punch，來源記為補登。';

CREATE TABLE leave_request (
    id serial,
    leave_no text NOT NULL,
    employee_id integer NOT NULL,
    kind text NOT NULL,
    days numeric(6,2) NOT NULL,
    reason text DEFAULT ''::text NOT NULL,
    status text NOT NULL,
    decided_by text DEFAULT ''::text NOT NULL,
    decided_role text DEFAULT ''::text NOT NULL,
    decided_at timestamp with time zone,
    decision_note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    from_at timestamp without time zone NOT NULL,
    to_at timestamp without time zone NOT NULL,
    hours numeric(6,2) NOT NULL,
    from_on date GENERATED ALWAYS AS ((from_at)::date) STORED,
    to_on date GENERATED ALWAYS AS ((to_at)::date) STORED,
    CONSTRAINT leave_request_pkey PRIMARY KEY (id),
    CONSTRAINT leave_request_leave_no_key UNIQUE (leave_no),
    CONSTRAINT leave_request_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE,
    CONSTRAINT leave_request_days CHECK ((days >= (0)::numeric)),
    CONSTRAINT leave_request_decided_iff_not_pending CHECK (((status = 'pending'::text) = (decided_at IS NULL))),
    CONSTRAINT leave_request_hours CHECK ((hours > (0)::numeric)),
    CONSTRAINT leave_request_kind_check CHECK ((kind = ANY (ARRAY['annual'::text, 'sick'::text, 'personal'::text, 'official'::text, 'marriage'::text, 'bereavement'::text, 'maternity'::text, 'menstrual'::text]))),
    CONSTRAINT leave_request_span CHECK ((to_at > from_at)),
    CONSTRAINT leave_request_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'approved'::text, 'rejected'::text])))
);
CREATE INDEX leave_request_emp_idx ON public.leave_request USING btree (employee_id, from_on DESC);
COMMENT ON COLUMN leave_request.from_at IS
    '休假開始的當地時刻，牆上時鐘的時間。from_on 由它推導。';
COMMENT ON COLUMN leave_request.hours IS
    '這段時間落在上班時間內的工作時數，送出當下依上下班時間計算並定案。';

CREATE TABLE overtime_request (
    id serial,
    request_no text NOT NULL,
    employee_id integer NOT NULL,
    day_type text NOT NULL,
    from_at timestamp without time zone NOT NULL,
    to_at timestamp without time zone NOT NULL,
    hours numeric(6,2) NOT NULL,
    reason text DEFAULT ''::text NOT NULL,
    status text NOT NULL,
    decided_by text DEFAULT ''::text NOT NULL,
    decided_role text DEFAULT ''::text NOT NULL,
    decided_at timestamp with time zone,
    decision_note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    worked_on date GENERATED ALWAYS AS ((from_at)::date) STORED,
    CONSTRAINT overtime_request_pkey PRIMARY KEY (id),
    CONSTRAINT overtime_request_request_no_key UNIQUE (request_no),
    CONSTRAINT overtime_request_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE,
    CONSTRAINT overtime_request_decided_iff_not_pending CHECK (((status = 'pending'::text) = (decided_at IS NULL))),
    CONSTRAINT overtime_request_hours_check CHECK ((hours > (0)::numeric)),
    CONSTRAINT overtime_request_span CHECK ((to_at > from_at)),
    CONSTRAINT overtime_request_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'approved'::text, 'rejected'::text])))
);
CREATE INDEX overtime_request_emp_idx ON public.overtime_request USING btree (employee_id, worked_on DESC);
COMMENT ON TABLE overtime_request IS
    '加班申請。核准是加班的前提，核准後由給付當期的薪資作業依規則庫算加班費。';
COMMENT ON COLUMN overtime_request.day_type IS
    '加班日的種類，值取自 overtime 規則庫（平日、休息日）。刻意不設 CHECK：規則有生效日，CHECK 沒有。';

CREATE TABLE payroll_run (
    id serial,
    period text NOT NULL,
    pay_on date NOT NULL,
    headcount integer NOT NULL,
    gross_total numeric(18,2) NOT NULL,
    deduction_total numeric(18,2) NOT NULL,
    net_total numeric(18,2) NOT NULL,
    employer_total numeric(18,2) DEFAULT 0 NOT NULL,
    voucher_no text,
    labor_version text NOT NULL,
    health_version text NOT NULL,
    pension_version text NOT NULL,
    withholding_version text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    CONSTRAINT payroll_run_pkey PRIMARY KEY (id),
    CONSTRAINT payroll_run_voucher_no_fkey FOREIGN KEY (voucher_no) REFERENCES voucher(no),
    CONSTRAINT payroll_run_balances CHECK ((gross_total = (deduction_total + net_total))),
    CONSTRAINT payroll_run_deduction_total_check CHECK ((deduction_total >= (0)::numeric)),
    CONSTRAINT payroll_run_employer_total_check CHECK ((employer_total >= (0)::numeric)),
    CONSTRAINT payroll_run_gross_total_check CHECK ((gross_total >= (0)::numeric)),
    CONSTRAINT payroll_run_headcount_check CHECK ((headcount > 0)),
    CONSTRAINT payroll_run_net_total_check CHECK ((net_total >= (0)::numeric)),
    CONSTRAINT payroll_run_period_check CHECK ((period ~ '^\d{4}-\d{2}$'::text))
);
CREATE INDEX payroll_run_period_idx ON public.payroll_run USING btree (period);
COMMENT ON TABLE payroll_run IS
    '一期一筆的薪資作業。四個 *_version 記錄本期依哪一版法規計算，是回溯修法時唯一查得出答案的地方。';
COMMENT ON COLUMN payroll_run.period IS
    '薪資期別。同一期可以有多次計算：先算過的人不會再算一次，後來到職或當時漏算的人各自成為一次補算。';

CREATE TABLE payslip (
    id serial,
    run_id integer NOT NULL,
    employee_id integer NOT NULL,
    base_salary numeric(18,2) NOT NULL,
    overtime numeric(18,2) DEFAULT 0 NOT NULL,
    allowance numeric(18,2) DEFAULT 0 NOT NULL,
    gross numeric(18,2) NOT NULL,
    labor_insured_salary numeric(18,2) NOT NULL,
    health_insured_salary numeric(18,2) NOT NULL,
    pension_wage numeric(18,2) NOT NULL,
    labor_employee numeric(18,2) NOT NULL,
    health_employee numeric(18,2) NOT NULL,
    income_tax numeric(18,2) NOT NULL,
    labor_employer numeric(18,2) NOT NULL,
    health_employer numeric(18,2) NOT NULL,
    pension_employer numeric(18,2) NOT NULL,
    deduction numeric(18,2) NOT NULL,
    net numeric(18,2) NOT NULL,
    paid_days numeric(5,1) NOT NULL,
    month_days numeric(5,1) NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    CONSTRAINT payslip_pkey PRIMARY KEY (id),
    CONSTRAINT payslip_run_id_employee_id_key UNIQUE (run_id, employee_id),
    CONSTRAINT payslip_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id),
    CONSTRAINT payslip_run_id_fkey FOREIGN KEY (run_id) REFERENCES payroll_run(id) ON DELETE CASCADE,
    CONSTRAINT payslip_allowance_check CHECK ((allowance >= (0)::numeric)),
    CONSTRAINT payslip_base_salary_check CHECK ((base_salary >= (0)::numeric)),
    CONSTRAINT payslip_deduction_check CHECK ((deduction >= (0)::numeric)),
    CONSTRAINT payslip_deduction_is_the_sum CHECK ((deduction = ((labor_employee + health_employee) + income_tax))),
    CONSTRAINT payslip_gross_check CHECK ((gross >= (0)::numeric)),
    CONSTRAINT payslip_gross_is_the_sum CHECK ((gross = ((base_salary + overtime) + allowance))),
    CONSTRAINT payslip_health_employee_check CHECK ((health_employee >= (0)::numeric)),
    CONSTRAINT payslip_health_employer_check CHECK ((health_employer >= (0)::numeric)),
    CONSTRAINT payslip_income_tax_check CHECK ((income_tax >= (0)::numeric)),
    CONSTRAINT payslip_labor_employee_check CHECK ((labor_employee >= (0)::numeric)),
    CONSTRAINT payslip_labor_employer_check CHECK ((labor_employer >= (0)::numeric)),
    CONSTRAINT payslip_month_days_check CHECK ((month_days > (0)::numeric)),
    CONSTRAINT payslip_net_check CHECK ((net >= (0)::numeric)),
    CONSTRAINT payslip_net_is_what_is_left CHECK ((net = (gross - deduction))),
    CONSTRAINT payslip_overtime_check CHECK ((overtime >= (0)::numeric)),
    CONSTRAINT payslip_paid_days_check CHECK ((paid_days > (0)::numeric)),
    CONSTRAINT payslip_pension_employer_check CHECK ((pension_employer >= (0)::numeric))
);
COMMENT ON TABLE payslip IS
    '逐人薪資明細。三條 CHECK 讓「應發＝各項加總」「代扣＝三項加總」「實發＝應發－代扣」在資料庫層面就不可能被寫壞。';

-- 設定、角色與權限 ------------------------------------------------------------------
--
-- Permissions are deny-by-default and checked on every call; nothing is cached.
-- Two risk floors sit outside this matrix and cannot be granted through it:
-- owner-only (payroll, budgets, approval thresholds) and administrator-only
-- (permissions, tokens, company settings, logins). Whoever installs the system
-- therefore does not thereby read everyone's salary.
--
-- mcp_tool_grant is a different question from role_permission: it governs what
-- AI may do, not what people may do.

CREATE TABLE company_setting (
    code text NOT NULL,
    label text NOT NULL,
    value text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    editable boolean DEFAULT true NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT company_setting_pkey PRIMARY KEY (code)
);
COMMENT ON TABLE company_setting IS
    '公司與帳務設定。法規決定的數值（稅率、級距）不得進這張表，那些只住在 backend/regulation/。';

CREATE TABLE role (
    code text NOT NULL,
    name text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    CONSTRAINT role_pkey PRIMARY KEY (code)
);
COMMENT ON TABLE role IS
    'code 與 ai_audit_log.actor_role、OpCtx.actor_role 是同一組值，稽核中的身分要對得回這張表。';

CREATE TABLE permission (
    code text NOT NULL,
    label text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    op_names text[] DEFAULT '{}'::text[] NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    CONSTRAINT permission_pkey PRIMARY KEY (code)
);
COMMENT ON COLUMN permission.op_names IS
    '對應 Registry 的操作名稱；dispatch 每次呼叫都即時查詢，名稱不在角色集合中就拒絕。';

CREATE TABLE role_permission (
    role_code text NOT NULL,
    permission_code text NOT NULL,
    level text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT role_permission_pkey PRIMARY KEY (role_code, permission_code),
    CONSTRAINT role_permission_permission_code_fkey FOREIGN KEY (permission_code) REFERENCES permission(code) ON DELETE CASCADE,
    CONSTRAINT role_permission_role_code_fkey FOREIGN KEY (role_code) REFERENCES role(code) ON DELETE CASCADE,
    CONSTRAINT role_permission_level_check CHECK ((level = ANY (ARRAY['full'::text, 'limited'::text])))
);

CREATE TABLE approval_rule (
    step_no integer NOT NULL,
    role_code text,
    label text NOT NULL,
    condition_note text DEFAULT ''::text NOT NULL,
    min_amount numeric(18,2),
    CONSTRAINT approval_rule_pkey PRIMARY KEY (step_no),
    CONSTRAINT approval_rule_role_code_fkey FOREIGN KEY (role_code) REFERENCES role(code)
);

CREATE TABLE mcp_tool_grant (
    tool_name text NOT NULL,
    enabled boolean NOT NULL,
    risk text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT mcp_tool_grant_pkey PRIMARY KEY (tool_name),
    CONSTRAINT mcp_tool_grant_risk_check CHECK ((risk = ANY (ARRAY['read_only'::text, 'needs_confirm'::text, 'high_risk'::text, 'boss_only'::text, 'admin_only'::text])))
);

CREATE TABLE oidc_setting (
    id integer NOT NULL,
    enabled boolean DEFAULT false NOT NULL,
    issuer text DEFAULT ''::text NOT NULL,
    client_id text DEFAULT ''::text NOT NULL,
    client_secret text DEFAULT ''::text NOT NULL,
    redirect_url text DEFAULT ''::text NOT NULL,
    allowed_domain text DEFAULT ''::text NOT NULL,
    provision_role text DEFAULT ''::text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_by text DEFAULT ''::text NOT NULL,
    verified_at timestamp with time zone,
    verify_error text DEFAULT ''::text NOT NULL,
    password_login_disabled boolean DEFAULT false NOT NULL,
    CONSTRAINT oidc_setting_pkey PRIMARY KEY (id),
    CONSTRAINT oidc_setting_id_check CHECK ((id = 1))
);
COMMENT ON TABLE oidc_setting IS
    '外部登入設定；單列表。client_secret 不經任何操作回傳，稽核紀錄依欄位名遮蔽。';
COMMENT ON COLUMN oidc_setting.allowed_domain IS
    'id_token 的 hd claim 必須相符，空字串代表不限網域。';
COMMENT ON COLUMN oidc_setting.provision_role IS
    '網域內首次登入自動建立帳號時使用的角色代碼；空字串代表不自動建立。';
COMMENT ON COLUMN oidc_setting.password_login_disabled IS
    '啟用時非系統管理員帳號只能用外部登入；系統管理員例外，避免外部登入設定壞掉時整個系統進不去。';

CREATE TABLE install_bootstrap (
    id integer DEFAULT 1 NOT NULL,
    at timestamp with time zone DEFAULT now() NOT NULL,
    username text NOT NULL,
    user_agent text DEFAULT ''::text NOT NULL,
    token_required boolean NOT NULL,
    CONSTRAINT install_bootstrap_pkey PRIMARY KEY (id),
    CONSTRAINT install_bootstrap_id_check CHECK ((id = 1))
);
COMMENT ON TABLE install_bootstrap IS
    '網頁初始化只能成功一次；單列主鍵是資料庫層的保證，不只靠應用層檢查。';
COMMENT ON COLUMN install_bootstrap.token_required IS
    '記錄當時是否要求 SETUP_TOKEN；沒有這一欄就無法事後判斷這台是怎麼被認領的。';

-- 登入與憑證 ---------------------------------------------------------------------
--
-- Neither a password nor a token is stored in the clear. api_token keeps a BLAKE3
-- hash and the first eight characters, which is enough for a person to recognise
-- a row and for the system to compare, and not enough for anyone who copies the
-- database. A token is visible once, at issue.
--
-- The login name is the email address; 'admin' is the one exception, and its
-- empty email is what makes it so: an address would let an external identity
-- resolve onto the account that has to stay reachable by password alone.

CREATE TABLE user_account (
    id serial,
    username text NOT NULL,
    display_name text NOT NULL,
    email text DEFAULT ''::text NOT NULL,
    password_hash text,
    role_code text NOT NULL,
    employee_id integer,
    status text NOT NULL,
    must_change_password boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text DEFAULT ''::text NOT NULL,
    last_login_at timestamp with time zone,
    CONSTRAINT user_account_pkey PRIMARY KEY (id),
    CONSTRAINT user_account_username_key UNIQUE (username),
    CONSTRAINT user_account_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id),
    CONSTRAINT user_account_role_code_fkey FOREIGN KEY (role_code) REFERENCES role(code),
    CONSTRAINT user_account_status_check CHECK ((status = ANY (ARRAY['active'::text, 'suspended'::text])))
);
CREATE INDEX user_account_employee_idx ON public.user_account USING btree (employee_id) WHERE (employee_id IS NOT NULL);
CREATE INDEX user_account_role_status_idx ON public.user_account USING btree (role_code, status);
COMMENT ON TABLE user_account IS
    '登入帳號獨立於員工名冊；employee_id 為 NULL 時仍可代表事務所等外部使用者。';
COMMENT ON COLUMN user_account.password_hash IS
    'NULL 代表只能用 OAuth／OIDC 登入；本欄永不保存明文密碼。';

CREATE TABLE user_identity (
    id serial,
    user_id integer NOT NULL,
    provider text NOT NULL,
    subject text NOT NULL,
    email_at_link text DEFAULT ''::text NOT NULL,
    linked_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_identity_pkey PRIMARY KEY (id),
    CONSTRAINT user_identity_provider_subject_key UNIQUE (provider, subject),
    CONSTRAINT user_identity_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(id) ON DELETE CASCADE
);
CREATE INDEX user_identity_user_idx ON public.user_identity USING btree (user_id);
COMMENT ON TABLE user_identity IS
    '外部登入身分以 IdP 的 provider 與穩定 subject 唯一識別；email 只留作連結當下的稽核線索。';

CREATE TABLE user_session (
    id serial,
    token_hash text NOT NULL,
    user_id integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone,
    revoked_by text DEFAULT ''::text NOT NULL,
    user_agent text DEFAULT ''::text NOT NULL,
    ip text DEFAULT ''::text NOT NULL,
    CONSTRAINT user_session_pkey PRIMARY KEY (id),
    CONSTRAINT user_session_token_hash_key UNIQUE (token_hash),
    CONSTRAINT user_session_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(id) ON DELETE CASCADE
);
CREATE INDEX user_session_live_idx ON public.user_session USING btree (expires_at) WHERE (revoked_at IS NULL);
CREATE INDEX user_session_user_idx ON public.user_session USING btree (user_id, created_at DESC);
COMMENT ON TABLE user_session IS
    '登入工作階段只保存 token 雜湊；明文 token 不落地，撤銷後也不能復原。';
COMMENT ON COLUMN user_session.token_hash IS
    '登入 token 的雜湊。伺服器收到明文後先雜湊再比對，資料庫無法還原明文。';

CREATE TABLE api_token (
    id serial,
    name text NOT NULL,
    kind text NOT NULL,
    scope text NOT NULL,
    prefix text NOT NULL,
    token_hash text NOT NULL,
    actor text NOT NULL,
    actor_role text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    last_used_at timestamp with time zone,
    rotate_days integer,
    expiry_note text DEFAULT ''::text NOT NULL,
    revoked_at timestamp with time zone,
    revoked_by text DEFAULT ''::text NOT NULL,
    owner_employee_id integer,
    CONSTRAINT api_token_pkey PRIMARY KEY (id),
    CONSTRAINT api_token_token_hash_key UNIQUE (token_hash),
    CONSTRAINT api_token_actor_role_fkey FOREIGN KEY (actor_role) REFERENCES role(code),
    CONSTRAINT api_token_owner_employee_id_fkey FOREIGN KEY (owner_employee_id) REFERENCES employee(id) ON DELETE SET NULL,
    CONSTRAINT api_token_kind_check CHECK ((kind = ANY (ARRAY['api'::text, 'mcp'::text]))),
    CONSTRAINT api_token_rotate_days_check CHECK (((rotate_days IS NULL) OR (rotate_days > 0)))
);
CREATE INDEX api_token_live_idx ON public.api_token USING btree (revoked_at, id DESC);
CREATE INDEX api_token_owner_idx ON public.api_token USING btree (owner_employee_id, revoked_at);
COMMENT ON COLUMN api_token.token_hash IS
    'BLAKE3 十六進位。明文只在發出來的那一次回傳，系統自己也讀不回來。';
COMMENT ON COLUMN api_token.owner_employee_id IS
    '自助核發時的所屬員工。NULL 表示由負責人代發，不屬於任何個人。';

-- 文件 ------------------------------------------------------------------------
--
-- The bytes live in object storage. The database keeps the object key, the size,
-- the MIME type and a BLAKE3 of what landed; a read recomputes the hash and
-- refuses to hand back content that does not match. Versions cannot be changed
-- and an archived document cannot be deleted.

CREATE TABLE document (
    id serial,
    doc_no text NOT NULL,
    name text NOT NULL,
    kind text NOT NULL,
    link_type text DEFAULT ''::text NOT NULL,
    link_ref text DEFAULT ''::text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    archived boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    CONSTRAINT document_pkey PRIMARY KEY (id),
    CONSTRAINT document_doc_no_key UNIQUE (doc_no),
    CONSTRAINT document_kind_check CHECK ((kind = ANY (ARRAY['receipt'::text, 'vendor_bill'::text, 'expense_receipt'::text, 'bank'::text, 'contract'::text, 'closing'::text, 'other'::text]))),
    CONSTRAINT document_link_type_kinds CHECK ((link_type = ANY (ARRAY[''::text, 'voucher'::text, 'ap_bill'::text, 'ar_invoice'::text, 'expense_claim'::text, 'bank_account'::text, 'import_batch'::text, 'period'::text, 'other'::text])))
);
CREATE INDEX document_link_idx ON public.document USING btree (link_type, link_ref);
COMMENT ON TABLE document IS
    '文件的身分。內容在 RustFS，版本在 document_version；這張表只回答「這是什麼、綁著哪張單」。';

CREATE TABLE document_version (
    id serial,
    document_id integer NOT NULL,
    version_no integer NOT NULL,
    object_key text NOT NULL,
    file_name text NOT NULL,
    byte_size bigint NOT NULL,
    content_type text NOT NULL,
    checksum text NOT NULL,
    ocr_status text DEFAULT 'pending'::text NOT NULL,
    uploaded_at timestamp with time zone DEFAULT now() NOT NULL,
    uploaded_by text NOT NULL,
    CONSTRAINT document_version_pkey PRIMARY KEY (id),
    CONSTRAINT document_version_document_id_version_no_key UNIQUE (document_id, version_no),
    CONSTRAINT document_version_object_key_key UNIQUE (object_key),
    CONSTRAINT document_version_document_id_fkey FOREIGN KEY (document_id) REFERENCES document(id) ON DELETE CASCADE,
    CONSTRAINT document_version_byte_size_check CHECK ((byte_size > 0)),
    CONSTRAINT document_version_checksum_check CHECK ((checksum ~ '^[0-9a-f]{64}$'::text)),
    CONSTRAINT document_version_ocr_status_check CHECK ((ocr_status = ANY (ARRAY['pending'::text, 'not_applicable'::text]))),
    CONSTRAINT document_version_version_no_check CHECK ((version_no > 0))
);
COMMENT ON COLUMN document_version.checksum IS
    '上傳當時計算的 BLAKE3。取回時重算比對，這是「拿得出當初那一份」的唯一依據。';
COMMENT ON COLUMN document_version.ocr_status IS
    'pending＝還沒辨識（系統沒有 OCR 引擎）；not_applicable＝這種檔不需要辨識。刻意沒有「已辨識」。';

-- 費用報支、預算與匯入 ----------------------------------------------------------------
--
-- budget holds only the budgeted figure — the actual is always read from
-- voucher_line, so the two cannot disagree. Approval steps are derived from
-- approval_rule's amount thresholds, so no amount appears in the operations.
--
-- import_source.supported is what routes a file to a parser, so a row exists
-- only once its parser does.

CREATE TABLE expense_claim (
    id serial,
    claim_no text NOT NULL,
    title text NOT NULL,
    claimant text NOT NULL,
    claimant_role text DEFAULT ''::text NOT NULL,
    dept_id integer,
    spent_on date NOT NULL,
    amount numeric(18,2) NOT NULL,
    suggested_account_code text,
    account_code text,
    pay_method text DEFAULT 'reimburse'::text NOT NULL,
    status text NOT NULL,
    voucher_no text,
    document_id integer,
    memo text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    employee_id integer,
    CONSTRAINT expense_claim_pkey PRIMARY KEY (id),
    CONSTRAINT expense_claim_claim_no_key UNIQUE (claim_no),
    CONSTRAINT expense_claim_account_code_fkey FOREIGN KEY (account_code) REFERENCES account(code),
    CONSTRAINT expense_claim_dept_id_fkey FOREIGN KEY (dept_id) REFERENCES dimension_value(id),
    CONSTRAINT expense_claim_document_id_fkey FOREIGN KEY (document_id) REFERENCES document(id),
    CONSTRAINT expense_claim_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE SET NULL,
    CONSTRAINT expense_claim_suggested_account_code_fkey FOREIGN KEY (suggested_account_code) REFERENCES account(code),
    CONSTRAINT expense_claim_voucher_no_fkey FOREIGN KEY (voucher_no) REFERENCES voucher(no),
    CONSTRAINT expense_claim_amount_check CHECK ((amount > (0)::numeric)),
    CONSTRAINT expense_claim_approved_has_voucher CHECK (((status = 'approved'::text) = (voucher_no IS NOT NULL))),
    CONSTRAINT expense_claim_pay_method_check CHECK ((pay_method = ANY (ARRAY['reimburse'::text, 'petty_cash'::text]))),
    CONSTRAINT expense_claim_status_check CHECK ((status = ANY (ARRAY['submitted'::text, 'approving'::text, 'approved'::text, 'rejected'::text])))
);
CREATE INDEX expense_claim_employee_idx ON public.expense_claim USING btree (employee_id, status);
COMMENT ON TABLE expense_claim IS
    '費用報支單。要經過哪幾關由 approval_rule 的金額門檻算出來，不寫死在程式裡。';
COMMENT ON COLUMN expense_claim.employee_id IS
    '自助送出時的申請人。NULL 表示由會計代為建立，申請人只有姓名。';

CREATE TABLE expense_claim_step (
    id serial,
    claim_id integer NOT NULL,
    step_no integer NOT NULL,
    decision text NOT NULL,
    actor text NOT NULL,
    actor_role text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    decided_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT expense_claim_step_pkey PRIMARY KEY (id),
    CONSTRAINT expense_claim_step_claim_id_step_no_key UNIQUE (claim_id, step_no),
    CONSTRAINT expense_claim_step_claim_id_fkey FOREIGN KEY (claim_id) REFERENCES expense_claim(id) ON DELETE CASCADE,
    CONSTRAINT expense_claim_step_decision_check CHECK ((decision = ANY (ARRAY['approved'::text, 'rejected'::text])))
);
COMMENT ON TABLE expense_claim_step IS
    '每一關誰簽的、什麼時候簽的。一關只能簽一次（UNIQUE），退回會整張作廢重來。';
COMMENT ON COLUMN expense_claim_step.step_no IS
    '簽核當下的關卡序號。簽核規則之後可能變更或刪除，這個數字不跟著動。';

CREATE TABLE budget_version (
    code text NOT NULL,
    label text NOT NULL,
    fiscal_year integer NOT NULL,
    is_current boolean DEFAULT false NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT budget_version_pkey PRIMARY KEY (code)
);
CREATE UNIQUE INDEX budget_version_one_current_per_year ON public.budget_version USING btree (fiscal_year) WHERE is_current;

CREATE TABLE budget (
    id serial,
    version_code text NOT NULL,
    account_code text NOT NULL,
    dept_id integer,
    amount numeric(18,2) NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT budget_pkey PRIMARY KEY (id),
    CONSTRAINT budget_version_code_account_code_dept_id_key UNIQUE NULLS NOT DISTINCT (version_code, account_code, dept_id),
    CONSTRAINT budget_account_code_fkey FOREIGN KEY (account_code) REFERENCES account(code),
    CONSTRAINT budget_dept_id_fkey FOREIGN KEY (dept_id) REFERENCES dimension_value(id),
    CONSTRAINT budget_version_code_fkey FOREIGN KEY (version_code) REFERENCES budget_version(code) ON DELETE CASCADE,
    CONSTRAINT budget_amount_check CHECK ((amount >= (0)::numeric))
);
COMMENT ON TABLE budget IS
    '預算數。實際數不存於此處，也不存於任何地方，一律現查 voucher_line，與三張財務報表同一條原則。';

CREATE TABLE import_source (
    code text NOT NULL,
    label text NOT NULL,
    format text NOT NULL,
    cadence text DEFAULT ''::text NOT NULL,
    supported boolean DEFAULT false NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    CONSTRAINT import_source_pkey PRIMARY KEY (code)
);
COMMENT ON COLUMN import_source.supported IS
    '是否已實作解析器。目前只有 voucher（歷史傳票 CSV）為 true，其餘在畫面上必須標明尚未接上。';

CREATE TABLE import_batch (
    id serial,
    batch_no text NOT NULL,
    source_code text NOT NULL,
    file_name text NOT NULL,
    row_count integer DEFAULT 0 NOT NULL,
    ok_count integer DEFAULT 0 NOT NULL,
    error_count integer DEFAULT 0 NOT NULL,
    warn_count integer DEFAULT 0 NOT NULL,
    status text NOT NULL,
    payload jsonb DEFAULT '[]'::jsonb NOT NULL,
    voucher_nos text[] DEFAULT '{}'::text[] NOT NULL,
    document_id integer,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    settled_at timestamp with time zone,
    settled_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT import_batch_pkey PRIMARY KEY (id),
    CONSTRAINT import_batch_batch_no_key UNIQUE (batch_no),
    CONSTRAINT import_batch_document_id_fkey FOREIGN KEY (document_id) REFERENCES document(id),
    CONSTRAINT import_batch_source_code_fkey FOREIGN KEY (source_code) REFERENCES import_source(code),
    CONSTRAINT import_batch_status_check CHECK ((status = ANY (ARRAY['preview'::text, 'committed'::text, 'rolled_back'::text, 'discarded'::text])))
);
COMMENT ON COLUMN import_batch.payload IS
    '預覽的結果。確認匯入寫入的就是這一份，不會重新解析檔案。';

CREATE TABLE import_issue (
    id serial,
    batch_id integer NOT NULL,
    row_no integer NOT NULL,
    severity text NOT NULL,
    issue text NOT NULL,
    fix text DEFAULT ''::text NOT NULL,
    CONSTRAINT import_issue_pkey PRIMARY KEY (id),
    CONSTRAINT import_issue_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES import_batch(id) ON DELETE CASCADE,
    CONSTRAINT import_issue_severity_check CHECK ((severity = ANY (ARRAY['error'::text, 'warning'::text])))
);
CREATE INDEX import_issue_batch_idx ON public.import_issue USING btree (batch_id, row_no);

-- Triggers ------------------------------------------------------------------

CREATE TRIGGER document_archived_is_undeletable BEFORE DELETE ON public.document FOR EACH ROW EXECUTE FUNCTION forbid_archived_document_delete();
CREATE TRIGGER document_version_is_immutable BEFORE DELETE OR UPDATE ON public.document_version FOR EACH ROW EXECUTE FUNCTION forbid_document_version_mutation();
CREATE CONSTRAINT TRIGGER voucher_must_balance AFTER INSERT OR UPDATE ON public.voucher DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_voucher_balanced();
CREATE TRIGGER voucher_posted_is_immutable BEFORE DELETE OR UPDATE ON public.voucher FOR EACH ROW EXECUTE FUNCTION forbid_posted_voucher_mutation();
CREATE CONSTRAINT TRIGGER voucher_line_must_balance AFTER INSERT OR DELETE OR UPDATE ON public.voucher_line DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_voucher_balanced();
CREATE TRIGGER voucher_line_posted_is_immutable BEFORE INSERT OR DELETE OR UPDATE ON public.voucher_line FOR EACH ROW EXECUTE FUNCTION forbid_posted_line_mutation();

-- Seed data -----------------------------------------------------------------
--
-- What a fresh install needs in order to be usable: the chart of accounts,
-- the permission matrix, the AI tool switches and the setting keys. No
-- company data — employees, vouchers, bank accounts and document counters
-- are created by the people who run the install. `laysi-admin seed-demo`
-- adds evaluation data, and only to a throwaway install.

INSERT INTO role (code, name, note, sort_order) VALUES
    ('admin', '系統管理員', '管理帳號、權限與系統設定，不含薪資資料', 5),
    ('boss', '負責人', '公司的最終決定者：薪資、預算與簽核門檻', 10),
    ('acct', '會計', '可開立傳票、不可付款', 20),
    ('cashier', '出納', '執行匯款，不涉及分錄', 30),
    ('manager', '主管', '簽核部門的費用報支', 40),
    ('accountant', '會計師', '事務所端，調整分錄與年度結算', 50),
    ('staff', '一般職員', '只能看本人的薪資、休假與請款', 60);

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('view_reports', '看報表與現金', '總覽、試算表與三張財務報表；收款與銀行核銷另立一組權限', '{get_dashboard,get_trial_balance,get_balance_sheet,get_income_statement,get_cash_flow,list_chart_of_accounts}', 10),
    ('view_masters', '看基礎主檔', '會計科目、分類與公司設定的唯讀入口；科目餘額屬於報表權限', '{list_accounts,get_company_settings,list_dimensions}', 15),
    ('manage_masters', '維護基礎主檔', '新增、修改或停用科目，建立銀行帳戶與分類項目，並設定費用簽核流程', '{create_account,update_account,create_bank_account,create_dimension_value,set_approval_rule}', 16),
    ('create_voucher', '開立傳票', '草擬待確認傳票；直接開立分錄是調整分錄權限', '{draft_voucher}', 20),
    ('post_voucher', '過帳／結帳', '過帳後不可修改；不應存在的傳票可作廢，確實發生但記錯的交易以反向傳票更正', '{post_voucher,void_voucher,reverse_voucher}', 30),
    ('adjust_entry', '調整分錄（會計師）', '查閱既有傳票、直接開立待過帳分錄，並刪除開錯而尚未過帳的傳票', '{search_vouchers,create_voucher,discard_voucher}', 40),
    ('view_subledgers', '看應收應付明細', '查看應收帳齡、應收明細與待付帳單', '{get_ar_aging,list_receivables,list_payables}', 45),
    ('issue_invoice', '開立應收單', '建立應收單據並產生待過帳傳票；營業稅申報是稅務權限', '{create_ar_invoice}', 50),
    ('manage_tax', '稅務與法規試算', '查看適用法規、營業稅申報與各類所得扣繳明細及試算', '{list_regulations,get_vat_return,list_withholding,compute_withholding}', 55),
    ('request_payment', '送出付款申請', '送出費用報支或建立待過帳應付帳單', '{submit_expense_claim,create_ap_bill}', 60),
    ('manage_assets', '固定資產與折舊', '登記資產、查看資產、預覽折舊並產生一期待過帳折舊傳票', '{list_fixed_assets,create_fixed_asset,preview_depreciation,run_depreciation}', 65),
    ('execute_payment', '實際匯款', '將應付帳單組成付款批次；這是職責分離中的實際付款權限', '{create_payment_batch}', 70),
    ('manage_banking', '收款與銀行核銷', '查看現金與銀行流水、登記收款、取得建議並完成人工核銷', '{get_cash_position,record_receipt,list_bank_transactions,suggest_bank_matches,match_bank_transaction}', 75),
    ('approve_expense', '簽核費用報支', '查看簽核規則與報支單，並依目前關卡核准或退回', '{list_expense_claims,approve_expense_claim,reject_expense_claim,list_approval_rules}', 80),
    ('view_department', '看部門預算與損益', '查看預算執行率與部門損益；未指定部門的分錄仍會列出', '{get_budget_status,get_department_pl}', 81),
    ('manage_budget', '設定預算', '設定年度與部門預算；實際數仍一律由已過帳分錄現算', '{set_budget,create_budget_version}', 82),
    ('manage_documents', '管理文件', '查看、上傳與驗證文件版本與內容雜湊', '{list_documents,upload_document,get_document}', 85),
    ('run_imports', '執行匯入', '查看匯入來源、預覽、確認匯入與復原尚未過帳的批次', '{list_imports,preview_import,commit_import,rollback_import}', 86),
    ('view_payroll', '看薪資彙總', '全公司薪資總額與總帳對帳，不含任何個人金額', '{get_payroll_summary}', 90),
    ('view_hr', '看人資資料', '人資總覽、名冊、出勤休假、打卡、投保摘要、保費試算與法遵檢核', '{get_hr_dashboard,list_employees,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches,list_punch_corrections,list_overtime_requests}', 91),
    ('view_payroll_details', '看個人薪資明細', '個人薪資明細與年度扣繳憑單限負責人；全公司彙總會計亦可查看', '{list_payslips,list_withholding_statements}', 92),
    ('manage_payroll', '執行薪資作業', '計算一期薪資並產生待過帳薪資傳票，不含正式過帳', '{run_payroll}', 93),
    ('manage_hr', '人事異動與請假', '建立與維護員工主檔，處理到離職與人事異動流程，對待簽休假單與補登申請作成決定，並補登打卡', '{decide_leave_request,create_employee,update_employee,correct_punch,decide_punch_correction,decide_overtime_request}', 95),
    ('staff_self_service', '使用員工自助', '只回傳目前登入帳號所連結員工本人的首頁、薪資、休假、打卡、補登申請、報支進度與 MCP Token', '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections,submit_my_overtime_request,list_my_overtime_requests}', 96),
    ('view_audit', '看稽核紀錄', '查看每次操作的呼叫者、參數摘要、結果與被拒絕紀錄', '{list_ai_audit}', 97),
    ('view_system', '看安全與系統狀態', '查看權限對照表、安全政策、Token 後設資料與系統健康狀態', '{list_roles,get_security_policy,list_api_tokens,get_system_health}', 98),
    ('manage_access', '改權限與 MCP', '建立與檢視登入帳號、修改公司設定與角色權限，並簽發或撤銷 API／MCP Token', '{update_company_setting,set_company_timezone,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,unlink_user_identity,delete_user_account,list_user_accounts,get_oidc_setting,update_oidc_setting}', 100);

INSERT INTO role_permission (role_code, permission_code, level, note, updated_at, updated_by) VALUES
    ('acct', 'view_reports', 'full', '會計要用總覽、試算表與三張報表完成日常帳務。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'view_masters', 'full', '會計開帳與選科目前必須能查基礎主檔。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'view_subledgers', 'full', '會計要用應收應付明細與總帳對帳。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'create_voucher', 'full', '會計可以建立待確認的傳票草稿。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'adjust_entry', 'limited', '會計可登打日常分錄及事務所提出的調整。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'post_voucher', 'full', '會計負責把已確認傳票正式過帳。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_reports', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'request_payment', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'execute_payment', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'approve_expense', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_payroll', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'manage_hr', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'staff_self_service', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'create_voucher', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'post_voucher', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'adjust_entry', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'issue_invoice', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_masters', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_subledgers', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'manage_banking', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'manage_tax', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'manage_assets', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_department', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'manage_budget', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'manage_documents', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'run_imports', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_hr', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'issue_invoice', 'full', '會計負責電子發票的查看、開立與重送。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'request_payment', 'full', '會計可以送出報支申請，但這不包含實際匯款。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'approve_expense', 'full', '報支流程需要會計確認科目並處理目前關卡。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'manage_banking', 'full', '會計負責收款登記與銀行對帳，但不能建立付款批次。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'manage_tax', 'full', '會計負責營業稅與扣繳作業。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'manage_assets', 'full', '會計負責資產台帳與每期折舊。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'view_department', 'full', '會計需要部門預算與損益來完成管理報表。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'manage_documents', 'full', '會計負責保存與驗證傳票、發票及報支憑證。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'view_hr', 'full', '中小企業的人資資料由會計兼辦，需查看名冊、出勤與投保資料。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'manage_hr', 'full', '會計兼辦到離職、人事異動與請假流程。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'view_payroll', 'full', '會計需要薪資彙總與總帳對帳。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'view_payroll_details', 'full', '會計執行薪資作業時需要核對逐人明細。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'manage_payroll', 'full', '會計負責計算薪資並產生待過帳傳票。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'view_audit', 'full', '會計需要追查帳務操作與被拒絕紀錄。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'staff_self_service', 'full', '會計若有員工主檔，也只能從自助操作查看本人資料。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('cashier', 'view_subledgers', 'limited', '出納只需查看收款與付款所需的應收應付明細。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('cashier', 'request_payment', 'full', '出納可以送出自己的報支申請，尚不會讓錢匯出。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('cashier', 'execute_payment', 'full', '實際建立付款批次是出納的核心職責。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('cashier', 'manage_banking', 'full', '出納負責收款、銀行流水與核銷。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('cashier', 'staff_self_service', 'full', '出納以員工身分只能查看與申請自己的資料。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('manager', 'request_payment', 'full', '主管可以送出自己的報支申請。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('manager', 'approve_expense', 'limited', '主管只在流程輪到主管時簽核部門報支。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('manager', 'view_department', 'limited', '主管只需要部門預算與損益資料。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('manager', 'staff_self_service', 'full', '主管以員工身分只能查看與申請自己的資料。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'view_reports', 'full', '會計師需要完整財務報表完成查核與結算。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'view_subledgers', 'full', '會計師需要應收應付明細核對總帳。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'adjust_entry', 'full', '事務所端可以查傳票並建立年度調整分錄。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'post_voucher', 'full', '年度調整確認後由會計師完成結算過帳。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'manage_tax', 'full', '會計師負責營業稅、扣繳與適用法規查核。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'manage_assets', 'full', '年度結算需要核對資產與折舊提列。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'view_payroll', 'limited', '只看薪資彙總與總帳差額，不取得任何個人薪資。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('staff', 'staff_self_service', 'full', '一般職員只有本人首頁、薪資單、請假與報支進度等自助操作。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('acct', 'manage_masters', 'full', '內部會計負責維護入帳、報表與收付款所需的基礎主檔。', '2026-08-24 16:26:05.704868+00', 'migration 0012'),
    ('boss', 'manage_masters', 'full', '負責人是公司主檔與簽核政策的最終負責人。', '2026-08-24 16:26:05.704868+00', 'migration 0012'),
    ('boss', 'view_payroll_details', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'manage_payroll', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_audit', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('boss', 'view_system', 'full', '負責人保留這組操作的完整權限。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('admin', 'manage_access', 'full', '系統管理員負責帳號、權限與 Token。', '2026-08-24 16:26:05.817852+00', 'migration 0023'),
    ('admin', 'manage_masters', 'full', '首次設定需要建立銀行帳戶、部門與科目；簽核門檻仍限負責人。', '2026-08-24 16:26:05.817852+00', 'migration 0023'),
    ('admin', 'view_system', 'full', '開啟角色與核准、安全與營運兩個畫面。', '2026-08-24 16:26:05.817852+00', 'migration 0023'),
    ('admin', 'view_masters', 'full', '開啟基礎設定與首次設定精靈。', '2026-08-24 16:26:05.817852+00', 'migration 0023'),
    ('accountant', 'view_audit', 'full', '查核需要追溯操作與被拒絕的完整稽核紀錄。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('admin', 'view_audit', 'full', '開啟 MCP 與 AI 的稽核紀錄。', '2026-08-24 16:26:05.817852+00', 'migration 0023'),
    ('acct', 'run_imports', 'full', '會計可以預覽、確認匯入，必要時復原帳務匯入。', '2026-08-24 16:26:05.694728+00', 'migration 0011'),
    ('accountant', 'view_masters', 'full', '會計師需要科目與分類定義判讀報表。', '2026-08-24 16:26:05.694728+00', 'migration 0011');

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('get_dashboard', true, 'read_only', '讀總覽數字', '2026-08-24 16:26:05.327083+00'),
    ('get_trial_balance', true, 'read_only', '讀試算表、科目餘額', '2026-08-24 16:26:05.327083+00'),
    ('get_cash_position', true, 'read_only', '讀各銀行餘額與現金水位', '2026-08-24 16:26:05.327083+00'),
    ('search_vouchers', true, 'read_only', '查傳票、憑證明細', '2026-08-24 16:26:05.327083+00'),
    ('list_regulations', true, 'read_only', '讀目前適用的法規版本', '2026-08-24 16:26:05.327083+00'),
    ('draft_voucher', true, 'needs_confirm', '草擬分錄，存成待確認傳票', '2026-08-24 16:26:05.327083+00'),
    ('list_accounts', true, 'read_only', '讀會計科目表', '2026-08-24 16:26:05.336026+00'),
    ('get_ar_aging', true, 'read_only', '讀應收帳齡，並與 1131 總帳餘額對帳', '2026-08-24 16:26:05.345112+00'),
    ('list_receivables', true, 'read_only', '讀應收明細：客戶、發票、到期日、未收金額', '2026-08-24 16:26:05.345112+00'),
    ('record_receipt', true, 'needs_confirm', '登記收款，產生待過帳的收款傳票', '2026-08-24 16:26:05.345112+00'),
    ('list_payables', true, 'read_only', '讀應付明細，並與 2101 總帳餘額對帳', '2026-08-24 16:26:05.345112+00'),
    ('create_payment_batch', false, 'high_risk', '送出付款單並產生付款傳票。會產生實際付款，預設關閉。', '2026-08-24 16:26:05.345112+00'),
    ('list_bank_transactions', true, 'read_only', '讀銀行流水與核銷狀態', '2026-08-24 16:26:05.345112+00'),
    ('get_vat_return', true, 'read_only', '試算 401 營業稅申報，銷項稅額與 2211 代收營業稅對帳', '2026-08-24 16:26:05.398102+00'),
    ('list_withholding', true, 'read_only', '讀各類所得扣繳明細，扣繳率與補充保費依 ruleset 現算', '2026-08-24 16:26:05.398102+00'),
    ('list_fixed_assets', true, 'read_only', '讀固定資產：成本、累計折舊、淨額，並與 1601／1602 總帳對帳', '2026-08-24 16:26:05.398102+00'),
    ('run_depreciation', true, 'needs_confirm', '提列某個月的折舊，產生待過帳傳票（借 6152 折舊費用／貸 1602 累計折舊）', '2026-08-24 16:26:05.398102+00'),
    ('get_cash_flow', true, 'read_only', '現金流量表（間接法），期末現金與現金科目餘額對帳', '2026-08-24 16:26:05.457454+00'),
    ('list_chart_of_accounts', true, 'read_only', '讀科目表，含報表分組與現金流量活動別', '2026-08-24 16:26:05.457454+00'),
    ('list_approval_rules', true, 'read_only', '讀費用核准流程與金額門檻', '2026-08-24 16:26:05.457454+00'),
    ('get_system_health', true, 'read_only', '讀 migration 版本、法規版本、稽核與 Token 現況', '2026-08-24 16:26:05.457454+00'),
    ('approve_expense_claim', false, 'high_risk', '簽核報支單，最後一關會產生傳票。對 AI 關閉：送單者自行簽核等同沒有簽核流程。', '2026-08-24 16:26:05.516218+00'),
    ('reject_expense_claim', true, 'needs_confirm', '退回報支單給申請人補資料', '2026-08-24 16:26:05.516218+00'),
    ('list_documents', true, 'read_only', '讀文件清單、RustFS 用量與 OCR 現況', '2026-08-24 16:26:05.516218+00'),
    ('upload_document', true, 'needs_confirm', '上傳一份檔案到 RustFS，存下 BLAKE3 雜湊', '2026-08-24 16:26:05.516218+00'),
    ('list_imports', true, 'read_only', '讀匯入來源、批次紀錄與對外整合現況', '2026-08-24 16:26:05.516218+00'),
    ('preview_import', true, 'needs_confirm', '匯入預覽：解析、驗證、逐列回報問題。不寫入任何資料。', '2026-08-24 16:26:05.516218+00'),
    ('list_ai_audit', true, 'read_only', '讀 AI 稽核紀錄', '2026-08-24 16:26:05.327083+00'),
    ('update_company_setting', false, 'admin_only', '改公司設定，對 AI 預設關閉。', '2026-08-24 16:26:05.457454+00'),
    ('list_roles', true, 'read_only', '讀角色權限對照表：哪個角色有哪些權限。', '2026-08-24 16:26:05.457454+00'),
    ('list_dimensions', true, 'read_only', '讀部門清單', '2026-08-24 16:26:05.457454+00'),
    ('create_voucher', false, 'high_risk', '直接開立待過帳傳票。AI 請改用草擬分錄。', '2026-08-24 16:26:05.336026+00'),
    ('get_balance_sheet', true, 'read_only', '資產負債表，由已過帳分錄現算；會驗證資產＝負債＋權益', '2026-08-24 16:26:05.457454+00'),
    ('get_income_statement', true, 'read_only', '損益表，期間可選，由已過帳分錄現算', '2026-08-24 16:26:05.457454+00'),
    ('commit_import', false, 'high_risk', '寫入一批傳票。對 AI 關閉：復原只救得回尚未過帳的部分。', '2026-08-24 16:26:05.516218+00'),
    ('rollback_import', true, 'high_risk', '復原一批匯入，是往安全方向的操作，對 AI 開著。', '2026-08-24 16:26:05.516218+00'),
    ('post_voucher', false, 'high_risk', '直接過帳；過帳後不能修改，預設關閉。', '2026-08-24 16:26:05.327083+00'),
    ('get_hr_dashboard', true, 'read_only', '讀人資總覽：在職人數、待辦事項與人資財務的銜接狀態', '2026-08-24 16:26:05.608173+00'),
    ('get_department_pl', true, 'read_only', '讀部門損益。未指定部門的分錄列為「未指定」，不分攤。', '2026-08-24 16:26:05.516218+00'),
    ('get_budget_status', true, 'read_only', '讀預算執行率。實際數一律現查總帳。', '2026-08-24 16:26:05.516218+00'),
    ('get_document', true, 'read_only', '取回檔案內容並比對雜湊；雜湊不符就不回傳內容。', '2026-08-24 16:26:05.516218+00'),
    ('get_security_policy', true, 'read_only', '讀控制規則；AI 禁止事項依目前的工具開關現算', '2026-08-24 16:26:05.457454+00'),
    ('list_expense_claims', true, 'read_only', '讀費用報支單與目前關卡', '2026-08-24 16:26:05.516218+00'),
    ('decide_leave_request', false, 'needs_confirm', '准假或駁回。對 AI 關閉：核准請假屬主管職責。', '2026-08-24 16:26:05.608173+00'),
    ('run_payroll', false, 'high_risk', '計算並產生某一期的薪資傳票。對 AI 關閉。', '2026-08-24 16:26:05.608173+00'),
    ('get_insurance_summary', true, 'read_only', '讀勞健保與勞退：公司負擔、員工自付、逐人投保級距。費率全部來自規則庫。', '2026-08-24 16:26:05.608173+00'),
    ('create_account', false, 'needs_confirm', '新增科目會改變三張財務報表的分類結構，AI 預設不得自行擴充科目表。', '2026-08-24 16:26:05.704868+00'),
    ('update_account', false, 'needs_confirm', '修改或停用科目會影響後續入帳與報表分類，AI 預設關閉。', '2026-08-24 16:26:05.704868+00'),
    ('create_employee', false, 'needs_confirm', '員工主檔含薪資、投保申報事實與遮罩個資，AI 預設不得自行建立。', '2026-08-24 16:26:05.704868+00'),
    ('update_employee', false, 'needs_confirm', '員工狀態、薪資與投保申報事實屬敏感主檔，AI 預設關閉。', '2026-08-24 16:26:05.704868+00'),
    ('create_bank_account', false, 'needs_confirm', '銀行帳戶會綁定總帳科目並成為收付款入口，AI 預設不得自行建立。', '2026-08-24 16:26:05.704868+00'),
    ('set_approval_rule', false, 'boss_only', '簽核關卡與金額門檻是公司政策；AI 不得自行改寫約束自己的流程。', '2026-08-24 16:26:05.704868+00'),
    ('create_ap_bill', true, 'needs_confirm', '建立應付帳單與待過帳傳票；屬日常單據，仍須人工過帳才進正式總帳。', '2026-08-24 16:26:05.704868+00'),
    ('create_ar_invoice', true, 'needs_confirm', '建立非電子發票應收與待過帳傳票；屬日常單據，仍須人工過帳才進正式總帳。', '2026-08-24 16:26:05.704868+00'),
    ('void_voucher', false, 'high_risk', '作廢會讓一張已過帳傳票退出總帳與報表；AI 預設不得自行判定原交易不存在。', '2026-08-24 16:26:05.716081+00'),
    ('reverse_voucher', false, 'high_risk', '沖銷會立即過帳一張反向傳票；AI 預設不得自行決定更正日期與正式總帳分錄。', '2026-08-24 16:26:05.716081+00'),
    ('list_attendance_punches', true, 'read_only', '讀某個月全公司的打卡明細與統計，並與匯入的 attendance 月統計對帳。', '2026-08-24 16:26:05.730849+00'),
    ('create_dimension_value', false, 'needs_confirm', '新增分類項目，對 AI 預設關閉。', '2026-08-24 16:26:05.704868+00'),
    ('list_employees', true, 'read_only', '讀員工名冊。不含薪資；薪資明細限負責人。', '2026-08-24 16:26:05.608173+00'),
    ('create_fixed_asset', true, 'needs_confirm', '登記固定資產並產生取得傳票（借資產科目與 1284 進項稅額／貸付款科目）', '2026-08-24 16:26:05.986244+00'),
    ('list_my_api_tokens', true, 'read_only', '讀本人可用的 MCP 工具清單與自己核發的 Token。', '2026-08-24 16:26:05.749476+00'),
    ('attach_my_expense_receipt', false, 'needs_confirm', '為本人送出的報支單附上收據檔案。寫入物件儲存且無法刪除，預設關閉。', '2026-08-24 16:26:05.781156+00'),
    ('get_company_settings', true, 'read_only', '讀公司與帳務設定；營業稅由法規規則庫決定。', '2026-08-24 16:26:05.457454+00'),
    ('list_api_tokens', true, 'read_only', '讀 API／MCP Token 清單，只有前八碼與雜湊。', '2026-08-24 16:26:05.457454+00'),
    ('get_my_home', true, 'read_only', '讀本人基本資料、出勤、特休與送單進度。', '2026-08-24 16:26:05.671698+00'),
    ('list_my_payslips', false, 'read_only', '只讀本人的薪資單，預設關閉。', '2026-08-24 16:26:05.671698+00'),
    ('list_my_leave_requests', true, 'read_only', '讀本人的休假單與特休餘額。', '2026-08-24 16:26:05.671698+00'),
    ('list_my_expense_claims', true, 'read_only', '讀本人送出的費用報支與逐關進度。', '2026-08-24 16:26:05.671698+00'),
    ('list_my_punches', true, 'read_only', '讀本人的打卡明細與當月統計。', '2026-08-24 16:26:05.730849+00'),
    ('submit_my_expense_claim', false, 'needs_confirm', '以本人身分送一張費用報支單，預設關閉。', '2026-08-24 16:26:05.724194+00'),
    ('issue_my_api_token', false, 'needs_confirm', '以本人身分核發 MCP Token，預設關閉。', '2026-08-24 16:26:05.749476+00'),
    ('list_user_accounts', false, 'admin_only', '讀登入帳號清單，對 AI 預設關閉。', '2026-08-24 16:26:05.842597+00'),
    ('update_user_account', false, 'admin_only', '改登入帳號的顯示欄位、狀態與員工連結，對 AI 預設關閉。', '2026-08-24 16:26:05.851238+00'),
    ('delete_user_account', false, 'admin_only', '刪除登入帳號，對 AI 關閉：移除帳號是人的決定。', '2026-08-24 16:26:05.860045+00'),
    ('change_user_role', false, 'admin_only', '變更帳號角色，對 AI 關閉：決定誰能做什麼是人的判斷。', '2026-08-24 16:26:06.044679+00'),
    ('list_my_punch_corrections', false, 'read_only', '只讀本人的補登申請。個人出勤不預設交給 AI，預設關閉。', '2026-08-24 16:26:06.052964+00'),
    ('list_punch_corrections', true, 'read_only', '讀全公司的打卡補登申請與目前狀態。', '2026-08-24 16:26:06.052964+00'),
    ('decide_punch_correction', false, 'needs_confirm', '核准或駁回一張補登申請，核准即寫入打卡紀錄。不由 AI 代為決定，預設關閉。', '2026-08-24 16:26:06.052964+00'),
    ('unlink_user_identity', false, 'admin_only', '解除外部帳號連結，對 AI 關閉：決定誰能登入是人的判斷。', '2026-08-24 16:26:06.071906+00'),
    ('update_oidc_setting', false, 'admin_only', '改外部登入設定，對 AI 關閉：決定誰能登入是人的判斷。', '2026-08-24 16:26:06.080235+00'),
    ('set_role_permission', false, 'admin_only', '改權限對照表，對 AI 預設關閉。', '2026-08-24 16:26:05.457454+00'),
    ('get_payroll_summary', true, 'read_only', '讀某一期薪資彙總，並與總帳 6111 對帳。彙總不含個人金額。', '2026-08-24 16:26:05.608173+00'),
    ('get_hr_compliance', true, 'read_only', '讀法遵檢核表：最低工資、投保級距、勞退提繳率、特休。沒收錄的規則會照實標示為未檢查。', '2026-08-24 16:26:05.608173+00'),
    ('list_payslips', false, 'boss_only', '逐人薪資明細。限負責人，且對 AI 關閉。', '2026-08-24 16:26:05.608173+00'),
    ('list_withholding_statements', false, 'boss_only', '年度扣繳憑單（逐人年所得）。限負責人，且對 AI 關閉。', '2026-08-24 16:26:05.608173+00'),
    ('submit_expense_claim', true, 'needs_confirm', '送出報支單；送單不會付款。', '2026-08-24 16:26:05.516218+00'),
    ('suggest_bank_matches', true, 'read_only', '為一筆銀行流水找出可能相符的傳票', '2026-08-24 16:26:05.345112+00'),
    ('punch_clock', false, 'needs_confirm', '以本人身分打上班或下班卡，預設關閉。', '2026-08-24 16:26:05.730849+00'),
    ('request_leave', false, 'needs_confirm', '以本人身分送出請假單，預設關閉。', '2026-08-24 16:26:05.671698+00'),
    ('submit_my_punch_correction', false, 'needs_confirm', '以本人身分送出打卡補登申請。AI 代填出勤時間等同代寫出勤紀錄，預設關閉。', '2026-08-24 16:26:06.052964+00'),
    ('match_bank_transaction', true, 'needs_confirm', '將一筆銀行流水核銷至一張傳票', '2026-08-24 16:26:05.345112+00'),
    ('issue_api_token', false, 'admin_only', '核發一組新 Token；明文只回傳一次。對 AI 預設關閉。', '2026-08-24 16:26:05.457454+00'),
    ('revoke_api_token', true, 'high_risk', '撤銷一組 Token，對 AI 開著。', '2026-08-24 16:26:05.457454+00'),
    ('revoke_my_api_token', false, 'needs_confirm', '撤銷本人自助核發的 Token。預設關閉；畫面上的撤銷不受此限。', '2026-08-24 16:26:05.749476+00'),
    ('set_budget', false, 'boss_only', '改預算金額。對 AI 關閉：預算是比較基準，不應由 AI 變更。', '2026-08-24 16:26:05.516218+00'),
    ('create_budget_version', false, 'boss_only', '建立年度預算版本。對 AI 關閉：預算是比較基準，不應由 AI 自行建立。', '2026-08-24 16:26:05.939963+00'),
    ('reset_user_password', false, 'admin_only', '重設登入密碼，對 AI 關閉：交付密碼須由人執行。', '2026-08-24 16:26:05.877876+00'),
    ('create_user_account', false, 'admin_only', '建立登入帳號。對 AI 關閉：建立帳號是人的決定。', '2026-08-24 16:26:05.817852+00'),
    ('correct_punch', false, 'needs_confirm', '補登或更正某人某天的打卡時間，來源標示為補登。不由 AI 代為決定，預設關閉。', '2026-08-24 16:26:05.730849+00'),
    ('get_oidc_setting', false, 'admin_only', '讀外部登入設定，對 AI 關閉：這是系統本身的設定。', '2026-08-24 16:26:06.080235+00'),
    ('compute_withholding', true, 'read_only', '試算一筆給付要扣繳的稅額與二代健保補充保費', '2026-08-24 16:26:05.398102+00'),
    ('compute_insurance', true, 'read_only', '試算某個投保級距的三項保費，並回報依哪一版規則', '2026-08-24 16:26:05.608173+00'),
    ('list_leave_requests', true, 'read_only', '讀請假單與特休額度（額度依年資由特休規則計算）', '2026-08-24 16:26:05.608173+00'),
    ('preview_depreciation', true, 'read_only', '試算某個月的折舊，逐筆列出用的耐用年數與規則版本', '2026-08-24 16:26:05.398102+00'),
    ('set_company_timezone', false, 'admin_only', '變更記帳時區。對 AI 關閉：改動會重新定義既有出勤日期的意義。', '2026-08-24 16:26:06.151977+00'),
    ('discard_voucher', false, 'needs_confirm', '刪除後無法復原，AI 預設不得自行判定一張傳票是開錯的。', '2026-08-24 16:26:06.180171+00'),
    ('list_my_overtime_requests', true, 'read_only', '讀本人送出的加班申請與目前狀態。', '2026-08-24 16:26:06.21788+00'),
    ('submit_my_overtime_request', false, 'needs_confirm', '送出加班申請。對 AI 關閉：要不要加班由本人決定。', '2026-08-24 16:26:06.21788+00'),
    ('list_overtime_requests', true, 'read_only', '讀加班申請與本月已核准時數。', '2026-08-24 16:26:06.21788+00'),
    ('decide_overtime_request', false, 'needs_confirm', '核准或駁回加班申請。對 AI 關閉：核准加班屬主管職責。', '2026-08-24 16:26:06.21788+00');

INSERT INTO account (code, name, kind, normal_side, is_postable, parent_code, sort_order, report_group, cash_flow_section, is_active) VALUES
    ('1101', '現金', 'asset', 'debit', true, NULL, 10, 'current_asset', 'cash', true),
    ('1102', '銀行存款', 'asset', 'debit', true, NULL, 20, 'current_asset', 'cash', true),
    ('1131', '應收帳款', 'asset', 'debit', true, NULL, 30, 'current_asset', 'operating', true),
    ('1251', '存貨', 'asset', 'debit', true, NULL, 40, 'current_asset', 'operating', true),
    ('1284', '進項稅額', 'asset', 'debit', true, NULL, 45, 'current_asset', 'operating', true),
    ('1451', '預付費用', 'asset', 'debit', true, NULL, 50, 'current_asset', 'operating', true),
    ('1601', '辦公設備', 'asset', 'debit', true, NULL, 60, 'non_current_asset', 'investing', true),
    ('1602', '累計折舊', 'asset', 'credit', true, NULL, 65, 'non_current_asset', 'operating', true),
    ('2101', '應付帳款', 'liability', 'credit', true, NULL, 110, 'current_liability', 'operating', true),
    ('2171', '應付費用', 'liability', 'credit', true, NULL, 120, 'current_liability', 'operating', true),
    ('2117', '代扣款', 'liability', 'credit', true, NULL, 130, 'current_liability', 'operating', true),
    ('2211', '代收營業稅', 'liability', 'credit', true, NULL, 140, 'current_liability', 'operating', true),
    ('3101', '股本', 'equity', 'credit', true, NULL, 210, 'equity', 'financing', true),
    ('3201', '保留盈餘', 'equity', 'credit', true, NULL, 220, 'equity', 'financing', true),
    ('4111', '銷貨收入', 'revenue', 'credit', true, NULL, 310, 'revenue', NULL, true),
    ('5111', '銷貨成本', 'expense', 'debit', true, NULL, 410, 'cogs', NULL, true),
    ('6111', '薪資支出', 'expense', 'debit', true, NULL, 510, 'operating_expense', NULL, true),
    ('6115', '租金支出', 'expense', 'debit', true, NULL, 520, 'operating_expense', NULL, true),
    ('6118', '水電費', 'expense', 'debit', true, NULL, 530, 'operating_expense', NULL, true),
    ('6122', '旅費', 'expense', 'debit', true, NULL, 540, 'operating_expense', NULL, true),
    ('6131', '郵電費', 'expense', 'debit', true, NULL, 550, 'operating_expense', NULL, true),
    ('6134', '郵電費（快遞）', 'expense', 'debit', true, NULL, 560, 'operating_expense', NULL, true),
    ('6142', '廣告費', 'expense', 'debit', true, NULL, 570, 'operating_expense', NULL, true),
    ('6152', '折舊費用', 'expense', 'debit', true, NULL, 580, 'operating_expense', NULL, true),
    ('6188', '修繕費', 'expense', 'debit', true, NULL, 590, 'operating_expense', NULL, true),
    ('6199', '其他費用', 'expense', 'debit', true, NULL, 600, 'operating_expense', NULL, true);

INSERT INTO dimension (code, label, note, sort_order) VALUES
    ('department', '部門', '', 10);

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('company_name', '公司名稱', '', '', true, 10, '2026-08-24 16:26:05.457454+00', ''),
    ('business_utc_offset', '記帳時區', '+08:00', '出勤日期與單據日期以此時區判定。有出勤紀錄之後不能變更。', false, 15, '2026-08-24 16:26:06.151977+00', ''),
    ('tax_id', '統一編號', '', '', true, 20, '2026-08-24 16:26:05.457454+00', ''),
    ('fiscal_year', '會計年度起始月份', '1', '會計年度起始月份，填 1 到 12。財務報表的預設期間依此設定。', true, 30, '2026-08-24 16:26:05.457454+00', ''),
    ('currency', '記帳本位幣', 'TWD', '記帳本位幣固定為新臺幣。', false, 50, '2026-08-24 16:26:05.457454+00', ''),
    ('budget_alert_percent', '預算提醒門檻', '', '執行率超過設定的百分比就示警；留空不示警。', true, 80, '2026-08-24 16:26:05.516218+00', ''),
    ('work_start', '上班時間', '', '打卡遲到的判定基準，格式 HH:MM。留空不判定遲到。', true, 80, '2026-08-24 16:26:05.730849+00', ''),
    ('work_end', '下班時間', '', '加班時數的判定基準，格式 HH:MM。留空不計加班時數。', true, 90, '2026-08-24 16:26:05.730849+00', ''),
    ('flex_window_minutes', '彈性上班區間（分鐘）', '', '上班時間之後這幾分鐘內到班都算準時，下班時間順延相同分鐘數，工作總時數不變。留空表示不採彈性工時。', true, 95, '2026-08-24 16:26:05.772462+00', ''),
    ('late_grace_minutes', '遲到寬限分鐘', '', '上班時間之後幾分鐘內不計遲到。留空視為 0。', true, 100, '2026-08-24 16:26:05.730849+00', ''),
    ('punch_correction_policy', '補登申請', '人資審核', '員工送出補登申請後的處理方式：人資審核，或自動通過。', true, 110, '2026-08-24 16:26:06.052964+00', '');

INSERT INTO approval_rule (step_no, role_code, label, condition_note, min_amount) VALUES
    (1, NULL, '申請人上傳', '上傳收據', NULL),
    (2, 'acct', '會計確認科目', '一律需要', NULL),
    (3, 'manager', '主管核准', '超過才需要', 10000.00),
    (4, 'boss', '負責人簽核', '超過或非預算內才需要', 50000.00);

INSERT INTO import_source (code, label, format, cadence, supported, note, sort_order) VALUES
    ('voucher', '歷史傳票', 'CSV', '隨時', true, '民國年日期會自動轉換；借貸不平的傳票不會匯入', 40);

INSERT INTO oidc_setting (id, enabled, issuer, client_id, client_secret, redirect_url, allowed_domain, provision_role, updated_at, updated_by, verified_at, verify_error, password_login_disabled) VALUES
    (1, false, '', '', '', '', '', '', '2026-08-24 16:26:06.080235+00', '', NULL, '', false);
