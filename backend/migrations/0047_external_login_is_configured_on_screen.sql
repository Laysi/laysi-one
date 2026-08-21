-- External sign-in was configured with four environment variables, which put
-- it out of reach of the person who owns the installation: turning on Google
-- sign-in meant editing a container's environment and restarting it. The
-- settings now live here and take effect without a restart.
--
-- One row, like install_bootstrap. `client_secret` has to be stored as it is
-- because the token endpoint needs it; no operation ever returns it, and the
-- audit trail masks it by field name.
--
-- `verified_at` and `verify_error` are written by the server after it tries to
-- build a client from these values, so a wrong issuer or a typo in the client
-- id is answered on the settings screen rather than at somebody's first
-- attempt to sign in.

CREATE TABLE oidc_setting (
    id             int PRIMARY KEY CHECK (id = 1),
    enabled        boolean NOT NULL DEFAULT false,
    issuer         text NOT NULL DEFAULT '',
    client_id      text NOT NULL DEFAULT '',
    client_secret  text NOT NULL DEFAULT '',
    redirect_url   text NOT NULL DEFAULT '',
    allowed_domain text NOT NULL DEFAULT '',
    provision_role text NOT NULL DEFAULT '',
    updated_at     timestamptz NOT NULL DEFAULT now(),
    updated_by     text NOT NULL DEFAULT '',
    verified_at    timestamptz,
    verify_error   text NOT NULL DEFAULT ''
);

COMMENT ON TABLE oidc_setting IS
    '外部登入設定；單列表。client_secret 不經任何操作回傳，稽核紀錄依欄位名遮蔽。';
COMMENT ON COLUMN oidc_setting.allowed_domain IS
    'id_token 的 hd claim 必須相符，空字串代表不限網域。';
COMMENT ON COLUMN oidc_setting.provision_role IS
    '網域內首次登入自動建立帳號時使用的角色代碼；空字串代表不自動建立。';

INSERT INTO oidc_setting (id) VALUES (1);

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,unlink_user_identity,delete_user_account,list_user_accounts,get_oidc_setting,update_oidc_setting}'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('get_oidc_setting', false, 'admin_only',
     '讀外部登入設定，對 AI 關閉：這是系統本身的組態。'),
    ('update_oidc_setting', false, 'admin_only',
     '改外部登入設定，對 AI 關閉：決定誰能登入是人的判斷。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
