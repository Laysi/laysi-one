-- The working year was Monday to Friday and nothing else.
--
-- Saturday and Sunday were the whole of the week off and public holidays did
-- not exist, so leave spanning 春節 charged every one of those days against
-- the employee's entitlement, and a 補行上班 Saturday counted as a day off.
-- Overtime derived from punches had the same hole in it: attendance on a
-- non-working day had the length of a normal day subtracted from it before it
-- counted as overtime.
--
-- The table holds only the days that differ from that default (Mon-Fri work,
-- Saturday a rest day, Sunday a regular leave day), which is exactly what the
-- government calendar publishes: 國定假日, 調整放假 and 補行上班. About twenty
-- rows a year, so `WorkSchedule` reads the whole of it.
--
-- `source` keeps the two apart. Fetching the published file writes and
-- rewrites 'government' rows and leaves 'company' rows alone: a day the
-- company set by hand is a decision, and re-reading the official file must
-- not undo it.
--
-- 'natural_disaster' is 天災停班, which the published calendar never carries:
-- it is announced the evening before, per county. It is not a public holiday
-- either -- the Labor Standards Act grants it none of the same things -- so it
-- is its own kind rather than a holiday with a different name.

CREATE TABLE work_calendar (
    on_date date NOT NULL,
    kind text NOT NULL,
    name text DEFAULT ''::text NOT NULL,
    source text DEFAULT 'company'::text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT work_calendar_pkey PRIMARY KEY (on_date),
    CONSTRAINT work_calendar_kind_check CHECK ((kind = ANY (ARRAY['workday'::text, 'rest_day'::text, 'regular_leave'::text, 'holiday'::text, 'natural_disaster'::text]))),
    CONSTRAINT work_calendar_source_check CHECK ((source = ANY (ARRAY['government'::text, 'company'::text])))
);

UPDATE permission
    SET op_names = '{get_hr_dashboard,list_employees,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches,list_punch_corrections,list_overtime_requests,list_notices,list_work_calendar}'
    WHERE code = 'view_hr';

UPDATE permission
    SET op_names = '{decide_leave_request,create_employee,update_employee,correct_punch,decide_punch_correction,decide_overtime_request,create_notice,update_notice,set_work_calendar_day,refresh_work_calendar}',
        note = '建立與維護員工主檔，維護工作日曆，對待簽休假單與補登申請作成決定，並補登打卡'
    WHERE code = 'manage_hr';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('list_work_calendar', true, 'read_only', '讀一年的工作日、休息日、例假與國定假日', now()),
    ('set_work_calendar_day', false, 'needs_confirm', '改一天的日別。對 AI 關閉：這一天算不算工作日由人決定。', now()),
    ('refresh_work_calendar', false, 'needs_confirm', '向政府資料開放平臺取行事曆。對 AI 關閉。', now())
ON CONFLICT (tool_name) DO NOTHING;
