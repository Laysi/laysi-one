-- Taking back a leave or overtime request.
--
-- A request had one way forward, pending to approved or rejected, and none
-- back. A requester whose plans changed could only ask the approver to reject
-- it, which left a refusal on their record; an approval given in error went to
-- payroll as it stood.
--
-- Two new states, because they are two different acts. `withdrawn` is the
-- requester taking back a request nobody has decided yet, so `decided_at`
-- stays NULL. `revoked` is HR undoing an approval; the approval itself is kept
-- in the `decided_*` columns, and the `cancelled_*` columns say who undid it,
-- when and why.
--
-- The overlap constraints now name the two states that hold hours rather than
-- the one that does not, so a withdrawn or revoked request frees its hours for
-- a new one and a later state cannot be added without deciding which side it
-- falls on.

ALTER TABLE leave_request
    ADD COLUMN cancelled_by text DEFAULT ''::text NOT NULL,
    ADD COLUMN cancelled_role text DEFAULT ''::text NOT NULL,
    ADD COLUMN cancelled_at timestamp with time zone,
    ADD COLUMN cancel_reason text DEFAULT ''::text NOT NULL;

ALTER TABLE leave_request DROP CONSTRAINT leave_request_status_check;
ALTER TABLE leave_request ADD CONSTRAINT leave_request_status_check
    CHECK (status = ANY (ARRAY['pending'::text, 'approved'::text, 'rejected'::text,
                               'withdrawn'::text, 'revoked'::text]));

ALTER TABLE leave_request DROP CONSTRAINT leave_request_decided_iff_not_pending;
ALTER TABLE leave_request ADD CONSTRAINT leave_request_decided_iff_reached_approver
    CHECK ((decided_at IS NULL) = (status = ANY (ARRAY['pending'::text, 'withdrawn'::text])));
ALTER TABLE leave_request ADD CONSTRAINT leave_request_cancelled_iff_taken_back
    CHECK ((cancelled_at IS NOT NULL) = (status = ANY (ARRAY['withdrawn'::text, 'revoked'::text])));

ALTER TABLE leave_request DROP CONSTRAINT leave_request_no_overlap;
ALTER TABLE leave_request ADD CONSTRAINT leave_request_no_overlap EXCLUDE USING gist (
    employee_id WITH =,
    tsrange(from_at, to_at, '[)'::text) WITH &&
) WHERE (status = ANY (ARRAY['pending'::text, 'approved'::text]));

COMMENT ON COLUMN leave_request.cancelled_by IS
    '撤回或撤銷這張單的人；狀態為已撤回或已撤銷時才有值。';
COMMENT ON COLUMN leave_request.cancel_reason IS
    '撤回或撤銷的理由。撤銷必填，撤回選填。';

ALTER TABLE overtime_request
    ADD COLUMN cancelled_by text DEFAULT ''::text NOT NULL,
    ADD COLUMN cancelled_role text DEFAULT ''::text NOT NULL,
    ADD COLUMN cancelled_at timestamp with time zone,
    ADD COLUMN cancel_reason text DEFAULT ''::text NOT NULL;

ALTER TABLE overtime_request DROP CONSTRAINT overtime_request_status_check;
ALTER TABLE overtime_request ADD CONSTRAINT overtime_request_status_check
    CHECK (status = ANY (ARRAY['pending'::text, 'approved'::text, 'rejected'::text,
                               'withdrawn'::text, 'revoked'::text]));

ALTER TABLE overtime_request DROP CONSTRAINT overtime_request_decided_iff_not_pending;
ALTER TABLE overtime_request ADD CONSTRAINT overtime_request_decided_iff_reached_approver
    CHECK ((decided_at IS NULL) = (status = ANY (ARRAY['pending'::text, 'withdrawn'::text])));
ALTER TABLE overtime_request ADD CONSTRAINT overtime_request_cancelled_iff_taken_back
    CHECK ((cancelled_at IS NOT NULL) = (status = ANY (ARRAY['withdrawn'::text, 'revoked'::text])));

ALTER TABLE overtime_request DROP CONSTRAINT overtime_request_no_overlap;
ALTER TABLE overtime_request ADD CONSTRAINT overtime_request_no_overlap EXCLUDE USING gist (
    employee_id WITH =,
    tsrange(from_at, to_at, '[)'::text) WITH &&
) WHERE (status = ANY (ARRAY['pending'::text, 'approved'::text]));

COMMENT ON COLUMN overtime_request.cancelled_by IS
    '撤回或撤銷這張單的人；狀態為已撤回或已撤銷時才有值。';
COMMENT ON COLUMN overtime_request.cancel_reason IS
    '撤回或撤銷的理由。撤銷必填，撤回選填。';

-- Withdrawing belongs with submitting, revoking with deciding. Both rows are
-- re-stated in full: the later row wins, in the database and in the test that
-- reads these seeds.
INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_hr', '人事異動與請假', '建立與維護員工主檔，核發一般職員的登入帳號，上傳員工附件，維護工作日曆，對待簽休假單、加班申請與補登申請作成決定，撤銷已核准的休假單與加班申請，並補登打卡', '{decide_leave_request,revoke_leave_request,create_employee,update_employee,create_employee_login,upload_employee_document,correct_punch,decide_punch_correction,decide_overtime_request,revoke_overtime_request,create_notice,update_notice,set_work_calendar_day,refresh_work_calendar}', 95),
    ('staff_self_service', '使用員工自助', '只回傳目前登入帳號所連結員工本人的首頁、薪資、休假、加班、打卡、補登申請、報支進度與 MCP Token', '{get_my_home,list_my_payslips,list_my_leave_requests,preview_leave,request_leave,withdraw_my_leave_request,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections,submit_my_overtime_request,withdraw_my_overtime_request,list_my_overtime_requests,list_my_notices,mark_my_notices_read}', 96)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('withdraw_my_leave_request', false, 'needs_confirm', '撤回本人仍在等簽核的休假單。', '2026-10-07 00:00:00+00'),
    ('withdraw_my_overtime_request', false, 'needs_confirm', '撤回本人仍在等簽核的加班申請。', '2026-10-07 00:00:00+00'),
    ('revoke_leave_request', false, 'needs_confirm', '撤銷已核准、所在期別尚未計算薪資的休假單。', '2026-10-07 00:00:00+00'),
    ('revoke_overtime_request', false, 'needs_confirm', '撤銷已核准、所在期別尚未計算薪資的加班申請。', '2026-10-07 00:00:00+00')
ON CONFLICT (tool_name) DO UPDATE
    SET enabled = EXCLUDED.enabled, risk = EXCLUDED.risk,
        note = EXCLUDED.note, updated_at = EXCLUDED.updated_at;
