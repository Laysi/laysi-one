-- Working out a leave range before it is sent.
--
-- The leave screen used to compute the hours itself, without the break and
-- with every Saturday and Sunday off whatever the work calendar said, so the
-- number shown before sending was not the number the request stored.
-- `preview_leave` returns the stored number instead. It joins the permission
-- that already holds `request_leave`, and reads nothing personal, so AI may
-- call it the same as `list_my_leave_requests`.

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('staff_self_service', '使用員工自助', '只回傳目前登入帳號所連結員工本人的首頁、薪資、休假、打卡、補登申請、報支進度與 MCP Token', '{get_my_home,list_my_payslips,list_my_leave_requests,preview_leave,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections,submit_my_overtime_request,list_my_overtime_requests,list_my_notices,mark_my_notices_read}', 96)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('preview_leave', true, 'read_only', '試算一段休假區間的時數與天數。', '2026-09-23 00:00:00+00')
ON CONFLICT (tool_name) DO NOTHING;
