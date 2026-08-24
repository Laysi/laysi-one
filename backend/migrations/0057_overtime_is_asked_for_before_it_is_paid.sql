-- Laysi One: overtime is asked for, approved, and only then paid

-- Overtime existed in three places that never met. Punches derive overtime
-- *hours* and stop there; `attendance.overtime_hours` and `overtime_pay` are
-- what an outside timekeeping system asserts about a month; the payslip's
-- `overtime` amount was copied from that import. A company running only this
-- system therefore paid nobody for overtime, and nobody ever approved any --
-- while art. 32 makes the employer's consent the thing that makes overtime
-- lawful in the first place.
--
-- The request is that consent, in the same shape as a leave request: a
-- wall-clock range, hours derived from it, pending until somebody decides.
-- `worked_on` is generated from `from_at` so the day and the range cannot
-- disagree, and so the month a request belongs to is not a second field
-- somebody could set to something else.
--
-- `day_type` has no CHECK. Its values come from the `overtime` ruleset
-- (`weekday`, `rest_day` today) and a ruleset is dated, while a CHECK is not:
-- an amendment that adds a kind of day would leave the constraint refusing
-- rows the law allows. `submit_my_overtime_request` validates against the
-- ruleset resolved on the day worked, which is the dated question.
--
-- The pay is not stored. It is the hours times a multiplier the ruleset holds
-- with an effective date, so `run_payroll` computes it from the ruleset in
-- force on the day worked. Storing it would freeze a figure that an amendment
-- is entitled to change retroactively -- the same reason no withholding tax
-- amount is stored either.

CREATE TABLE overtime_request (
    id          serial PRIMARY KEY,
    request_no  text NOT NULL UNIQUE,
    employee_id int  NOT NULL REFERENCES employee(id) ON DELETE CASCADE,
    day_type    text NOT NULL,
    from_at     timestamp NOT NULL,
    to_at       timestamp NOT NULL,
    hours       numeric(6,2) NOT NULL CHECK (hours > 0),
    reason      text NOT NULL DEFAULT '',
    status      text NOT NULL CHECK (status IN ('pending','approved','rejected')),
    decided_by    text NOT NULL DEFAULT '',
    decided_role  text NOT NULL DEFAULT '',
    decided_at    timestamptz,
    decision_note text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL,
    worked_on   date GENERATED ALWAYS AS (from_at::date) STORED,
    CONSTRAINT overtime_request_span CHECK (to_at > from_at),
    -- A pending request has no decision; a decided one always has a
    -- timestamp. The same rule as leave.
    CONSTRAINT overtime_request_decided_iff_not_pending
        CHECK ((status = 'pending') = (decided_at IS NULL))
);

CREATE INDEX overtime_request_emp_idx ON overtime_request (employee_id, worked_on DESC);

COMMENT ON TABLE overtime_request IS
    '加班申請。核准是加班的前提，核准後由給付當期的薪資作業依規則庫算加班費。';
COMMENT ON COLUMN overtime_request.day_type IS
    '加班日的種類，值取自 overtime 規則庫（平日、休息日）。刻意不設 CHECK：規則有生效日，CHECK 沒有。';

-- Permissions
--
-- Submitting and reading one's own are self-service by the group's rule:
-- neither input names a person, so the subject can only be ctx.employee_id.
-- Reading everyone's is HR reading; deciding one is an HR decision, beside
-- `decide_leave_request` and `decide_punch_correction`.

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections,submit_my_overtime_request,list_my_overtime_requests}' WHERE code = 'staff_self_service';

UPDATE permission SET op_names = '{get_hr_dashboard,list_employees,get_attendance,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches,list_punch_corrections,list_overtime_requests}' WHERE code = 'view_hr';

UPDATE permission SET op_names = '{list_employee_movements,record_employee_movement,complete_movement_step,decide_leave_request,create_employee,update_employee,correct_punch,decide_punch_correction,decide_overtime_request}' WHERE code = 'manage_hr';

-- MCP defaults follow the leave request: reading is open to AI, submitting
-- and deciding are not. Approving overtime is what makes it lawful, and
-- asking for it is a statement about what somebody will do with their
-- evening.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('list_my_overtime_requests', true,  'read_only',
     '讀本人送出的加班申請與目前狀態。'),
    ('submit_my_overtime_request', false, 'needs_confirm',
     '送出加班申請。對 AI 關閉：要不要加班由本人決定。'),
    ('list_overtime_requests',    true,  'read_only',
     '讀加班申請與本月已核准時數。'),
    ('decide_overtime_request',   false, 'needs_confirm',
     '核准或駁回加班申請。對 AI 關閉：核准加班屬主管職責。');
