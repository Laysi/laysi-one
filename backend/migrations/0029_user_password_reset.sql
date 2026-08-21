-- Forgetting a password left one way out: `laysi-admin reset-password` from a
-- shell on the server. The one-time password from create_user_account is shown
-- once and stored only as an Argon2 hash, so an administrator who cannot reach
-- a shell had nothing to give the person who lost it.
--
-- The new password ends that account's live sessions as well. An account with
-- no password hash signs in through its identity provider only, and the
-- operation refuses it rather than quietly turning it into a password login.

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,reset_user_password,delete_user_account,list_user_accounts}'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('reset_user_password', false, 'admin_only',
     '重設登入密碼，對 AI 關閉：交付密碼是人對人的動作。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
