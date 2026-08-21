-- Editing an account was shell-only, which made one field unreachable in
-- practice: the employee link. Only HR can create employees, so on a fresh
-- install the administrator creates logins before any employee number exists,
-- and `create_user_account` is the sole place that link could ever be set.
--
-- The operation cannot reach `role_code` or `password_hash`. The role stays
-- fixed at creation, which is what stops an administrator promoting anyone to
-- owner; passwords stay with their owner and `laysi-admin reset-password`.

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,list_user_accounts}'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('update_user_account', false, 'admin_only',
     '改登入帳號的顯示欄位、狀態與員工連結，對 AI 預設關閉。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
