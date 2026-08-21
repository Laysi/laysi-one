-- An external identity could only be attached from a shell, and never
-- detached at all. Where the installation provisions accounts from a
-- directory, that made deleting an account meaningless: the next sign-in
-- built it again. `unlink_user_identity` breaks the link, ends the account's
-- sessions in the same transaction, and refuses an account with no password,
-- which unlinking would leave with no way in at all.
--
-- `delete_user_account` now refuses a linked account for the same reason, so
-- removing somebody is one deliberate step rather than one that quietly
-- undoes itself.

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,unlink_user_identity,delete_user_account,list_user_accounts}'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('unlink_user_identity', false, 'admin_only',
     '解除外部帳號連結，對 AI 關閉：決定誰能登入是人的判斷。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
