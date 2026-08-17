-- Laysi One: what a self-issued token makes safe to read
--
-- 0009 switched every self-service tool off for AI, with a reason that was
-- correct at the time: an MCP token's actor was a name in a row, not the
-- person at the screen, so handing someone's payslip to whatever held the
-- token had no justification. 0016 and 0017 kept the same default.
--
-- 0017 changed the fact the reason rested on. `api_token.owner_employee_id`
-- is set only by `issue_my_api_token`, which takes no field naming anybody:
-- the employee comes from the session of the person pressing the button. A
-- token carrying an owner is therefore bound to one specific employee by that
-- employee, and `/mcp` now resolves `ctx.employee_id` from that column alone.
-- Tokens the owner issued on the installation's behalf still carry no owner,
-- so for them the self-service operations refuse to answer at all rather than
-- guessing whose records to return.
--
-- With that link in place these five reads return exactly what the token
-- holder can already see on their own screens, which is the point of
-- connecting an assistant. They are switched on so that "我的 MCP" does
-- something on a fresh install instead of issuing a token that can call
-- nothing.
--
-- The writes stay off, and so does the payslip read. Clocking in is the clear
-- case: a tool that lets an assistant punch for you is a tool for falsifying
-- an attendance record, and it is worth no convenience. `issue_my_api_token`
-- would let a token mint another token and outlive its own revocation.
-- Salary detail is not refused on principle, only left for a person to turn
-- on deliberately.

UPDATE mcp_tool_grant SET enabled = true,
       note = '讀本人基本資料、出勤、特休與送單進度。Token 由本人自助核發並綁定 owner_employee_id，回傳的就是本人畫面上看得到的內容。'
 WHERE tool_name = 'get_my_home';

UPDATE mcp_tool_grant SET enabled = true,
       note = '讀本人的休假單與特休餘額。對象由 Token 的 owner_employee_id 決定，無法指定他人。'
 WHERE tool_name = 'list_my_leave_requests';

UPDATE mcp_tool_grant SET enabled = true,
       note = '讀本人送出的費用報支與逐關進度。對象由 Token 的 owner_employee_id 決定。'
 WHERE tool_name = 'list_my_expense_claims';

UPDATE mcp_tool_grant SET enabled = true,
       note = '讀本人的打卡明細與當月統計。對象由 Token 的 owner_employee_id 決定。'
 WHERE tool_name = 'list_my_punches';

UPDATE mcp_tool_grant SET enabled = true,
       note = '讀本人可用的 MCP 工具清單與自己核發的 Token。'
 WHERE tool_name = 'list_my_api_tokens';
