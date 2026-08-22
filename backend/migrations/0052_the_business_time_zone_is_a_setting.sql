-- The business time zone stops being a constant in Rust.
--
-- It was fixed at +08:00 in three places at once: `laysi_domain`, the frontend
-- and the CHECK constraints below. Two of those are invisible to whoever
-- installs the product, so an install running on a machine in another zone had
-- no way to say so and no way to find out that it could not.
--
-- The stored form is the text PostgreSQL takes in `AT TIME ZONE INTERVAL`, so
-- the setting, the constraints and the screen all read one string.
--
-- Changing it is not a display change. `attendance_punch.punched_on` and
-- `punch_correction.punched_on` are dates *derived* from an instant and then
-- stored, and both are part of a key: a punch at 23:30 under +08:00 belongs to
-- another day under +09:00. `set_company_timezone` therefore rewrites the two
-- CHECK constraints in the same transaction, and refuses while either table
-- holds a row — the offset is settled during setup, before anyone punches.

INSERT INTO company_setting (code, label, value, note, editable, sort_order) VALUES
    ('business_utc_offset', '記帳時區', '+08:00',
     '出勤日期與單據日期以此時區判定。有出勤紀錄之後不能變更。',
     false, 15)
ON CONFLICT (code) DO NOTHING;

-- Seeded as not editable so `update_company_setting` refuses it: writing the
-- text without rewriting the constraints would leave the database checking one
-- offset while the application uses another. `set_company_timezone` owns both.
UPDATE permission
   SET op_names = '{update_company_setting,set_company_timezone,set_role_permission,issue_api_token,revoke_api_token,create_user_account,update_user_account,change_user_role,reset_user_password,unlink_user_identity,delete_user_account,list_user_accounts,get_oidc_setting,update_oidc_setting}'
 WHERE code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('set_company_timezone', false, 'admin_only',
     '變更記帳時區。對 AI 關閉：改動會重新定義既有出勤日期的意義。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;
