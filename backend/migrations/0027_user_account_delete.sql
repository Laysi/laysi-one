-- Removing an account was shell-only. Sessions and external identities already
-- cascade from user_account, and ai_audit_log records the actor as text, so a
-- deleted login leaves the audit trail intact.
--
-- Two accounts can never be removed this way: the last one, because the web
-- bootstrap endpoint does not reopen once an installation has been claimed, and
-- the last active administrator, because nothing restores a role.

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,delete_user_account,list_user_accounts}'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('delete_user_account', false, 'admin_only',
     '刪除登入帳號，對 AI 關閉：移除帳號是人的決定。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
