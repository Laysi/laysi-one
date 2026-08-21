-- Screen copy that still carried identifiers from inside the program.
--
-- Permission notes, integration details and MCP grant notes are all read out
-- on screen, so an English operation name in the second half of a Chinese
-- sentence makes the reader do the lookup. Every one of them has a Chinese
-- name already; this uses it.
--
-- The integration rows also described themselves by endpoint path and by
-- storage product. The reader of 「匯入與整合」 decides whether a channel is
-- available, not how it is addressed. The webhook row additionally stated a
-- development position (「還沒實作」), which says nothing the reader can act on;
-- what matters is that nothing sends a notification.

UPDATE permission
   SET note = '總覽、試算表與三張財務報表；收款與銀行核銷另立一組權限'
 WHERE code = 'view_reports';

UPDATE permission
   SET note = '草擬待確認傳票；直接開立分錄屬於調整分錄那一組權限'
 WHERE code = 'create_voucher';

UPDATE permission
   SET note = '建立應收單據並產生待過帳傳票；營業稅申報屬於稅務那一組權限'
 WHERE code = 'issue_invoice';

UPDATE permission
   SET note = '個人薪資明細與年度扣繳憑單限負責人；全公司彙總會計看得到'
 WHERE code = 'view_payroll_details';

UPDATE integration
   SET detail = '所有操作共用一組介面',
       note = '與 MCP 共用同一份操作定義'
 WHERE code = 'rest';

UPDATE integration
   SET detail = 'AI 助理可列出並呼叫已開啟的工具'
 WHERE code = 'mcp';

UPDATE integration
   SET detail = '文件與附件的檔案儲存',
       note = '文件管理的檔案確實寫入儲存區'
 WHERE code = 'objects';

UPDATE integration
   SET note = '傳票過帳與付款完成不會送出通知'
 WHERE code = 'webhook';

UPDATE integration
   SET note = '沒有辨識引擎，因此不會出現辨識結果或辨識率'
 WHERE code = 'ocr';

-- 0030 already moved the fact into `detail`; a note repeating it says the same
-- thing twice on one card.
UPDATE integration
   SET note = ''
 WHERE code = 'payroll';

UPDATE integration
   SET label = '檔案儲存'
 WHERE code = 'objects';

-- Opening balances have no importer of their own, but they are one voucher,
-- and the historical-voucher importer takes one. Saying so turns a greyed-out
-- row into something that can be done today.
UPDATE import_source
   SET note = '期初餘額請以歷史傳票 CSV 匯入一張期初分錄'
 WHERE code = 'opening';

-- An empty threshold is the state of every new installation, and it means no
-- alert rather than a broken budget screen.
UPDATE company_setting
   SET note = '執行率超過這個百分比就示警。留空就不示警。'
 WHERE code = 'budget_alert_percent';

-- MCP tool notes are listed on 「MCP 與 AI」 next to each switch. The tool name
-- is the switch's own label there, but the note is a sentence for a reader.
UPDATE mcp_tool_grant
   SET note = '直接開立待過帳傳票。AI 請改用草擬分錄。'
 WHERE tool_name = 'create_voucher';

UPDATE mcp_tool_grant
   SET note = '讀員工名冊。不含薪資；薪資明細限負責人。'
 WHERE tool_name = 'list_employees';

UPDATE mcp_tool_grant
   SET note = '讀請假單與特休額度（額度依年資由特休規則算）'
 WHERE tool_name = 'list_leave_requests';

UPDATE mcp_tool_grant
   SET note = '資產負債表，由已過帳分錄現算；會驗證資產＝負債＋權益'
 WHERE tool_name = 'get_balance_sheet';

UPDATE mcp_tool_grant
   SET note = '損益表，期間可選，由已過帳分錄現算'
 WHERE tool_name = 'get_income_statement';

UPDATE mcp_tool_grant
   SET note = '讀控制規則；AI 禁止事項依目前的工具開關現算，不是寫死的文字'
 WHERE tool_name = 'get_security_policy';
