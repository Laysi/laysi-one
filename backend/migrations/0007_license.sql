-- Product licensing: the activation key, the signed token it earns, and the
-- fingerprint that ties both to this machine.
--
-- 0001 is applied and frozen, so everything here is additive.
--
-- One row, like `oidc_setting` and `backup_setting`. What it holds is not a
-- setting anybody chooses: `install_id` is drawn once by the database, the
-- token arrives from the licence server, and the only field a person ever
-- types is the activation key their reseller gave them.

CREATE TABLE license_state (
    id integer DEFAULT 1 NOT NULL,
    -- Drawn here so that every installation differs from the first boot,
    -- before anything has been configured and without asking anyone.
    install_id uuid DEFAULT gen_random_uuid() NOT NULL,
    activation_key text DEFAULT ''::text NOT NULL,
    -- The signed payload. Everything the screens show about the licence is
    -- read out of this rather than stored in columns of its own: a second
    -- copy is a copy that can be edited without the signature noticing.
    token text DEFAULT ''::text NOT NULL,
    -- Handed out by the last successful check and presented on the next one.
    -- Two copies of this database each invalidate the other's, which is how
    -- the licence server learns that a clone exists.
    nonce text DEFAULT ''::text NOT NULL,
    last_check_at timestamp with time zone,
    last_error text DEFAULT ''::text NOT NULL,
    -- Installations that predate licensing. Without it, upgrading would put
    -- every existing customer into read-only on the day they pulled the
    -- image. A fresh install gets NULL and activates during initialisation.
    grace_until date,
    CONSTRAINT license_state_pkey PRIMARY KEY (id),
    CONSTRAINT license_state_id_check CHECK ((id = 1))
);
COMMENT ON TABLE license_state IS
    '本安裝的授權；單列表。啟動金鑰與授權檔不經任何操作回傳，稽核紀錄依欄位名遮蔽。';
COMMENT ON COLUMN license_state.install_id IS
    '本安裝的識別碼，建表時由資料庫抽出；與主機識別碼一起組成綁定用的指紋。';
COMMENT ON COLUMN license_state.grace_until IS
    '導入授權機制之前就存在的安裝才有值；到期後與未啟用相同。';

-- One row, and only one. An installation that already has a first account
-- predates this migration, so it gets ninety days to enter its key; a fresh
-- database gets nothing, because initialisation asks for the key before it
-- creates the administrator account.
INSERT INTO license_state (id, grace_until)
SELECT 1,
       CASE WHEN EXISTS (SELECT 1 FROM install_bootstrap)
            THEN current_date + 90
            ELSE NULL
       END
ON CONFLICT (id) DO NOTHING;

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_license', '授權', '查看本安裝的授權、被授權單位與經銷商；輸入啟動金鑰與重新驗證限系統管理員', '{get_license,activate_license}', 57)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO role_permission (role_code, permission_code, level, note, updated_at, updated_by) VALUES
    ('admin', 'manage_license', 'full', '授權屬於這套系統本身，與帳號、Token 同一組職責。', '2026-09-02 00:00:00+00', 'migration 0007'),
    ('boss', 'manage_license', 'full', '負責人保留這組操作的完整權限。', '2026-09-02 00:00:00+00', 'migration 0007')
ON CONFLICT (role_code, permission_code) DO UPDATE
    SET level = EXCLUDED.level, note = EXCLUDED.note,
        updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by;

-- Both closed to AI. Reading the licence would tell a model the customer's
-- own company name and reseller, and entering a key is a purchase decision.
INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('get_license', false, 'admin_only', '讀本安裝的授權狀態與被授權單位。對 AI 關閉。', '2026-09-02 00:00:00+00'),
    ('activate_license', false, 'admin_only', '輸入啟動金鑰或離線授權檔，並向授權伺服器重新驗證。對 AI 關閉：採購是人的決定。', '2026-09-02 00:00:00+00')
ON CONFLICT (tool_name) DO NOTHING;
