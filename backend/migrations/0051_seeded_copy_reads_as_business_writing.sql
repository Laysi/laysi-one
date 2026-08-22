-- Laysi One: seeded copy reads as business writing
--
-- The notes on 「角色與核准」, 「MCP 與 AI」, 「基礎設定」 and 「匯入與整合」 were
-- written in speech: 「要開請想清楚」, 「只是單子」, 「錢不會因此出去」,
-- 「不是存起來的」, 「對不上就不交出內容」. The screens they sit on are read by
-- accountants and HR staff during work, so they now read as written business
-- Chinese. The facts are unchanged.
--
-- Three other things are fixed while the rows are open:
-- `list_payslips` and `list_withholding_statements` still said 老闆, which 0022
-- renamed to 負責人 everywhere else; `correct_punch` printed the internal source
-- code `manual`; and 「替本人」 becomes 「以本人身分」, matching the operations.

UPDATE mcp_tool_grant SET note = '寫入一批傳票。對 AI 關閉：復原只救得回尚未過帳的部分。' WHERE tool_name = 'commit_import';
UPDATE mcp_tool_grant SET note = '復原一批匯入，是往安全方向的操作，對 AI 開著。' WHERE tool_name = 'rollback_import';
UPDATE mcp_tool_grant SET note = '直接過帳；過帳後不能修改，預設關閉。' WHERE tool_name = 'post_voucher';
UPDATE mcp_tool_grant SET note = '讀公司與帳務設定；營業稅由法規規則庫決定。' WHERE tool_name = 'get_company_settings';
UPDATE mcp_tool_grant SET note = '讀某一期薪資彙總，並與總帳 6111 對帳。彙總不含個人金額。' WHERE tool_name = 'get_payroll_summary';
UPDATE mcp_tool_grant SET note = '讀人資總覽：在職人數、待辦事項與人資財務的銜接狀態' WHERE tool_name = 'get_hr_dashboard';
UPDATE mcp_tool_grant SET note = '讀法遵檢核表：最低工資、投保級距、勞退提繳率、特休。沒收錄的規則會照實標示為未檢查。' WHERE tool_name = 'get_hr_compliance';
UPDATE mcp_tool_grant SET note = '讀部門損益。未指定部門的分錄列為「未指定」，不分攤。' WHERE tool_name = 'get_department_pl';
UPDATE mcp_tool_grant SET note = '讀預算執行率。實際數一律現查總帳。' WHERE tool_name = 'get_budget_status';
UPDATE mcp_tool_grant SET note = '取回檔案內容並比對雜湊；雜湊不符就不回傳內容。' WHERE tool_name = 'get_document';
UPDATE mcp_tool_grant SET note = '讀控制規則；AI 禁止事項依目前的工具開關現算' WHERE tool_name = 'get_security_policy';
UPDATE mcp_tool_grant SET note = '讀到離職與異動單，含各關卡進度' WHERE tool_name = 'list_employee_movements';
UPDATE mcp_tool_grant SET note = '讀費用報支單與目前關卡' WHERE tool_name = 'list_expense_claims';
UPDATE mcp_tool_grant SET note = '逐人薪資明細。限負責人，且對 AI 關閉。' WHERE tool_name = 'list_payslips';
UPDATE mcp_tool_grant SET note = '年度扣繳憑單（逐人年所得）。限負責人，且對 AI 關閉。' WHERE tool_name = 'list_withholding_statements';
UPDATE mcp_tool_grant SET note = '開立異動單，還沒辦加保或退保。' WHERE tool_name = 'record_employee_movement';
UPDATE mcp_tool_grant SET note = '送出報支單；送單不會付款。' WHERE tool_name = 'submit_expense_claim';
UPDATE mcp_tool_grant SET note = '為一筆銀行流水找出可能相符的傳票' WHERE tool_name = 'suggest_bank_matches';
UPDATE mcp_tool_grant SET note = '以本人身分打上班或下班卡，預設關閉。' WHERE tool_name = 'punch_clock';
UPDATE mcp_tool_grant SET note = '以本人身分送出請假單，預設關閉。' WHERE tool_name = 'request_leave';
UPDATE mcp_tool_grant SET note = '以本人身分送出打卡補登申請。AI 代填出勤時間等同代寫出勤紀錄，預設關閉。' WHERE tool_name = 'submit_my_punch_correction';
UPDATE mcp_tool_grant SET note = '將異動單的某一關卡標記為完成' WHERE tool_name = 'complete_movement_step';
UPDATE mcp_tool_grant SET note = '將一筆銀行流水核銷至一張傳票' WHERE tool_name = 'match_bank_transaction';
UPDATE mcp_tool_grant SET note = '核發一組新 Token；明文只回傳一次。對 AI 預設關閉。' WHERE tool_name = 'issue_api_token';
UPDATE mcp_tool_grant SET note = '撤銷一組 Token，對 AI 開著。' WHERE tool_name = 'revoke_api_token';
UPDATE mcp_tool_grant SET note = '撤銷本人自助核發的 Token。預設關閉；畫面上的撤銷不受此限。' WHERE tool_name = 'revoke_my_api_token';
UPDATE mcp_tool_grant SET note = '改預算金額。對 AI 關閉：預算是比較基準，不應由 AI 變更。' WHERE tool_name = 'set_budget';
UPDATE mcp_tool_grant SET note = '建立年度預算版本。對 AI 關閉：預算是比較基準，不應由 AI 自行建立。' WHERE tool_name = 'create_budget_version';
UPDATE mcp_tool_grant SET note = '重設登入密碼，對 AI 關閉：交付密碼須由人執行。' WHERE tool_name = 'reset_user_password';
UPDATE mcp_tool_grant SET note = '建立登入帳號。對 AI 關閉：建立帳號是人的決定。' WHERE tool_name = 'create_user_account';
UPDATE mcp_tool_grant SET note = '補登或更正某人某天的打卡時間，來源標示為補登。不由 AI 代為決定，預設關閉。' WHERE tool_name = 'correct_punch';
UPDATE mcp_tool_grant SET note = '讀外部登入設定，對 AI 關閉：這是系統本身的設定。' WHERE tool_name = 'get_oidc_setting';
UPDATE mcp_tool_grant SET note = '試算一筆給付要扣繳的稅額與二代健保補充保費' WHERE tool_name = 'compute_withholding';
UPDATE mcp_tool_grant SET note = '試算某個投保級距的三項保費，並回報依哪一版規則' WHERE tool_name = 'compute_insurance';
UPDATE mcp_tool_grant SET note = '讀請假單與特休額度（額度依年資由特休規則計算）' WHERE tool_name = 'list_leave_requests';
UPDATE mcp_tool_grant SET note = '試算某個月的折舊，逐筆列出用的耐用年數與規則版本' WHERE tool_name = 'preview_depreciation';

UPDATE permission SET note = '草擬待確認傳票；直接開立分錄是調整分錄權限' WHERE code = 'create_voucher';
UPDATE permission SET note = '建立應收單據並產生待過帳傳票；營業稅申報是稅務權限' WHERE code = 'issue_invoice';
UPDATE permission SET note = '將應付帳單組成付款批次；這是職責分離中的實際付款權限' WHERE code = 'execute_payment';
UPDATE permission SET note = '個人薪資明細與年度扣繳憑單限負責人；全公司彙總會計亦可查看' WHERE code = 'view_payroll_details';
UPDATE permission SET note = '只回傳目前登入帳號所連結員工本人的首頁、薪資、休假、打卡、補登申請、報支進度與 MCP Token' WHERE code = 'staff_self_service';

UPDATE role SET note = '只能看本人的薪資、休假與請款' WHERE code = 'staff';
UPDATE role SET note = '執行匯款，不涉及分錄' WHERE code = 'cashier';
UPDATE role SET note = '管理帳號、權限與系統設定，不含薪資資料' WHERE code = 'admin';

UPDATE company_setting SET note = '執行率超過設定的百分比就示警；留空不示警。' WHERE code = 'budget_alert_percent';
UPDATE company_setting SET note = '打卡遲到的判定基準，格式 HH:MM。留空不判定遲到。' WHERE code = 'work_start';
UPDATE company_setting SET note = '加班時數的判定基準，格式 HH:MM。留空不計加班時數。' WHERE code = 'work_end';
UPDATE company_setting SET note = '會計年度起始月份，填 1 到 12。財務報表的預設期間依此設定。' WHERE code = 'fiscal_year';
UPDATE company_setting SET note = '上班時間之後這幾分鐘內到班都算準時，下班時間順延相同分鐘數，工作總時數不變。留空表示不採彈性工時。' WHERE code = 'flex_window_minutes';
UPDATE company_setting SET note = '記帳本位幣固定為新臺幣。' WHERE code = 'currency';

UPDATE import_source SET note = '民國年日期會自動轉換；借貸不平的傳票不會匯入' WHERE code = 'voucher';

UPDATE integration SET note = '文件管理的檔案寫入儲存區' WHERE code = 'objects';
