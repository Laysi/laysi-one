-- Laysi One: report groups, role permissions, approval flows, API tokens,
-- company settings
--
-- There is no table of statement figures in this file, and there never will
-- be.
--
-- Every figure on the balance sheet, income statement and cash flow
-- statement is computed from `voucher_line` on demand. The moment a
-- statement can disagree with the ledger the whole system loses its point:
-- the only question users ask is where a number came from, and the only
-- defensible answer is that the entries were added up. Storing a statement
-- amount offers a second answer, and two answers eventually diverge.
--
-- What this migration adds is grouping, not amounts: two columns on
-- `account` saying which section of the balance sheet it belongs to and
-- which cash flow activity. Grouping is configuration; amounts are derived.
--
-- The grouping belongs in the database rather than in a Rust match because
-- "1131 receivables is a current asset" is part of the chart of accounts,
-- not program logic. The person adding an account is an accountant, not an
-- engineer, and they must state which section it appears in or the account
-- would vanish from the statements (or worse, be defaulted into one). A
-- CHECK ties it to the account kind, so an asset cannot be grouped into the
-- income statement; SQL guarantees that.
--
-- Role permissions and MCP tool grants are two different things.
-- `mcp_tool_grant` governs which tools AI may call; `role_permission`
-- governs what people may do. Switching post_voucher off for AI must not
-- stop the accountant posting (see Channel in laysi-ops), and separate
-- tables are what make that true in the data model.
--
-- `Registry::dispatch` reads `role_permission` into `permission.op_names` on
-- every call and refuses when there is no row; `Risk::BossOnly` remains an
-- additional hard limit.

-- Report grouping for accounts

ALTER TABLE account
    ADD COLUMN report_group       text,
    ADD COLUMN cash_flow_section  text;

COMMENT ON COLUMN account.report_group IS
    '這個科目在資產負債表／損益表的哪一段。分組是設定，金額一律由 voucher_line 推導。';
COMMENT ON COLUMN account.cash_flow_section IS
    '現金流量表的活動別。cash 代表它本身就是現金；損益科目為 NULL（由本期損益代表）。';

UPDATE account SET report_group = CASE
        WHEN code IN ('1601','1602')          THEN 'non_current_asset'
        WHEN kind = 'asset'                   THEN 'current_asset'
        WHEN kind = 'liability'               THEN 'current_liability'
        WHEN kind = 'equity'                  THEN 'equity'
        WHEN kind = 'revenue'                 THEN 'revenue'
        WHEN code = '5111'                    THEN 'cogs'
        ELSE 'operating_expense'
    END,
    cash_flow_section = CASE
        -- 1101 and 1102 are cash itself, and their movement is what this
        -- statement explains.
        WHEN code IN ('1101','1102')          THEN 'cash'
        -- 1602 accumulated depreciation sits in operating, because the first
        -- adjustment of the indirect method is adding depreciation back. The
        -- movement of 1601 is investing (buying equipment really paid out).
        WHEN code = '1602'                    THEN 'operating'
        WHEN code = '1601'                    THEN 'investing'
        WHEN kind IN ('asset','liability')    THEN 'operating'
        WHEN kind = 'equity'                  THEN 'financing'
        ELSE NULL
    END;

ALTER TABLE account
    ALTER COLUMN report_group SET NOT NULL,
    -- An account cannot be both an asset and appear on the income statement.
    -- SQL enforces that rather than relying on anyone remembering.
    ADD CONSTRAINT account_report_group_matches_kind CHECK (
        (kind = 'asset'     AND report_group IN ('current_asset','non_current_asset'))
     OR (kind = 'liability' AND report_group IN ('current_liability','non_current_liability'))
     OR (kind = 'equity'    AND report_group =  'equity')
     OR (kind = 'revenue'   AND report_group IN ('revenue','other_income'))
     OR (kind = 'expense'   AND report_group IN ('cogs','operating_expense','other_expense'))
    ),
    ADD CONSTRAINT account_cash_flow_section_values CHECK (
        cash_flow_section IS NULL
     OR cash_flow_section IN ('cash','operating','investing','financing')
    ),
    -- Balance sheet accounts must have an activity and P&L accounts must not:
    -- operating activities start from profit, which already contains revenue
    -- and expense, and counting them again would double up.
    ADD CONSTRAINT account_cash_flow_section_matches_kind CHECK (
        (kind IN ('asset','liability','equity')) = (cash_flow_section IS NOT NULL)
    ),
    ADD CONSTRAINT account_only_assets_can_be_cash CHECK (
        cash_flow_section IS DISTINCT FROM 'cash' OR kind = 'asset'
    );

-- Company and accounting settings
--
-- Regulation values do not enter this table. The VAT rate and the number of
-- months per filing period look like settings but are fixed by the VAT Act,
-- so they may live only in backend/regulation/vat.toml.
-- get_company_settings returns that as a derived row marked as sourced from
-- the ruleset and not editable: the owner cannot change the tax rate, which
-- was never theirs to change.

CREATE TABLE company_setting (
    code       text PRIMARY KEY,
    label      text NOT NULL,
    value      text NOT NULL,
    note       text NOT NULL DEFAULT '',
    -- false means changing this entry requires a schema change or reopening
    -- the period, not just editing a string.
    editable   boolean NOT NULL DEFAULT true,
    sort_order int NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text NOT NULL DEFAULT ''
);

COMMENT ON TABLE company_setting IS
    '公司與帳務設定。法規決定的數值（稅率、級距）不得進這張表，那些只住在 backend/regulation/。';

INSERT INTO company_setting (code, label, value, note, editable, sort_order) VALUES
    ('company_name',   '公司名稱',     '', '',                                             true,  10),
    ('tax_id',         '統一編號',     '', '報稅與電子發票都使用公司設定的統一編號',       true,  20),
    ('fiscal_year',    '會計年度',     '', '非曆年制需要重建會計期間，不是只改顯示字串', false, 30),
    ('period',         '本期會計期間', '', '由結帳狀態與目前會計期間決定',                 false, 40),
    ('currency',       '記帳本位幣',   '', '',                                             false, 50),
    ('opening_locked', '期初餘額',     '', '已過帳的期初傳票不可異動',                   false, 60),
    ('doc_number',     '單據編號規則', '', '由系統的單據編號器產生',                     false, 70);

-- Dimensions and terms
--
-- Departments, projects and cost centres are configuration values only and
-- are not yet attached to voucher_line. Establishing the data model first
-- is more honest than four hard-coded lines on screen, and it is what
-- budgets and departmental P&L attach to.

CREATE TABLE dimension (
    code       text PRIMARY KEY,
    label      text NOT NULL,
    note       text NOT NULL DEFAULT '',
    sort_order int NOT NULL DEFAULT 0
);

CREATE TABLE dimension_value (
    id             serial PRIMARY KEY,
    dimension_code text NOT NULL REFERENCES dimension(code) ON DELETE CASCADE,
    code           text NOT NULL,
    label          text NOT NULL,
    active         boolean NOT NULL DEFAULT true,
    sort_order     int NOT NULL DEFAULT 0,
    UNIQUE (dimension_code, code)
);

INSERT INTO dimension (code, label, note, sort_order) VALUES
    ('department',   '部門',     '費用報支與部門損益會用到',       10),
    ('project',      '專案',     '專案損益',                       20),
    ('cost_center',  '成本中心', '倉儲與據點',                     30),
    ('payment_term', '付款條件', '決定應收應付的到期日怎麼推算',   40);

-- Roles and permissions
--
-- A hard-coded permission matrix can only be read, never enforced, and a
-- read-only permission table makes people believe permissions are already
-- in effect. As a table, "who may do what" at least has one place to look
-- it up, change it, and see who changed it.

CREATE TABLE role (
    code       text PRIMARY KEY,
    name       text NOT NULL,
    note       text NOT NULL DEFAULT '',
    sort_order int NOT NULL DEFAULT 0
);

COMMENT ON TABLE role IS
    'code 與 ai_audit_log.actor_role、OpCtx.actor_role 是同一組值，稽核中的身分要對得回這張表。';

CREATE TABLE permission (
    code       text PRIMARY KEY,
    label      text NOT NULL,
    note       text NOT NULL DEFAULT '',
    -- Which operations this permission governs. An empty array means the
    -- corresponding operations are not implemented yet.
    op_names   text[] NOT NULL DEFAULT '{}',
    sort_order int NOT NULL DEFAULT 0
);

COMMENT ON COLUMN permission.op_names IS
    '對應到 Registry 裡的操作名稱。dispatch 每次呼叫都即時查這一欄。';

-- No row means no permission. level distinguishes only full from limited.
CREATE TABLE role_permission (
    role_code       text NOT NULL REFERENCES role(code) ON DELETE CASCADE,
    permission_code text NOT NULL REFERENCES permission(code) ON DELETE CASCADE,
    level           text NOT NULL CHECK (level IN ('full','limited')),
    note            text NOT NULL DEFAULT '',
    updated_at      timestamptz NOT NULL DEFAULT now(),
    updated_by      text NOT NULL DEFAULT '',
    PRIMARY KEY (role_code, permission_code)
);

INSERT INTO role (code, name, note, sort_order) VALUES
    ('boss',       '老闆',   '看得到全部，含薪資',           10),
    ('acct',       '會計',   '可開立傳票、不可付款',         20),
    ('cashier',    '出納',   '執行匯款，不碰分錄',           30),
    ('manager',    '主管',   '簽核部門的費用報支',           40),
    ('accountant', '會計師', '事務所端，調整分錄與年度結算', 50);

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('view_reports',    '看報表與現金', '三張財務報表、試算表、現金水位',
        '{get_balance_sheet,get_income_statement,get_cash_flow,get_trial_balance,get_cash_position,get_dashboard}', 10),
    ('create_voucher',  '開立傳票',     '含 AI 草稿的確認',
        '{create_voucher,draft_voucher}', 20),
    ('post_voucher',    '過帳／結帳',   '過帳後不可修改，更正只能開反向分錄',
        '{post_voucher}', 30),
    ('adjust_entry',    '調整分錄（會計師）', '事務所端的年度調整',
        '{}', 40),
    ('issue_invoice',   '開發票',       '電子發票開立與重送',
        '{issue_einvoice,retry_einvoice_upload}', 50),
    ('request_payment', '送出付款申請', '只是申請，錢還沒出去',
        '{}', 60),
    ('execute_payment', '實際匯款',     '產生實際付款',
        '{create_payment_batch}', 70),
    ('approve_expense', '簽核費用報支', '費用報支模組在後續里程碑',
        '{}', 80),
    ('view_payroll',    '看薪資明細',   '人資模組在後續里程碑',
        '{}', 90),
    ('manage_access',   '改權限與 MCP', '改權限、改設定、發 Token',
        '{set_role_permission,update_company_setting,issue_api_token,revoke_api_token}', 100);

INSERT INTO role_permission (role_code, permission_code, level, note) VALUES
    ('boss','view_reports','full',''),         ('acct','view_reports','full',''),
    ('cashier','view_reports','limited','只看現金與付款相關'),
    ('manager','view_reports','limited','只看自己部門'),
    ('accountant','view_reports','full',''),

    ('acct','create_voucher','full',''),       ('accountant','create_voucher','full',''),
    ('acct','post_voucher','full',''),         ('accountant','post_voucher','full',''),

    ('acct','adjust_entry','limited','事務所提的調整由會計登打'),
    ('accountant','adjust_entry','full',''),

    ('acct','issue_invoice','full',''),        ('cashier','issue_invoice','full',''),

    ('boss','request_payment','full',''),      ('acct','request_payment','full',''),
    ('cashier','request_payment','full',''),   ('manager','request_payment','full',''),

    ('boss','execute_payment','full',''),      ('cashier','execute_payment','full',''),

    ('boss','approve_expense','full',''),      ('manager','approve_expense','full',''),

    ('boss','view_payroll','full',''),         ('acct','view_payroll','full',''),
    ('accountant','view_payroll','limited','只看彙總，不看個人'),

    ('boss','manage_access','full','');

-- Approval flow
--
-- Amount thresholds are company policy rather than regulation, so they may
-- live in a table. They still must not live in Rust: a different company
-- has different thresholds, and changing one must not require a rebuild.

CREATE TABLE approval_rule (
    step_no        int PRIMARY KEY,
    -- NULL means the claimant handles this step themselves (uploading the
    -- receipt).
    role_code      text REFERENCES role(code),
    label          text NOT NULL,
    condition_note text NOT NULL DEFAULT '',
    -- NULL means always required; a value means required above that amount.
    min_amount     numeric(18,2)
);

INSERT INTO approval_rule (step_no, role_code, label, condition_note, min_amount) VALUES
    (1, NULL,        '申請人上傳',   '上傳收據',     NULL),
    (2, 'acct',      '會計確認科目', '一律需要',     NULL),
    (3, 'manager',   '主管核准',     '超過才需要',   10000.00),
    (4, 'boss',      '老闆簽核',     '超過或非預算內才需要', 50000.00);

-- Security policies
--
-- The `enforced` column is the reason this table exists. Static copy makes
-- every control rule look as though it is already in effect; making
-- "is it actually applied" a column removes the screen's room for
-- ambiguity.
--
-- "What AI may not do" and the amount thresholds are not in this table:
-- the first is derived from mcp_tool_grant and the second from
-- approval_rule. A fixed sentence saying AI cannot pay, while
-- create_payment_batch is actually enabled, would be a lie.

CREATE TABLE security_policy (
    code       text PRIMARY KEY,
    label      text NOT NULL,
    value      text NOT NULL,
    detail     text NOT NULL DEFAULT '',
    enforced   boolean NOT NULL,
    sort_order int NOT NULL DEFAULT 0
);

INSERT INTO security_policy (code, label, value, detail, enforced, sort_order) VALUES
    ('separation_of_duties', '職責分離',
     '建立傳票的人不能自行核准；會計不能執行匯款',
     '角色權限已由 dispatch 強制執行；建立者不得核准自己的單據仍由各操作另外檢查。',
     false, 20),
    ('two_factor', '雙因素驗證',
     '全員強制（老闆、出納含裝置綁定）',
     '密碼與 OIDC session 已實作，雙因素驗證與裝置綁定尚未實作。',
     false, 40),
    ('audit_retention', '稽核紀錄',
     '所有動作保留 10 年，含被擋下的呼叫',
     '每一次呼叫都寫 ai_audit_log（唯讀與被擋下的呼叫同樣寫入）；防竄改的 trigger 尚未加上。',
     false, 50);

-- API and MCP tokens
--
-- No plaintext is stored. The table keeps a BLAKE3 hash and the first eight
-- characters: the prefix lets a person recognise which token a row is, the
-- hash lets the system compare. Whoever copies the database gets a set of
-- useless hashes rather than credentials that connect. The reasoning is the
-- same as not storing passwords, and a token often carries more authority
-- than one person's password.
--
-- The cost is that a token is visible only at the moment it is issued;
-- afterwards the system cannot read it back either. A forgotten token can
-- only be reissued. That is the correct trade.

CREATE TABLE api_token (
    id           serial PRIMARY KEY,
    name         text NOT NULL,
    kind         text NOT NULL CHECK (kind IN ('api','mcp')),
    scope        text NOT NULL,
    -- First eight characters, only so a person can recognise which token a
    -- row is.
    prefix       text NOT NULL,
    token_hash   text NOT NULL UNIQUE,
    -- Who this token acts as. AI never gets more authority than that person.
    actor        text NOT NULL,
    actor_role   text NOT NULL REFERENCES role(code),
    created_at   timestamptz NOT NULL DEFAULT now(),
    created_by   text NOT NULL,
    last_used_at timestamptz,
    -- NULL means no fixed rotation cycle ("expires at period close", say),
    -- with the reason in expiry_note.
    rotate_days  int CHECK (rotate_days IS NULL OR rotate_days > 0),
    expiry_note  text NOT NULL DEFAULT '',
    revoked_at   timestamptz,
    revoked_by   text NOT NULL DEFAULT ''
);

COMMENT ON COLUMN api_token.token_hash IS
    'BLAKE3 十六進位。明文只在發出來的那一次回傳，系統自己也讀不回來。';

CREATE INDEX api_token_live_idx ON api_token (revoked_at, id DESC);

-- MCP tool grants
--
-- All the statements are open to AI: letting AI read the three statements
-- is exactly what this system is for.
--
-- Changing permissions, changing settings and issuing tokens are off for AI
-- by default. An AI that can grant itself permissions is the same as having
-- no permission system. revoke_api_token is the exception and stays on:
-- revoking moves in the safe direction and must not wait for a person when
-- a token has leaked.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('get_balance_sheet',      true,  'read_only', '資產負債表，由 voucher_line 現算；會驗證資產＝負債＋權益'),
    ('get_income_statement',   true,  'read_only', '損益表，期間可選，由 voucher_line 現算'),
    ('get_cash_flow',          true,  'read_only', '現金流量表（間接法），期末現金與現金科目餘額對帳'),
    ('get_company_settings',   true,  'read_only', '讀公司與帳務設定；營業稅那一列來自法規規則庫，不是設定值'),
    ('update_company_setting', false, 'boss_only', '改公司設定。改這裡會影響全系統，對 AI 預設關閉。'),
    ('list_chart_of_accounts', true,  'read_only', '讀科目表，含報表分組與現金流量活動別'),
    ('list_dimensions',        true,  'read_only', '讀部門、專案、成本中心、付款條件'),
    ('list_roles',             true,  'read_only', '讀角色權限矩陣（目前僅記錄，尚未強制執行）'),
    ('set_role_permission',    false, 'boss_only', '改權限矩陣。對 AI 預設關閉：能自行開通權限的 AI 等同沒有權限控制。'),
    ('list_approval_rules',    true,  'read_only', '讀費用核准流程與金額門檻'),
    ('get_security_policy',    true,  'read_only', '讀控制規則；AI 禁止事項由 mcp_tool_grant 現算，不是寫死的文字'),
    ('list_api_tokens',        true,  'read_only', '讀 API／MCP Token 清單。只有前八碼與雜湊，沒有明文。'),
    ('issue_api_token',        false, 'boss_only', '發一把新 Token。明文只回傳一次。對 AI 預設關閉。'),
    ('revoke_api_token',       true,  'high_risk', '撤銷一把 Token。撤銷是往安全的方向走，所以對 AI 開著。'),
    ('get_system_health',      true,  'read_only', '讀 migration 版本、法規版本、稽核與 Token 現況')
ON CONFLICT (tool_name) DO NOTHING;
