-- Employee birthday, personnel attachments, and an HR-issued staff login.
--
-- 0001 is applied and frozen, so everything here is additive.

ALTER TABLE employee ADD COLUMN birthday date;
COMMENT ON COLUMN employee.birthday IS
    '出生年月日。勞保申報與扣繳憑單要用；還沒收到可以留空。';

-- Personnel scans live in the same two tables as every other file, so they
-- inherit the object store, the revisions and the BLAKE3 check. They are
-- reached only through the employee-scoped operations: manage_documents opens
-- every finance document, and that is not a set the HR role should hold.
ALTER TABLE document DROP CONSTRAINT document_kind_check;
ALTER TABLE document ADD CONSTRAINT document_kind_check
    CHECK (kind = ANY (ARRAY['receipt'::text, 'vendor_bill'::text, 'expense_receipt'::text,
                             'bank'::text, 'contract'::text, 'closing'::text,
                             'id_document'::text, 'personnel'::text, 'other'::text]));
ALTER TABLE document DROP CONSTRAINT document_link_type_kinds;
ALTER TABLE document ADD CONSTRAINT document_link_type_kinds
    CHECK (link_type = ANY (ARRAY[''::text, 'voucher'::text, 'ap_bill'::text, 'ar_invoice'::text,
                                  'expense_claim'::text, 'bank_account'::text, 'import_batch'::text,
                                  'period'::text, 'employee'::text, 'other'::text]));

-- The two HR permissions are re-stated in full: the later row wins, in the
-- database and in the test that reads these seeds.
INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('view_hr', '看人資資料', '人資總覽、名冊、出勤休假、打卡、投保摘要、保費試算、法遵檢核與員工附件', '{get_hr_dashboard,list_employees,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches,list_punch_corrections,list_overtime_requests,list_notices,list_work_calendar,list_employee_documents,get_employee_document}', 91),
    ('manage_hr', '人事異動與請假', '建立與維護員工主檔，核發一般職員的登入帳號，上傳員工附件，維護工作日曆，對待簽休假單與補登申請作成決定，並補登打卡', '{decide_leave_request,create_employee,update_employee,create_employee_login,upload_employee_document,correct_punch,decide_punch_correction,decide_overtime_request,create_notice,update_notice,set_work_calendar_day,refresh_work_calendar}', 95)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('create_employee_login', false, 'needs_confirm', '為名冊上的人核發一般職員登入帳號，AI 不得自行核發。', '2026-08-31 00:00:00+00'),
    ('list_employee_documents', false, 'read_only', '讀某位員工的附件清單，含證件掃描檔的檔名與雜湊。', '2026-08-31 00:00:00+00'),
    ('get_employee_document', false, 'read_only', '取回員工附件內容，證件掃描檔會整份離開本系統。', '2026-08-31 00:00:00+00'),
    ('upload_employee_document', false, 'needs_confirm', '上傳一份員工附件到物件儲存，存下 BLAKE3 雜湊。', '2026-08-31 00:00:00+00')
ON CONFLICT (tool_name) DO NOTHING;
