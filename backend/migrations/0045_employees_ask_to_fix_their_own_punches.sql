-- Laysi One: an employee asks to fix their own punch
--
-- 0016 gave HR `correct_punch` and gave the employee nothing. The person who
-- knows they forgot to press the button is the person who cannot record it,
-- so every missed punch became an errand: tell HR, HR types it in. On a
-- fifteen-person company that is the most common attendance event there is.
--
-- The request is a separate row rather than a punch with a status column.
-- `attendance_punch` is the evidence of the working day and everything that
-- reads it -- worked hours, lateness, overtime -- must keep reading rows that
-- are all equally true. A proposed time is not evidence until somebody with
-- the authority to say so accepts it, and on acceptance the punch is written
-- through the same `record_punch` path HR already uses, with `source =
-- 'manual'` and the deciding person as `created_by`.
--
-- `correct_punch` stays. Not everyone on the payroll has a login: 0028 allows
-- an employee master record with nothing but a name and a start date, and
-- that person still misses punches.

CREATE TABLE punch_correction (
    id          serial PRIMARY KEY,
    employee_id int  NOT NULL REFERENCES employee(id) ON DELETE CASCADE,
    -- The working day and slot the request is about, in local terms.
    punched_on  date NOT NULL,
    kind        text NOT NULL CHECK (kind IN ('in','out')),
    -- The time the employee says it was.
    punched_at  timestamptz NOT NULL,
    reason      text NOT NULL DEFAULT '',
    status      text NOT NULL CHECK (status IN ('submitted','approved','rejected')),
    decided_by   text NOT NULL DEFAULT '',
    decided_role text NOT NULL DEFAULT '',
    decided_at   timestamptz,
    decision_note text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL,
    -- Same constant and same reason as attendance_punch: the date and the
    -- timestamp must agree in the business time zone, and `AT TIME ZONE
    -- 'Asia/Taipei'` is only STABLE so it cannot appear in a CHECK.
    CONSTRAINT punch_correction_day_matches_time
        CHECK (punched_on = (punched_at AT TIME ZONE INTERVAL '+08:00')::date),
    CONSTRAINT punch_correction_decided_iff_not_submitted
        CHECK ((status = 'submitted') = (decided_at IS NULL))
);

-- One open request per slot. Without this an employee who submits twice
-- leaves HR two rows proposing different times for the same box, and
-- approving both writes one over the other with no record of which was meant.
-- Decided rows are not covered, so the history of a slot is kept.
CREATE UNIQUE INDEX punch_correction_one_open_per_slot
    ON punch_correction (employee_id, punched_on, kind)
    WHERE status = 'submitted';

CREATE INDEX punch_correction_open ON punch_correction (status, punched_on);

COMMENT ON TABLE punch_correction IS
    '員工送出的打卡補登申請。核准後才寫進 attendance_punch，來源記為補登。';

-- Whether the company reviews these at all
--
-- Two answers, and the value is the one the screen prints. A company that
-- trusts its people to fix their own missed punches should not have to route
-- fifteen requests a month through a person who was not there either; a
-- company that wants the second pair of eyes keeps them. It is a company
-- policy value, so it sits with the working hours it belongs beside.

INSERT INTO company_setting (code, label, value, note, editable, sort_order) VALUES
    ('punch_correction_policy', '補登申請', '人資審核',
     '員工送出補登申請後的處理方式：人資審核，或自動通過。', true, 110);

-- Permissions
--
-- Submitting and reading are self-service by the group's rule: neither input
-- has a field naming a person, so the subject can only be ctx.employee_id.
-- Reading everyone's requests is HR reading; deciding one is an HR decision,
-- beside `decide_leave_request` and `correct_punch`.

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections}' WHERE code = 'staff_self_service';

UPDATE permission SET op_names = '{get_hr_dashboard,list_employees,get_attendance,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches,list_punch_corrections}' WHERE code = 'view_hr';

UPDATE permission SET op_names = '{list_employee_movements,record_employee_movement,complete_movement_step,decide_leave_request,create_employee,update_employee,correct_punch,decide_punch_correction}' WHERE code = 'manage_hr';

UPDATE permission SET note = '建立與維護員工主檔，處理到離職與人事異動流程，對待簽休假單與補登申請作成決定，並補登打卡' WHERE code = 'manage_hr';

UPDATE permission SET note = '只回目前登入帳號所連結員工本人的首頁、薪資、休假、打卡、補登申請、報支進度與 MCP Token' WHERE code = 'staff_self_service';

-- MCP tool grants
--
-- Submitting is closed for the same reason `punch_clock` is: a tool that
-- proposes attendance times is a tool for writing an attendance record the
-- token holder may not be the subject of. Deciding is closed because it is a
-- decision. Reading the company's open requests is ordinary HR reading, like
-- `list_attendance_punches`.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('submit_my_punch_correction', false, 'needs_confirm',
     '替本人送出打卡補登申請。AI 代提出勤時間等同代寫出勤紀錄，預設關閉。'),
    ('list_my_punch_corrections', false, 'read_only',
     '只讀本人的補登申請。個人出勤不預設交給 AI，預設關閉。'),
    ('list_punch_corrections', true, 'read_only',
     '讀全公司的打卡補登申請與目前狀態。'),
    ('decide_punch_correction', false, 'needs_confirm',
     '核准或駁回一張補登申請，核准即寫入打卡紀錄。不由 AI 代為決定，預設關閉。')
ON CONFLICT (tool_name) DO NOTHING;
