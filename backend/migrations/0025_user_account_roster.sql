-- Reading the account roster is its own operation, because listing logins is
-- now its own screen rather than a block on 「角色與核准」.
--
-- It joins manage_access rather than view_system. view_system is the read
-- permission the owner also holds; who can sign in belongs to whoever creates
-- the logins. The AdminOnly floor on the operation says the same thing a
-- second time, so a future grant of manage_access cannot open it either.

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account,list_user_accounts}',
       note = '建立與檢視登入帳號、修改公司設定與角色權限，並簽發或撤銷 API／MCP Token'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('list_user_accounts', false, 'admin_only',
     '讀登入帳號清單，對 AI 預設關閉。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
