-- The boss role was seeded with the colloquial display name 「老闆」 while the
-- rest of the product says 「負責人」. Screens show both, so the term is fixed
-- in the data, not only in the copy.
--
-- Seeded notes also printed internal names (dispatch, ai_audit_log, trigger)
-- into columns that are rendered on 「安全與營運」 and 「角色與核准」. Those are
-- rewritten here for the same reason: the text is user-facing.

UPDATE role
   SET name = '負責人'
 WHERE code = 'boss'
   AND name = '老闆';

UPDATE approval_rule
   SET label = '負責人簽核'
 WHERE role_code = 'boss'
   AND label = '老闆簽核';

UPDATE hr_flow_step
   SET label = '負責人核定',
       note  = '調薪一律需負責人同意'
 WHERE kind = 'salary_change'
   AND step_no = 2;

UPDATE permission
   SET note = '個人薪資明細與年度扣繳憑單限負責人；彙總（get_payroll_summary）會計看得到'
 WHERE code = 'view_payroll';

UPDATE role_permission
   SET note = '負責人是公司主檔與簽核政策的最終負責人。'
 WHERE role_code = 'boss'
   AND note = '老闆是公司主檔與簽核政策的最終負責人。';

UPDATE role_permission
   SET note = '負責人保留這組操作的完整權限。'
 WHERE role_code = 'boss'
   AND note = '老闆是公司資料與控制的最終負責人，因此保留這組操作的完整權限。';

UPDATE mcp_tool_grant
   SET note = '讀員工名冊。不含薪資；薪資在 list_payslips，該操作限負責人。'
 WHERE tool_name = 'list_employees';

UPDATE security_policy
   SET detail = '每次呼叫都即時檢查角色權限；會計沒有實際匯款權限，出納沒有開立、調整或過帳分錄權限。'
 WHERE code = 'separation_of_duties';

UPDATE security_policy
   SET value = '全員強制（負責人、出納含裝置綁定）'
 WHERE code = 'two_factor';

UPDATE security_policy
   SET detail = '每一次呼叫都寫入稽核紀錄，唯讀與被擋下的呼叫同樣寫入；防竄改機制尚未加上。'
 WHERE code = 'audit_retention';
