-- 「系統角色」 sat on the employee master and governed nothing.
--
-- Access is decided by `user_account.role_code`, which is fixed when the
-- account is created and cannot be changed afterwards — 「帳號管理」 says so on
-- its own header. The employee master offered the same seven roles in an
-- editable dropdown, so an employee with no login at all could be given
-- 「一般職員」 and saved. The roster then printed it in the same dotted run as
-- the job title and the employment type: 「業務專員 · 正職 · 一般職員」, three
-- unrelated things set in one voice.
--
-- Nothing read the column but the screens that displayed it, so it goes
-- rather than getting a better label. One thing, one name: the role is on the
-- account.

ALTER TABLE employee DROP COLUMN role_code;

-- The value is a month (1 to 12), which the label never said; the setup
-- wizard has always called the same field 「會計年度起始月份」.
UPDATE company_setting
   SET label = '會計年度起始月份'
 WHERE code = 'fiscal_year';
