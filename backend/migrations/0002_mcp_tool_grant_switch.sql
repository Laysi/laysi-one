-- The AI tool switches become editable on screen.
--
-- mcp_tool_grant decides what an AI client may call, and until now the only
-- way to change a row was to open the database. A new tool therefore stayed
-- at whatever the seed said forever, and the screen that shows the switches
-- could only report them.
--
-- The operation that flips them is itself seeded off: an administrator's MCP
-- token could otherwise open every other tool, and `set_mcp_tool_grant` also
-- refuses to enable itself.
INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('set_mcp_tool_grant', false, 'high_risk', '開啟或關閉一項 AI 工具。預設關閉：開著等於讓 AI 決定 AI 做得了什麼。', now())
ON CONFLICT (tool_name) DO NOTHING;

-- The whole row is restated rather than appended to, because the tests that
--守 registry、permission 與 grant 三者相等的那兩個 read the migrations as text
-- and only see an INSERT.
INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_access', '改權限與 MCP', '建立與檢視登入帳號、修改公司設定與角色權限，開關 AI 工具，並簽發或撤銷 API／MCP Token', '{update_company_setting,set_company_timezone,set_role_permission,set_mcp_tool_grant,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,unlink_user_identity,delete_user_account,list_user_accounts,get_oidc_setting,update_oidc_setting,get_backup_setting,update_backup_setting,confirm_backup_key}', 100)
ON CONFLICT (code) DO UPDATE SET label = EXCLUDED.label, note = EXCLUDED.note, op_names = EXCLUDED.op_names;
