-- Laysi One: master data operations, AI tool grants and human permissions
--
-- Master data changes and day-to-day documents are not the same risk.
-- Accounts, employees, bank accounts, dimension values and approval flows
-- change how the whole system classifies, authorises and books from then on;
-- even though the operations still require a person to confirm in the UI, AI
-- is not allowed to create or modify them by default. Receivable invoices
-- and payable bills are transactions that only produce unposted vouchers,
-- which still need a person to post before they reach the ledger, so they
-- are open to AI by default.
--
-- `mcp_tool_grant` governs AI only; what a person may do in the UI is still
-- decided by `role_permission` and `permission.op_names`. They are kept
-- separate so that an AI switch being off never stops accounting from
-- maintaining master data.

-- Account active flag
--
-- An account with entries cannot be deleted, or historical books would lose
-- the classification their foreign key points at. Deactivating only
-- prevents future selection while existing entries and statements stay
-- intact.

ALTER TABLE account
    ADD COLUMN is_active boolean NOT NULL DEFAULT true;

COMMENT ON COLUMN account.is_active IS
    'false 代表不可再用於新分錄；既有分錄與報表仍保留，科目不提供刪除。';

-- MCP tool grants

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('create_account',         false, 'needs_confirm',
     '新增科目會改變三張財務報表的分類結構，AI 預設不得自行擴充科目表。'),
    ('update_account',         false, 'needs_confirm',
     '修改或停用科目會影響後續入帳與報表分類，AI 預設關閉。'),
    ('create_employee',        false, 'needs_confirm',
     '員工主檔含薪資、投保申報事實與遮罩個資，AI 預設不得自行建立。'),
    ('update_employee',        false, 'needs_confirm',
     '員工狀態、薪資與投保申報事實屬敏感主檔，AI 預設關閉。'),
    ('create_bank_account',    false, 'needs_confirm',
     '銀行帳戶會綁定總帳科目並成為收付款入口，AI 預設不得自行建立。'),
    ('create_dimension_value', false, 'needs_confirm',
     '維度值會成為部門分錄與管理報表的分類依據，AI 預設不得自行增加。'),
    ('set_approval_rule',      false, 'boss_only',
     '簽核關卡與金額門檻是公司政策；AI 不得自行改寫約束自己的流程。'),
    ('create_ap_bill',         true,  'needs_confirm',
     '建立應付帳單與待過帳傳票；屬日常單據，仍須人工過帳才進正式總帳。'),
    ('create_ar_invoice',      true,  'needs_confirm',
     '建立非電子發票應收與待過帳傳票；屬日常單據，仍須人工過帳才進正式總帳。');

-- Human permissions
--
-- Master data maintenance is its own group, so write access is not folded
-- into view_masters and the cashier does not gain the ability to edit the
-- chart of accounts by virtue of reconciling bank transactions. Only the
-- owner and internal accounting receive this group; the external accountant
-- can still read through view_masters but cannot change the company's
-- structure or policy.

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_masters', '維護基礎主檔',
     '新增、修改或停用科目，建立銀行帳戶與維度值，並設定費用簽核流程',
     '{create_account,update_account,create_bank_account,create_dimension_value,set_approval_rule}',
     16);

UPDATE permission
   SET op_names = '{list_employee_movements,record_employee_movement,complete_movement_step,decide_leave_request,create_employee,update_employee}',
       note = '建立與維護員工主檔，處理到離職與人事異動流程，並對待簽請假單作成決定'
 WHERE code = 'manage_hr';

UPDATE permission
   SET op_names = '{list_einvoices,issue_einvoice,retry_einvoice_upload,create_ar_invoice}',
       note = '查看與開立電子發票，也可建立不需電子發票的應收單據；營業稅申報另由 manage_tax 管理'
 WHERE code = 'issue_invoice';

UPDATE permission
   SET op_names = '{submit_expense_claim,create_ap_bill}',
       note = '送出費用報支或建立待過帳應付帳單；兩者都不包含實際匯款'
 WHERE code = 'request_payment';

INSERT INTO role_permission (role_code, permission_code, level, note, updated_by) VALUES
    ('boss', 'manage_masters', 'full',
     '老闆是公司主檔與簽核政策的最終負責人。',
     'migration 0012'),
    ('acct', 'manage_masters', 'full',
     '內部會計負責維護入帳、報表與收付款所需的基礎主檔。',
     'migration 0012');
