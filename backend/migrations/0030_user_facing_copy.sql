-- Seeded notes and details are rendered on 「基礎設定」, 「安全與營運」,
-- 「角色與核准」, 「MCP 與 AI」 and 「匯入與整合」, so they are UI copy and
-- follow the same rules as the screens. This continues 0022 and 0024.
--
-- Three problems are fixed here.
--
-- Implementation progress reached the screen: 「尚未實作」, 「尚未接上」,
-- 「防竄改機制尚未加上」, 「還沒有主檔資料表」. A user buys working features,
-- not a roadmap, so the copy states what the screen can do today instead.
--
-- Rationale was written as copy: 「由結帳狀態與目前會計期間決定」,
-- 「由系統的單據編號器產生」, 「公司政策，不是法規」. Rationale belongs in
-- docs/architecture.md.
--
-- 「維度」 is a BI and mainland ERP term. The dimension tables keep their
-- English names; the Chinese copy now says 分類 / 類別 / 項目.

UPDATE company_setting SET note = '' WHERE code IN ('tax_id', 'period', 'doc_number');

UPDATE company_setting
   SET note = '執行率超過這個百分比就示警。'
 WHERE code = 'budget_alert_percent';

UPDATE company_setting
   SET note = '打卡遲到的判定基準，格式 HH:MM。留空就不判定遲到。'
 WHERE code = 'work_start';

UPDATE company_setting
   SET note = '加班時數的判定基準，格式 HH:MM。留空就不計加班時數。'
 WHERE code = 'work_end';

UPDATE company_setting
   SET note = '上班時間之後幾分鐘內不計遲到。留空視為 0。'
 WHERE code = 'late_grace_minutes';

UPDATE dimension SET note = '' WHERE code IN ('department', 'project', 'cost_center');

UPDATE dimension SET note = '決定應收應付的到期日' WHERE code = 'payment_term';

UPDATE security_policy
   SET detail = '登入方式為密碼或 OIDC。'
 WHERE code = 'two_factor';

UPDATE security_policy
   SET detail = '每一次呼叫都寫入稽核紀錄，唯讀與被擋下的呼叫同樣寫入。'
 WHERE code = 'audit_retention';

UPDATE integration
   SET detail = '文件的辨識狀態一律為待處理。'
 WHERE code = 'ocr';

UPDATE integration
   SET note = '在「安全與營運」核發與撤銷 Token。'
 WHERE code = 'mcp';

UPDATE import_source SET note = '可直接使用系統提供的基礎科目表' WHERE code = 'coa';
UPDATE import_source SET note = '' WHERE code IN ('opening', 'partner', 'bank', 'input_tax');
UPDATE import_source
   SET note = '民國年日期會自動轉換，借貸不平會被擋下'
 WHERE code = 'voucher';

UPDATE permission
   SET note = '會計科目、分類與公司設定的唯讀入口；科目餘額屬於報表權限'
 WHERE code = 'view_masters';

UPDATE permission
   SET note = '新增、修改或停用科目，建立銀行帳戶與分類項目，並設定費用簽核流程'
 WHERE code = 'manage_masters';

UPDATE mcp_tool_grant
   SET note = '新增分類項目，對 AI 預設關閉。'
 WHERE tool_name = 'create_dimension_value';
