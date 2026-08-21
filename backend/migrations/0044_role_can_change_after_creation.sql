-- A role was fixed at account creation so that an administrator could not
-- promote themselves to owner and read everyone's pay. The cost was that a
-- promotion or a transfer had to be done by deleting the login and building
-- it again, which loses the account's history.
--
-- The guard belongs on the caller's own account, not on every account:
-- creating a login with any role is already an administrator's to do, so
-- moving somebody else between roles adds no authority. `change_user_role`
-- refuses the caller's own login, refuses to empty the last active
-- administrator, and ends the account's sessions and self-issued MCP tokens
-- in the same transaction, since `api_token.actor_role` is a snapshot that
-- would otherwise keep the old role callable.

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,delete_user_account,list_user_accounts}'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('change_user_role', false, 'admin_only',
     '變更帳號角色，對 AI 關閉：決定誰能做什麼是人的判斷。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
