-- Seeded notes and details are rendered on 「安全與營運」, 「角色與核准」,
-- 「MCP 與 AI」, 「基礎設定」 and 「匯入與整合」, so they are UI copy and follow
-- the same rules as the screens. Two problems are fixed here.
--
-- Internal identifiers reached the screen: ctx.actor, ctx.employee_id,
-- owner_employee_id, api_token, mcp_tool_grant, ocr_status, run_payroll. This
-- repeats what 0022 did for dispatch/ai_audit_log/trigger.
--
-- The rest defended the design instead of stating it: 「不是只改顯示字串」,
-- 「能自行開通權限的 AI 等同沒有權限控制」, 「撤銷是往安全的方向走，所以」.
-- Rationale belongs in docs/architecture.md, not in a column a user reads.

UPDATE company_setting
   SET note = '非曆年制要重建會計期間。'
 WHERE code = 'fiscal_year';

UPDATE mcp_tool_grant SET note = '改公司設定，對 AI 預設關閉。'
 WHERE tool_name = 'update_company_setting';

UPDATE mcp_tool_grant SET note = '改權限矩陣，對 AI 預設關閉。'
 WHERE tool_name = 'set_role_permission';

UPDATE mcp_tool_grant SET note = '讀控制規則；AI 禁止事項依現行工具開關計算。'
 WHERE tool_name = 'get_security_policy';

UPDATE mcp_tool_grant SET note = '撤銷一把 Token，對 AI 開著。'
 WHERE tool_name = 'revoke_api_token';

UPDATE mcp_tool_grant SET note = '讀公司與帳務設定；營業稅那一列由法規規則庫決定。'
 WHERE tool_name = 'get_company_settings';

UPDATE mcp_tool_grant SET note = '讀 API／MCP Token 清單，只有前八碼與雜湊。'
 WHERE tool_name = 'list_api_tokens';

UPDATE mcp_tool_grant SET note = '讀本人基本資料、出勤、特休與送單進度。'
 WHERE tool_name = 'get_my_home';

UPDATE mcp_tool_grant SET note = '只讀本人的薪資單，預設關閉。'
 WHERE tool_name = 'list_my_payslips';

UPDATE mcp_tool_grant SET note = '讀本人的休假單與特休餘額。'
 WHERE tool_name = 'list_my_leave_requests';

UPDATE mcp_tool_grant SET note = '替本人送請假單，預設關閉。'
 WHERE tool_name = 'request_leave';

UPDATE mcp_tool_grant SET note = '讀本人送出的費用報支與逐關進度。'
 WHERE tool_name = 'list_my_expense_claims';

UPDATE mcp_tool_grant SET note = '讀本人的打卡明細與當月統計。'
 WHERE tool_name = 'list_my_punches';

UPDATE mcp_tool_grant SET note = '以本人身分送一張費用報支單，預設關閉。'
 WHERE tool_name = 'submit_my_expense_claim';

UPDATE mcp_tool_grant SET note = '替本人打上班或下班卡，預設關閉。'
 WHERE tool_name = 'punch_clock';

UPDATE mcp_tool_grant SET note = '以本人身分核發 MCP Token，預設關閉。'
 WHERE tool_name = 'issue_my_api_token';

UPDATE integration
   SET detail = '尚未接上辨識引擎，文件的辨識狀態一律為待處理。'
 WHERE code = 'ocr';

UPDATE integration
   SET note = '身分由未撤銷的 Token 決定，每次呼叫更新最後使用時間。'
 WHERE code = 'mcp';

UPDATE integration
   SET note = '薪資傳票由人資模組直接產生。'
 WHERE code = 'payroll';

UPDATE permission
   SET note = '查看應收帳齡、應收明細與待付帳單'
 WHERE code = 'view_subledgers';

UPDATE permission
   SET note = '送出費用報支或建立待過帳應付帳單'
 WHERE code = 'request_payment';

UPDATE security_policy
   SET detail = '建立傳票的人不能自行核准；改權限與看薪資分屬不同角色。'
 WHERE code = 'separation_of_duties';
