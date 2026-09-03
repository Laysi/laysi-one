-- Sending a test message to the push address.
--
-- Saving the address already sends one, but re-saving an unchanged value to
-- find out whether the chat tool is still reachable writes an audit row that
-- says the address was replaced when it was not. `test_notice_push` joins the
-- permission that already holds the rest of the company settings.

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_access', '改權限與 MCP', '建立與檢視登入帳號、修改公司設定與角色權限，開關 AI 工具，並簽發或撤銷 API／MCP Token', '{update_company_setting,set_company_timezone,test_notice_push,set_role_permission,set_mcp_tool_grant,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,unlink_user_identity,delete_user_account,list_user_accounts,get_oidc_setting,update_oidc_setting,get_backup_setting,update_backup_setting,confirm_backup_key,reset_backup_key}', 100)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('test_notice_push', false, 'admin_only', '送一則測試訊息到公告推送網址，對 AI 預設關閉。', '2026-09-04 00:00:00+00')
ON CONFLICT (tool_name) DO NOTHING;
