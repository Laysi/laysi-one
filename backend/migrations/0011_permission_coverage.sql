-- Laysi One: full operation permission coverage and the baseline role matrix
--
-- Deny by default means "no row, no permission". `role_permission` has had
-- that meaning since 0006: the absence of a row between a role and a
-- permission is not "fall back to a default" but an explicit refusal.
-- dispatch joins all of a role's rows to `permission.op_names` and refuses
-- when the operation name is not in the result. `level = limited` still
-- means the role may enter that group of operations; a department or own-
-- record scope is narrowed further by the operation's own query.
--
-- Coverage had to reach 100% before enforcement could be switched on. Before
-- 0010 the registry had 76 operations while permission.op_names covered only
-- 30 of them. Enabling deny-by-default in that state would have made the
-- other 46 permanently uncallable, even by the owner. Conversely, an
-- operation appearing in two permissions means revoking one cell still
-- leaves a way in through the other. So every operation below is assigned to
-- exactly one group; the pure string test in laysi-ops reads 0006, 0008,
-- 0009 and this migration, and fails on a missing or duplicated entry.
--
-- The grouping criterion is whether a role would want the whole group or
-- none of it. Payroll, for instance, splits into totals, per-person detail
-- and execution, so an accountant can see totals without touching individual
-- pay; requesting a payment and actually transferring money are separated
-- too, so accounting can keep the books without moving money past the
-- separation of duties.

-- Assign the operations of the existing permissions

UPDATE permission
   SET op_names = '{get_dashboard,get_trial_balance,get_balance_sheet,get_income_statement,get_cash_flow}',
       note = '總覽、試算表與三張財務報表；現金與銀行作業另由 manage_banking 管理'
 WHERE code = 'view_reports';

UPDATE permission
   SET op_names = '{draft_voucher}',
       note = '草擬待確認傳票；直接開立調整分錄另由 adjust_entry 管理'
 WHERE code = 'create_voucher';

UPDATE permission
   SET op_names = '{post_voucher}',
       note = '把待過帳傳票正式過帳；過帳後只能以反向分錄更正'
 WHERE code = 'post_voucher';

UPDATE permission
   SET op_names = '{search_vouchers,create_voucher}',
       note = '查閱既有傳票並直接開立待過帳分錄，供日常更正與年度調整'
 WHERE code = 'adjust_entry';

UPDATE permission
   SET op_names = '{list_einvoices,issue_einvoice,retry_einvoice_upload}',
       note = '查看、開立與重送電子發票；營業稅申報另由 manage_tax 管理'
 WHERE code = 'issue_invoice';

UPDATE permission
   SET op_names = '{submit_expense_claim}',
       note = '送出費用報支申請；只建立待簽單據，不代表已核准或已付款'
 WHERE code = 'request_payment';

UPDATE permission
   SET op_names = '{create_payment_batch}',
       note = '把應付帳單組成付款批次；這是職責分離中的實際付款權限'
 WHERE code = 'execute_payment';

UPDATE permission
   SET op_names = '{list_expense_claims,approve_expense_claim,reject_expense_claim,list_approval_rules}',
       note = '查看簽核規則與報支單，並依目前關卡核准或退回'
 WHERE code = 'approve_expense';

UPDATE permission
   SET op_names = '{get_payroll_summary}',
       note = '只看薪資彙總與總帳對帳，不包含任何個人薪資金額'
 WHERE code = 'view_payroll';

UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token}',
       note = '修改公司設定與角色權限，並簽發或撤銷 API／MCP Token'
 WHERE code = 'manage_access';

UPDATE permission
   SET op_names = '{list_employee_movements,record_employee_movement,complete_movement_step,decide_leave_request}',
       note = '處理到離職與人事異動流程，並對待簽請假單作成決定'
 WHERE code = 'manage_hr';

UPDATE permission
   SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims}',
       note = '只回目前登入帳號所連結員工本人的首頁、薪資、請假與報支進度'
 WHERE code = 'staff_self_service';

-- New permissions that can be granted or revoked as a group

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('view_masters', '看基礎主檔',
     '會計科目、報表分組、維度與公司設定的唯讀入口',
     '{list_accounts,get_company_settings,list_chart_of_accounts,list_dimensions}', 15),
    ('view_subledgers', '看應收應付明細',
     '查看應收帳齡、應收明細與待付帳單，不會產生收付款分錄',
     '{get_ar_aging,list_receivables,list_payables}', 45),
    ('manage_banking', '收款與銀行核銷',
     '查看現金與銀行流水、登記收款、取得建議並完成人工核銷',
     '{get_cash_position,record_receipt,list_bank_transactions,suggest_bank_matches,match_bank_transaction}', 75),
    ('manage_tax', '稅務與法規試算',
     '查看適用法規、營業稅申報與各類所得扣繳明細及試算',
     '{list_regulations,get_vat_return,list_withholding,compute_withholding}', 55),
    ('manage_assets', '固定資產與折舊',
     '查看資產、預覽折舊並產生一期待過帳折舊傳票',
     '{list_fixed_assets,preview_depreciation,run_depreciation}', 65),
    ('view_department', '看部門預算與損益',
     '查看預算執行率與部門損益；未指定部門的分錄仍會列出',
     '{get_budget_status,get_department_pl}', 81),
    ('manage_budget', '設定預算',
     '設定年度與部門預算；實際數仍一律由已過帳分錄現算',
     '{set_budget}', 82),
    ('manage_documents', '管理文件',
     '查看、上傳與驗證 RustFS 中的文件版本與內容雜湊',
     '{list_documents,upload_document,get_document}', 85),
    ('run_imports', '執行匯入',
     '查看匯入來源、預覽、提交批次與回滾尚未過帳的批次',
     '{list_imports,preview_import,commit_import,rollback_import}', 86),
    ('view_hr', '看人資資料',
     '人資總覽、名冊、出勤請假、投保摘要、保費試算與法遵檢核',
     '{get_hr_dashboard,list_employees,get_attendance,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance}', 91),
    ('view_payroll_details', '看個人薪資明細',
     '查看全公司逐人薪資單與年度逐人扣繳憑單，屬高度敏感資料',
     '{list_payslips,list_withholding_statements}', 92),
    ('manage_payroll', '執行薪資作業',
     '計算一期薪資並產生待過帳薪資傳票，不含正式過帳',
     '{run_payroll}', 93),
    ('view_audit', '看稽核軌跡',
     '查看每次操作的呼叫者、參數摘要、結果與被拒絕紀錄',
     '{list_ai_audit}', 97),
    ('view_system', '看安全與系統狀態',
     '查看角色矩陣、安全政策、Token 後設資料與系統健康狀態',
     '{list_roles,get_security_policy,list_api_tokens,get_system_health}', 98);

COMMENT ON COLUMN permission.op_names IS
    '對應 Registry 的操作名稱；dispatch 每次呼叫都即時查詢，名稱不在角色集合中就拒絕。';

-- Baseline role matrix
--
-- Only cells that clearly conflict with the new grouping are removed from
-- the old matrix; cells an administrator added after deployment are not
-- silently erased by a migration. Staff are the exception: by product rule
-- they may hold only staff_self_service.

DELETE FROM role_permission
 WHERE (role_code, permission_code) IN (
    ('cashier', 'view_reports'),
    ('cashier', 'issue_invoice'),
    ('manager', 'view_reports'),
    ('manager', 'manage_hr'),
    ('accountant', 'create_voucher')
 );

DELETE FROM role_permission
 WHERE role_code = 'staff'
   AND permission_code <> 'staff_self_service';

-- The owner is ultimately accountable for the data and the controls, so
-- every permission is granted at full today. New permissions still have to
-- be granted explicitly by their own migration; no implicit wildcard
-- applies.
INSERT INTO role_permission (role_code, permission_code, level, note, updated_by)
SELECT 'boss', code, 'full',
       '老闆是公司資料與控制的最終負責人，因此保留這組操作的完整權限。',
       'migration 0011'
  FROM permission
 WHERE true
ON CONFLICT (role_code, permission_code) DO UPDATE
SET level = EXCLUDED.level,
    note = EXCLUDED.note,
    updated_at = now(),
    updated_by = EXCLUDED.updated_by;

-- Accounting handles day-to-day books, statements, invoices, tax, assets,
-- documents and part of HR. On payments it goes as far as requesting and
-- bookkeeping, deliberately without execute_payment, so the person who
-- creates the entry cannot also send the money.
INSERT INTO role_permission (role_code, permission_code, level, note, updated_by) VALUES
    ('acct','view_reports','full','會計要用總覽、試算表與三張報表完成日常帳務。','migration 0011'),
    ('acct','view_masters','full','會計開帳與選科目前必須能查基礎主檔。','migration 0011'),
    ('acct','view_subledgers','full','會計要用應收應付明細與總帳對帳。','migration 0011'),
    ('acct','create_voucher','full','會計可以建立待確認的傳票草稿。','migration 0011'),
    ('acct','adjust_entry','limited','會計可登打日常分錄及事務所提出的調整。','migration 0011'),
    ('acct','post_voucher','full','會計負責把已確認傳票正式過帳。','migration 0011'),
    ('acct','issue_invoice','full','會計負責電子發票的查看、開立與重送。','migration 0011'),
    ('acct','request_payment','full','會計可以送出報支申請，但這不包含實際匯款。','migration 0011'),
    ('acct','approve_expense','full','報支流程需要會計確認科目並處理目前關卡。','migration 0011'),
    ('acct','manage_banking','full','會計負責收款登記與銀行對帳，但不能建立付款批次。','migration 0011'),
    ('acct','manage_tax','full','會計負責營業稅與扣繳作業。','migration 0011'),
    ('acct','manage_assets','full','會計負責資產台帳與每期折舊。','migration 0011'),
    ('acct','view_department','full','會計需要部門預算與損益來完成管理報表。','migration 0011'),
    ('acct','manage_documents','full','會計負責保存與驗證傳票、發票及報支憑證。','migration 0011'),
    ('acct','run_imports','full','會計可以預覽、提交，必要時回滾帳務匯入。','migration 0011'),
    ('acct','view_hr','full','中小企業的人資資料由會計兼辦，需查看名冊、出勤與投保資料。','migration 0011'),
    ('acct','manage_hr','full','會計兼辦到離職、人事異動與請假流程。','migration 0011'),
    ('acct','view_payroll','full','會計需要薪資彙總與總帳對帳。','migration 0011'),
    ('acct','view_payroll_details','full','會計執行薪資作業時需要核對逐人明細。','migration 0011'),
    ('acct','manage_payroll','full','會計負責計算薪資並產生待過帳傳票。','migration 0011'),
    ('acct','view_audit','full','會計需要追查帳務操作與被拒絕紀錄。','migration 0011'),
    ('acct','staff_self_service','full','會計若有員工主檔，也只能從自助操作查看本人資料。','migration 0011')
ON CONFLICT (role_code, permission_code) DO UPDATE
SET level = EXCLUDED.level,
    note = EXCLUDED.note,
    updated_at = now(),
    updated_by = EXCLUDED.updated_by;

-- The cashier handles settlement and banking only, and receives no
-- permission to create, adjust or post journal entries.
INSERT INTO role_permission (role_code, permission_code, level, note, updated_by) VALUES
    ('cashier','view_subledgers','limited','出納只需查看收款與付款所需的應收應付明細。','migration 0011'),
    ('cashier','request_payment','full','出納可以送出自己的報支申請，尚不會讓錢匯出。','migration 0011'),
    ('cashier','execute_payment','full','實際建立付款批次是出納的核心職責。','migration 0011'),
    ('cashier','manage_banking','full','出納負責收款、銀行流水與核銷。','migration 0011'),
    ('cashier','staff_self_service','full','出納以員工身分只能查看與申請自己的資料。','migration 0011')
ON CONFLICT (role_code, permission_code) DO UPDATE
SET level = EXCLUDED.level,
    note = EXCLUDED.note,
    updated_at = now(),
    updated_by = EXCLUDED.updated_by;

-- A manager approves their department's claims, sees department data, and
-- keeps their own self-service.
INSERT INTO role_permission (role_code, permission_code, level, note, updated_by) VALUES
    ('manager','request_payment','full','主管可以送出自己的報支申請。','migration 0011'),
    ('manager','approve_expense','limited','主管只在流程輪到主管時簽核部門報支。','migration 0011'),
    ('manager','view_department','limited','主管只需要部門預算與損益資料。','migration 0011'),
    ('manager','staff_self_service','full','主管以員工身分只能查看與申請自己的資料。','migration 0011')
ON CONFLICT (role_code, permission_code) DO UPDATE
SET level = EXCLUDED.level,
    note = EXCLUDED.note,
    updated_at = now(),
    updated_by = EXCLUDED.updated_by;

-- The external accountant handles adjusting entries, financial and tax
-- reporting and the annual close. On payroll they receive only the totals
-- permission and deliberately not view_payroll_details, so no individual's
-- payslip or withholding statement is visible.
INSERT INTO role_permission (role_code, permission_code, level, note, updated_by) VALUES
    ('accountant','view_reports','full','會計師需要完整財務報表完成查核與結算。','migration 0011'),
    ('accountant','view_masters','full','會計師需要科目與維度定義判讀報表。','migration 0011'),
    ('accountant','view_subledgers','full','會計師需要應收應付明細核對總帳。','migration 0011'),
    ('accountant','adjust_entry','full','事務所端可以查傳票並建立年度調整分錄。','migration 0011'),
    ('accountant','post_voucher','full','年度調整確認後由會計師完成結算過帳。','migration 0011'),
    ('accountant','manage_tax','full','會計師負責營業稅、扣繳與適用法規查核。','migration 0011'),
    ('accountant','manage_assets','full','年度結算需要核對資產與折舊提列。','migration 0011'),
    ('accountant','view_payroll','limited','只看薪資彙總與總帳差額，不取得任何個人薪資。','migration 0011'),
    ('accountant','view_audit','full','查核需要追溯操作與被拒絕的完整稽核軌跡。','migration 0011')
ON CONFLICT (role_code, permission_code) DO UPDATE
SET level = EXCLUDED.level,
    note = EXCLUDED.note,
    updated_at = now(),
    updated_by = EXCLUDED.updated_by;

INSERT INTO role_permission (role_code, permission_code, level, note, updated_by) VALUES
    ('staff','staff_self_service','full','一般職員只有本人首頁、薪資單、請假與報支進度等自助操作。','migration 0011')
ON CONFLICT (role_code, permission_code) DO UPDATE
SET level = EXCLUDED.level,
    note = EXCLUDED.note,
    updated_at = now(),
    updated_by = EXCLUDED.updated_by;

-- With dispatch live, separation of duties is no longer only a matrix on
-- screen: accounting lacks execute_payment and the cashier lacks the
-- journal permissions, and both are stopped before execution with a blocked
-- audit entry.
UPDATE security_policy
   SET detail = 'dispatch 每次依 role_permission 即時檢查；會計沒有實際匯款權限，出納沒有開立、調整或過帳分錄權限。',
       enforced = true
 WHERE code = 'separation_of_duties';
