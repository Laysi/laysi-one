-- 已讀標記，並把「佈告欄」正名為「公告與規章」
--
-- 一人一列，只記「看到哪一刻為止」。它不是 0004 那個閱讀確認：沒有版本、沒有簽署
-- 的意思，人資那一面也讀不到它。它回答的是「這個人還沒看過什麼」，好讓側邊欄那個
-- 數字消得掉；0005 之所以拿掉確認，是因為確認被當成證據用，而這一列不是證據。
--
-- 標記掛在員工而不是登入帳號：自助操作一律以 ctx.employee_id 為鍵，MCP Token 也拿
-- 得到同一個鍵，換成 user_account 會讓兩個通道看到不同的未讀數。

CREATE TABLE notice_read (
    employee_id integer NOT NULL,
    read_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT notice_read_pkey PRIMARY KEY (employee_id),
    CONSTRAINT notice_read_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE
);

COMMENT ON TABLE notice_read IS
    '一位員工把公告與規章看到哪一刻為止。一人一列，用來算未讀數，不是閱讀確認。';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('mark_my_notices_read', false, 'needs_confirm', '把本人的公告與規章標為已讀。這一下要由本人按，對 AI 一律關閉。', now());

UPDATE mcp_tool_grant SET note = '讀公告與規章，含修訂紀錄' WHERE tool_name = 'list_notices';
UPDATE mcp_tool_grant SET note = '讀本人看得到的公告與規章，含修訂紀錄' WHERE tool_name = 'list_my_notices';
UPDATE mcp_tool_grant SET note = '發布公告或規章，對 AI 預設關閉。' WHERE tool_name = 'create_notice';
UPDATE mcp_tool_grant SET note = '修改公告或規章，改動內容會發布新的一版。對 AI 預設關閉。' WHERE tool_name = 'update_notice';

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections,submit_my_overtime_request,list_my_overtime_requests,list_my_notices,mark_my_notices_read}' WHERE code = 'staff_self_service';
