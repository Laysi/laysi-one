-- Laysi One: seeded copy uses the words people here use
--
-- Seeded notes are rendered on 「角色與核准」, 「MCP 與 AI」 and 「匯入與整合」,
-- so they are UI copy and follow the screen rules. This continues 0022, 0024
-- and 0030.
--
-- 「矩陣」 is a maths and BI word; a role-by-permission grid is a 對照表 to the
-- accountant reading it. 「回滾」 is a mainland software term; undoing an import
-- is 復原. 「維度」 was already replaced everywhere else in 0030 and the row
-- below is the one it missed. The table and operation names are unchanged.
--
-- The MCP note on `list_roles` also claimed the matrix was recorded but not
-- enforced. It has been enforced on every call since 0011.

UPDATE mcp_tool_grant
   SET note = '讀角色權限對照表：哪個角色有哪些權限。'
 WHERE tool_name = 'list_roles';

UPDATE mcp_tool_grant
   SET note = '改權限對照表，對 AI 預設關閉。'
 WHERE tool_name = 'set_role_permission';

UPDATE mcp_tool_grant
   SET note = '寫入一批傳票。對 AI 關閉：復原只救得回尚未過帳的部分。'
 WHERE tool_name = 'commit_import';

UPDATE mcp_tool_grant
   SET note = '復原一批匯入。往安全的方向走，所以對 AI 開著。'
 WHERE tool_name = 'rollback_import';

UPDATE permission
   SET note = '查看匯入來源、預覽、確認匯入與復原尚未過帳的批次'
 WHERE code = 'run_imports';

UPDATE permission
   SET note = '查看權限對照表、安全政策、Token 後設資料與系統健康狀態'
 WHERE code = 'view_system';

UPDATE role_permission
   SET note = '會計可以預覽、確認匯入，必要時復原帳務匯入。'
 WHERE role_code = 'acct' AND permission_code = 'run_imports';

UPDATE role_permission
   SET note = '會計師需要科目與分類定義判讀報表。'
 WHERE role_code = 'accountant' AND permission_code = 'view_masters';
