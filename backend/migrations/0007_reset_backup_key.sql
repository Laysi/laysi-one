-- A lost restore key can be replaced.
--
-- The pair is generated once, on the first pass that finds age_recipient
-- empty, and confirm_backup_key then drops the private half for good. Losing
-- what was written down therefore left an installation backing up daily to
-- archives nobody could ever open, with no way back on any screen.
-- reset_backup_key clears both halves so the next pass generates a new pair
-- and shows it once. It stops there: archives already written stay encrypted
-- to the old recipient, and deleting them is not this operation's call.
INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('reset_backup_key', false, 'admin_only', '重設還原金鑰，對 AI 關閉：這是系統本身的設定。', now())
ON CONFLICT (tool_name) DO NOTHING;

-- The whole row is restated rather than appended to, because the two tests
-- that hold the registry, permission and grant name sets equal read the
-- migrations as text and only see an INSERT.
INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_access', '改權限與 MCP', '建立與檢視登入帳號、修改公司設定與角色權限，開關 AI 工具，並簽發或撤銷 API／MCP Token', '{update_company_setting,set_company_timezone,set_role_permission,set_mcp_tool_grant,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,unlink_user_identity,delete_user_account,list_user_accounts,get_oidc_setting,update_oidc_setting,get_backup_setting,update_backup_setting,confirm_backup_key,reset_backup_key}', 100)
ON CONFLICT (code) DO UPDATE SET label = EXCLUDED.label, note = EXCLUDED.note, op_names = EXCLUDED.op_names;
