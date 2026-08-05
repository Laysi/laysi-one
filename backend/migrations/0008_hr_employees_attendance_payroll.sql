-- Laysi One: HR. Employees, movements, attendance and leave, payroll,
-- insurance and pension
--
-- There is not one rate and not one bracket table in this file.
--
-- Labour insurance rates, health insurance rates, the average dependant
-- count, pension contribution rates, withholding rates, the minimum wage and
-- leave entitlements all live in `backend/regulation/2026/*.toml` and are
-- computed by `laysi-regulation`. This schema stores facts and computed
-- results, never the rules themselves.
--
-- `employee.labor_insured_salary` is the insured salary the company filed
-- with the bureau: a fact, not a rule. The rule is that the insured salary
-- has a ceiling, and that ceiling lives in labor_insurance.toml. The
-- calculator always applies min(filed bracket, rule ceiling), so even a
-- stale bracket here is still governed by the rules. The line is exactly
-- there: the bracket is what somebody filed (fact), the ceiling and the rate
-- are what the law says (rule).
--
-- Computed premiums are stored with their version. `payroll_run` has four
-- `*_version` columns and `payslip` records the three brackets used. This is
-- the "amounts derived from regulation must be stored with their version"
-- section of docs/architecture.md in practice: after a retroactive
-- amendment, the only question that matters is which periods used the old
-- version. Without these columns the answer could only come from rerunning
-- everything, which would overwrite figures already filed.
--
-- Overtime pay comes from the timekeeping system; this system never
-- multiplies it out. The multipliers of Labor Standards Act article 24 (4/3,
-- 5/3) are regulation values and are not yet in `backend/regulation/`, so
-- `attendance.overtime_pay` stores an amount rather than hours times a
-- remembered multiplier. To compute it here, add an `overtime.toml` first,
-- then change that one line in ops/payroll.rs.
--
-- Payslips are sensitive but totals are not. The 0006 permission matrix
-- already states that the accountant sees totals, not individuals, and this
-- follows it: `get_payroll_summary` is read-only and `list_payslips` is
-- `Risk::BossOnly`. Both read the same table; the difference is whether
-- dispatch stops the caller.

-- Employees

CREATE TABLE employee (
    id         serial PRIMARY KEY,
    emp_no     text NOT NULL UNIQUE,
    name       text NOT NULL,
    -- The department must point at a value of the department dimension, not
    -- at "net 30". Same approach as voucher_line.dept_id in 0007: a
    -- generated column plus a composite foreign key.
    dept_id    int,
    dept_dimension text
               GENERATED ALWAYS AS (CASE WHEN dept_id IS NULL THEN NULL ELSE 'department' END) STORED,
    title      text NOT NULL DEFAULT '',
    -- This employee's role in the system, sharing values with
    -- ai_audit_log.actor_role.
    role_code  text REFERENCES role(code),
    employment_type text NOT NULL DEFAULT 'full_time'
               CHECK (employment_type IN ('full_time','part_time','contract')),
    hired_on   date NOT NULL,
    left_on    date,
    status     text NOT NULL CHECK (status IN ('active','leaving','left')),
    -- Monthly base salary. Overtime and allowances vary by month and live in
    -- payslip.
    base_salary numeric(18,2) NOT NULL CHECK (base_salary > 0),
    -- The three brackets filed with the authorities. Facts, not rules (see
    -- the note at the top).
    labor_insured_salary  numeric(18,2) NOT NULL CHECK (labor_insured_salary  >= 0),
    health_insured_salary numeric(18,2) NOT NULL CHECK (health_insured_salary >= 0),
    pension_wage          numeric(18,2) NOT NULL CHECK (pension_wage          >= 0),
    -- Only a masked national ID is stored. No feature here needs the full
    -- number, and once stored it is personal data for life, the same
    -- reasoning as api_token holding no plaintext.
    id_no      text NOT NULL DEFAULT '',
    bank_account text NOT NULL DEFAULT '',
    note       text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    created_by text NOT NULL DEFAULT '',
    CONSTRAINT employee_left_after_hired CHECK (left_on IS NULL OR left_on >= hired_on),
    -- A departed employee must have a leaving date; a current one need not
    -- (someone leaving already has a planned date).
    CONSTRAINT employee_left_has_a_date CHECK (status <> 'left' OR left_on IS NOT NULL),
    CONSTRAINT employee_leaving_has_a_date CHECK (status <> 'leaving' OR left_on IS NOT NULL),
    FOREIGN KEY (dept_id, dept_dimension) REFERENCES dimension_value (id, dimension_code)
);

COMMENT ON TABLE employee IS
    '員工主檔。三個投保級距是向主管機關申報的事實；費率與上限來自 backend/regulation/，不在這裡。';
COMMENT ON COLUMN employee.id_no IS
    '遮罩後的身分證字號。完整字號不儲存，系統沒有任何功能需要它。';

CREATE INDEX employee_status_idx ON employee (status, emp_no);

-- Onboarding, offboarding and movements
--
-- A flow drawn as a few hard-coded strings cannot be adjusted per company.
-- In a table, a different company only changes data, the same approach as
-- approval_rule.

CREATE TABLE hr_flow_step (
    kind    text NOT NULL CHECK (kind IN ('hire','leave','salary_change','transfer')),
    step_no int  NOT NULL CHECK (step_no > 0),
    label   text NOT NULL,
    note    text NOT NULL DEFAULT '',
    PRIMARY KEY (kind, step_no)
);

COMMENT ON TABLE hr_flow_step IS
    '到職、離職、調薪、調部門各自要走的關卡。流程是資料，程式只負責走過去。';

INSERT INTO hr_flow_step (kind, step_no, label, note) VALUES
    ('hire', 1, '簽約與建檔',   '勞動契約、個資表、匯薪帳戶'),
    ('hire', 2, '加保',         '勞保、健保、勞退提繳，到職當日就要送件'),
    ('hire', 3, '系統與門禁開通', '差勤帳號、信箱、門禁卡'),
    ('hire', 4, '試用期評核',   '到職滿三個月由主管評核'),

    ('leave', 1, '離職申請與交接', '交接清單簽名'),
    ('leave', 2, '退保',           '勞保、健保於離職當日退保'),
    ('leave', 3, '結算',           '最後一期薪資與未休特休折發工資'),
    ('leave', 4, '開立服務證明',   '離職證明書'),

    ('salary_change', 1, '主管提案',     '寫清楚調整理由'),
    ('salary_change', 2, '老闆核定',     '調薪一律要老闆同意'),
    ('salary_change', 3, '調整投保級距', '薪資變動要跟著改申報級距，否則就是高薪低報'),
    ('salary_change', 4, '更新薪資檔',   '下一期薪資作業生效'),

    ('transfer', 1, '兩邊主管確認', ''),
    ('transfer', 2, '交接完成',     ''),
    ('transfer', 3, '更新部門歸屬', '之後的薪資費用會掛到新部門');

CREATE TABLE employee_movement (
    id          serial PRIMARY KEY,
    movement_no text NOT NULL UNIQUE,
    employee_id int NOT NULL REFERENCES employee(id) ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN ('hire','leave','salary_change','transfer')),
    effective_on date NOT NULL,
    -- Pay and department changes must show what changed from what. Hires and
    -- departures leave it empty.
    from_value  text NOT NULL DEFAULT '',
    to_value    text NOT NULL DEFAULT '',
    reason      text NOT NULL DEFAULT '',
    status      text NOT NULL CHECK (status IN ('open','done')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL
);

CREATE INDEX employee_movement_emp_idx ON employee_movement (employee_id, effective_on DESC);

CREATE TABLE employee_movement_step (
    movement_id int  NOT NULL REFERENCES employee_movement(id) ON DELETE CASCADE,
    step_no     int  NOT NULL,
    done_at     timestamptz NOT NULL DEFAULT now(),
    done_by     text NOT NULL,
    note        text NOT NULL DEFAULT '',
    PRIMARY KEY (movement_id, step_no)
);

COMMENT ON TABLE employee_movement_step IS
    '哪幾關已經完成。沒有資料列＝還沒做，所以「做到哪」是算出來的，不是一個會跟事實脫節的欄位。';

-- Attendance

CREATE TABLE attendance (
    id            serial PRIMARY KEY,
    employee_id   int  NOT NULL REFERENCES employee(id) ON DELETE CASCADE,
    month         text NOT NULL CHECK (month ~ '^\d{4}-\d{2}$'),
    required_days numeric(5,1) NOT NULL CHECK (required_days >= 0),
    worked_days   numeric(5,1) NOT NULL CHECK (worked_days   >= 0),
    leave_days    numeric(5,1) NOT NULL DEFAULT 0 CHECK (leave_days >= 0),
    overtime_hours numeric(6,1) NOT NULL DEFAULT 0 CHECK (overtime_hours >= 0),
    -- Overtime pay as an amount. The multipliers (Labor Standards Act
    -- article 24) are not in the rule library yet, so the system does not
    -- multiply; this column comes from the timekeeping system. See the note
    -- at the top.
    overtime_pay  numeric(18,2) NOT NULL DEFAULT 0 CHECK (overtime_pay >= 0),
    late_count    int NOT NULL DEFAULT 0 CHECK (late_count >= 0),
    note          text NOT NULL DEFAULT '',
    UNIQUE (employee_id, month)
);

COMMENT ON COLUMN attendance.overtime_pay IS
    '加班費金額，由差勤系統帶入。倍數是法規數字且尚未收錄，系統不會拿記憶中的倍數去乘。';

-- Leave
--
-- The annual leave entitlement is not in this table: it is derived from
-- annual_leave.toml by service length, differs every year, and storing it
-- would leave a copy that drifts from the law. Days taken are the fact, and
-- are summed from this table.

CREATE TABLE leave_request (
    id          serial PRIMARY KEY,
    leave_no    text NOT NULL UNIQUE,
    employee_id int  NOT NULL REFERENCES employee(id) ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN
                ('annual','sick','personal','official','marriage','bereavement','maternity','menstrual')),
    from_on     date NOT NULL,
    to_on       date NOT NULL,
    days        numeric(5,1) NOT NULL CHECK (days > 0),
    reason      text NOT NULL DEFAULT '',
    status      text NOT NULL CHECK (status IN ('pending','approved','rejected')),
    decided_by   text NOT NULL DEFAULT '',
    decided_role text NOT NULL DEFAULT '',
    decided_at   timestamptz,
    decision_note text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL,
    CONSTRAINT leave_request_range CHECK (to_on >= from_on),
    -- A pending request has no decision; a decided one always has a
    -- timestamp.
    CONSTRAINT leave_request_decided_iff_not_pending
        CHECK ((status = 'pending') = (decided_at IS NULL))
);

CREATE INDEX leave_request_emp_idx ON leave_request (employee_id, from_on DESC);

-- Payroll
--
-- One row per period (period UNIQUE), the same shape as depreciation: a
-- period cannot be paid twice.

CREATE TABLE payroll_run (
    id        serial PRIMARY KEY,
    period    text NOT NULL UNIQUE CHECK (period ~ '^\d{4}-\d{2}$'),
    pay_on    date NOT NULL,
    headcount int  NOT NULL CHECK (headcount > 0),
    gross_total     numeric(18,2) NOT NULL CHECK (gross_total     >= 0),
    deduction_total numeric(18,2) NOT NULL CHECK (deduction_total >= 0),
    net_total       numeric(18,2) NOT NULL CHECK (net_total       >= 0),
    -- Total employer contributions for insurance and pension. Not part of
    -- the payroll voucher (see the note below).
    employer_total  numeric(18,2) NOT NULL DEFAULT 0 CHECK (employer_total >= 0),
    voucher_no text REFERENCES voucher(no),
    -- Which rule versions this period used. Without these four columns a
    -- retroactive amendment could only be handled by rerunning everything.
    labor_version       text NOT NULL,
    health_version      text NOT NULL,
    pension_version     text NOT NULL,
    withholding_version text NOT NULL,
    note       text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    created_by text NOT NULL,
    -- Gross equals deductions plus net. Enforced by SQL rather than by Rust
    -- remembering.
    CONSTRAINT payroll_run_balances CHECK (gross_total = deduction_total + net_total)
);

COMMENT ON TABLE payroll_run IS
    '一期一筆的薪資作業。四個 *_version 記錄本期依哪一版法規計算，是回溯修法時唯一查得出答案的地方。';

CREATE TABLE payslip (
    id          serial PRIMARY KEY,
    run_id      int NOT NULL REFERENCES payroll_run(id) ON DELETE CASCADE,
    employee_id int NOT NULL REFERENCES employee(id),
    base_salary numeric(18,2) NOT NULL CHECK (base_salary >= 0),
    overtime    numeric(18,2) NOT NULL DEFAULT 0 CHECK (overtime  >= 0),
    allowance   numeric(18,2) NOT NULL DEFAULT 0 CHECK (allowance >= 0),
    gross       numeric(18,2) NOT NULL CHECK (gross >= 0),
    -- The three brackets used at the time. Changing a bracket later does not
    -- alter a period already paid.
    labor_insured_salary  numeric(18,2) NOT NULL,
    health_insured_salary numeric(18,2) NOT NULL,
    pension_wage          numeric(18,2) NOT NULL,
    -- Employee share, deducted from pay.
    labor_employee  numeric(18,2) NOT NULL CHECK (labor_employee  >= 0),
    health_employee numeric(18,2) NOT NULL CHECK (health_employee >= 0),
    income_tax      numeric(18,2) NOT NULL CHECK (income_tax      >= 0),
    -- Employer share, not deducted from pay; it is a company expense.
    labor_employer   numeric(18,2) NOT NULL CHECK (labor_employer   >= 0),
    health_employer  numeric(18,2) NOT NULL CHECK (health_employer  >= 0),
    pension_employer numeric(18,2) NOT NULL CHECK (pension_employer >= 0),
    deduction   numeric(18,2) NOT NULL CHECK (deduction >= 0),
    net         numeric(18,2) NOT NULL CHECK (net >= 0),
    -- Days worked in the period over days in the month. Joining or leaving
    -- mid-period prorates by calendar day.
    paid_days   numeric(5,1) NOT NULL CHECK (paid_days > 0),
    month_days  numeric(5,1) NOT NULL CHECK (month_days > 0),
    note        text NOT NULL DEFAULT '',
    UNIQUE (run_id, employee_id),
    CONSTRAINT payslip_gross_is_the_sum
        CHECK (gross = base_salary + overtime + allowance),
    CONSTRAINT payslip_deduction_is_the_sum
        CHECK (deduction = labor_employee + health_employee + income_tax),
    CONSTRAINT payslip_net_is_what_is_left
        CHECK (net = gross - deduction)
);

COMMENT ON TABLE payslip IS
    '逐人薪資明細。三條 CHECK 讓「應發＝各項加總」「代扣＝三項加總」「實發＝應發－代扣」在資料庫層面就不可能被寫壞。';

-- Permissions: connect "see payslip detail" to real operations
--
-- When 0006 created this permission its op_names was empty because the HR
-- module did not exist. It does now, so missing_ops on `list_roles` drops
-- by three; dispatch enforces role permissions from these names, and
-- individual payslips and withholding statements carry an additional
-- Risk::BossOnly floor.

UPDATE permission
   SET op_names = '{list_payslips,run_payroll,list_withholding_statements,get_payroll_summary}',
       note     = '個人薪資明細與年度扣繳憑單限老闆；彙總（get_payroll_summary）會計看得到'
 WHERE code = 'view_payroll';

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_hr', '人事異動與請假', '到離職、調薪、請假簽核',
     '{list_employees,list_employee_movements,record_employee_movement,complete_movement_step,decide_leave_request}', 95);

INSERT INTO role_permission (role_code, permission_code, level, note) VALUES
    ('boss','manage_hr','full',''),
    ('acct','manage_hr','full','人事作業實務上由會計兼辦'),
    ('manager','manage_hr','limited','只簽自己部門的請假');

-- MCP tool grants
--
-- Everything read-only is on; four are off:
--
--   decide_leave_request        off. Approving leave is a manager's job,
--                               not a model's.
--   run_payroll                 off. It decides what everyone is paid this
--                               month and creates a voucher.
--   list_payslips               off, and boss_only. Individual pay is the
--                               most sensitive data here and there is no
--                               reason for AI to read it by default.
--   list_withholding_statements off, for the same reason (a withholding
--                               statement is a person's annual income).

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('get_hr_dashboard',           true,  'read_only',     '讀人資總覽：在職人數、待辦、與財務相接的幾條'),
    ('list_employees',             true,  'read_only',     '讀員工名冊。不含薪資；薪資在 list_payslips，該操作限老闆。'),
    ('list_employee_movements',    true,  'read_only',     '讀到離職與異動單，含每一關走到哪'),
    ('record_employee_movement',   true,  'needs_confirm', '開一張異動單。只是單子，還沒有人被加保或退保。'),
    ('complete_movement_step',     true,  'needs_confirm', '把異動單的某一關標成完成'),
    ('get_attendance',             true,  'read_only',     '讀某個月的逐人出勤與加班時數'),
    ('list_leave_requests',        true,  'read_only',     '讀請假單與特休額度（額度依年資從 annual_leave 規則算）'),
    ('decide_leave_request',       false, 'needs_confirm', '准假或駁回。對 AI 關閉：核准請假屬主管職責。'),
    ('get_payroll_summary',        true,  'read_only',     '讀某一期薪資彙總與它跟總帳 6111 對不對得起來。彙總不含個人金額。'),
    ('list_payslips',              false, 'boss_only',     '逐人薪資明細。限老闆，且對 AI 關閉。'),
    ('run_payroll',                false, 'high_risk',     '計算並產生某一期的薪資傳票。對 AI 關閉。'),
    ('get_insurance_summary',      true,  'read_only',     '讀勞健保與勞退：公司負擔、員工自付、逐人投保級距。費率全部來自規則庫。'),
    ('compute_insurance',          true,  'read_only',     '試算某個投保級距的三項保費，並回報依哪一版規則'),
    ('get_hr_compliance',          true,  'read_only',     '讀法遵檢核表：最低工資、投保級距、勞退提繳率、特休。沒收錄的規則會照實說沒檢查。'),
    ('list_withholding_statements',false, 'boss_only',     '年度扣繳憑單（逐人年所得）。限老闆，且對 AI 關閉。')
ON CONFLICT (tool_name) DO NOTHING;

-- External integrations: the payroll entry can be reworded
--
-- 0007 said the HR module was a later milestone. Payroll is now computed by
-- this system rather than imported, so that integration stays false but for
-- a different reason.

UPDATE integration
   SET detail = '薪資由本系統計算，不從外部匯入',
       note   = '人資模組已實作：run_payroll 直接產生薪資傳票。外部薪資系統的匯入器沒有寫，也不需要。'
 WHERE code = 'payroll';
