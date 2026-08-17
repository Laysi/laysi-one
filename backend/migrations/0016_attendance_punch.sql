-- Laysi One: clock-in and clock-out
--
-- `attendance` holds one aggregated row per person per month, imported from a
-- timekeeping system. Until now nothing produced those rows: the product had
-- somewhere to put attendance and no way to record any. This migration adds
-- the raw event the aggregate is supposed to summarise.
--
-- The two do not overwrite each other. Punches are what happened; the
-- `attendance` row is what a source system says happened. The operations
-- derive worked days, late days and overtime hours from the punches and
-- report the difference against the imported row, the same way payroll
-- reconciles `payroll_year_totals` against `employee_income` rather than
-- letting one silently win.
--
-- Work hours are company policy, not regulation, so they live in
-- `company_setting` beside the other policy values — never in Rust. They ship
-- empty: a fresh install records punches from the first day and simply does
-- not judge lateness until someone states when the working day begins. An
-- assumed 09:00 would be a regulation-shaped guess about somebody else's
-- company.
--
-- Overtime stays hours, never money. The multipliers of Labor Standards Act
-- article 24 are regulation values and are still not in
-- `backend/regulation/`, so this migration does not enable a calculation that
-- 0008 deliberately refused.

CREATE TABLE attendance_punch (
    id          serial PRIMARY KEY,
    employee_id int  NOT NULL REFERENCES employee(id) ON DELETE CASCADE,
    -- The working day this punch belongs to, in local terms.
    punched_on  date NOT NULL,
    punched_at  timestamptz NOT NULL,
    kind        text NOT NULL CHECK (kind IN ('in','out')),
    -- 'self' is the employee pressing the button; 'manual' is HR entering a
    -- punch that was missed. Which one it was must survive, because a day
    -- entirely filled in by somebody else is not the same evidence.
    source      text NOT NULL CHECK (source IN ('self','manual')),
    note        text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL,
    -- One arrival and one departure per day. A second press is a mistake, not
    -- a second working day, and the operation says so rather than silently
    -- adding a row.
    UNIQUE (employee_id, punched_on, kind),
    -- The date and the timestamp must agree, in the business time zone rather
    -- than the server's. A fixed +08:00 is used because `AT TIME ZONE
    -- 'Asia/Taipei'` is only STABLE and cannot appear in a CHECK; Taiwan has
    -- had no daylight saving since 1979, so the two are the same thing here.
    -- The same constant is `laysi_domain::BUSINESS_UTC_OFFSET_HOURS`.
    CONSTRAINT attendance_punch_day_matches_time
        CHECK (punched_on = (punched_at AT TIME ZONE INTERVAL '+08:00')::date)
);

CREATE INDEX attendance_punch_by_day ON attendance_punch (punched_on, employee_id);

COMMENT ON TABLE attendance_punch IS
    '打卡原始紀錄。一天一組上下班，時數與遲到由此推導，不覆蓋 attendance 匯入的月統計。';

-- Working hours as company policy
--
-- Empty on purpose. `list_my_punches` and `list_attendance_punches` state
-- that lateness is not being judged while `work_start` is blank, instead of
-- picking a start time on the company's behalf.

INSERT INTO company_setting (code, label, value, note, editable, sort_order) VALUES
    ('work_start', '上班時間', '',
     '打卡遲到的判定基準，格式 HH:MM。留空時系統只記錄時間，不判定遲到。', true, 80),
    ('work_end', '下班時間', '',
     '加班時數的判定基準，格式 HH:MM。留空時系統只記錄時間，不計加班時數。', true, 90),
    ('late_grace_minutes', '遲到寬限分鐘', '',
     '上班時間之後幾分鐘內不計遲到。留空視為 0，畫面會標示目前採用的值。', true, 100);

-- Permissions
--
-- Clocking in is self-service by the same rule as the rest of that group: the
-- input has no field naming a person, so the subject can only be
-- ctx.employee_id. Reading everyone's punches is HR reading, and correcting
-- someone else's punch is an HR decision.

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,punch_clock,list_my_punches}' WHERE code = 'staff_self_service';

UPDATE permission SET op_names = '{get_hr_dashboard,list_employees,get_attendance,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches}' WHERE code = 'view_hr';

UPDATE permission SET op_names = '{list_employee_movements,record_employee_movement,complete_movement_step,decide_leave_request,create_employee,update_employee,correct_punch}' WHERE code = 'manage_hr';

-- MCP tool grants
--
-- `punch_clock` is false and should stay false. A tool that lets an assistant
-- clock somebody in is a tool for falsifying an attendance record, and the
-- token holder is not necessarily the person the record is about.
-- `correct_punch` is false for the same reason from the other direction.
-- Reading the company's monthly punch summary is ordinary HR reading, like
-- `get_attendance`, so it is on.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('punch_clock', false, 'needs_confirm',
     '替 ctx.employee_id 對應的員工打上班或下班卡。AI 代打卡等同偽造出勤紀錄，預設關閉。'),
    ('list_my_punches', false, 'read_only',
     '只讀本人的打卡明細與當月統計。個人出勤不預設交給 AI，預設關閉。'),
    ('list_attendance_punches', true, 'read_only',
     '讀某個月全公司的打卡明細與統計，並與匯入的 attendance 月統計對帳。'),
    ('correct_punch', false, 'needs_confirm',
     '補登或更正某人某天的打卡時間，來源標記為 manual。不由 AI 代為決定，預設關閉。')
ON CONFLICT (tool_name) DO NOTHING;

-- Wording: 請假 became 休假 across the interface, and these two notes are
-- shown on screen rather than only living in the schema.

UPDATE role SET note = '只看得到自己的薪資、休假與請款' WHERE code = 'staff';

UPDATE permission SET note = '建立與維護員工主檔，處理到離職與人事異動流程，對待簽休假單作成決定，並補登打卡' WHERE code = 'manage_hr';

UPDATE permission SET note = '只回目前登入帳號所連結員工本人的首頁、薪資、休假、打卡與報支進度' WHERE code = 'staff_self_service';

UPDATE permission SET note = '人資總覽、名冊、出勤休假、打卡、投保摘要、保費試算與法遵檢核' WHERE code = 'view_hr';
