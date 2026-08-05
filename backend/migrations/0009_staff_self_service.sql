-- Laysi One: the staff role and grants for employee self-service
--
-- This is not a second set of HR master data. Payslips, leave requests,
-- attendance and expense claims already live in the tables created by 0007
-- and 0008. Self-service is only a different direction of view: the general
-- HR operations look at the whole company, while self-service always returns
-- the caller's own data. This migration therefore copies no data and adds no
-- "whose records may be seen" column.
--
-- Self-service does not decide "own" from Risk. `list_payslips` is
-- `BossOnly` because it returns per-person detail for the whole company;
-- self-service operations are `ReadOnly`, but their input carries no
-- employee_id, employee number or name, and the subject can only come from
-- `ctx.employee_id` on a verified session. The role permission and the shape
-- of the input together hold the "own records only" line.
--
-- The UI identity is resolved from the HttpOnly session alone; neither
-- request headers nor operation parameters can name an employee.
--
-- Everything is off for MCP by default. The identity bound to an MCP token
-- is not the person sitting at the screen. Even though a self-service
-- operation only returns `ctx.actor`'s data, there is no reason to let AI
-- read payslips, view leave or submit leave requests by default, so all five
-- grants below are false. That is an AI switch and does not affect people in
-- the UI.

-- The staff role

INSERT INTO role (code, name, note, sort_order) VALUES
    ('staff', '一般職員', '只看得到自己的薪資、請假與請款', 60);

-- A production install preloads no employees, and a one-off UPDATE cannot
-- supply a default for rows added later, so the column default is the staff
-- role. Employees needing other permissions can still be given an explicit
-- role at creation.
ALTER TABLE employee
    ALTER COLUMN role_code SET DEFAULT 'staff';

-- The permission matrix for people
--
-- This permission says "a person with an employee record may use
-- self-service". None of the five operations has a field that can name
-- someone else; the subject comes from the employee_id bound to the trusted
-- session.

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('staff_self_service', '使用員工自助',
     '只回可信 session 的 ctx.employee_id 對應員工自己的資料',
     '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims}',
     96);

INSERT INTO role_permission (role_code, permission_code, level, note) VALUES
    ('boss',    'staff_self_service', 'full', '有員工主檔時只看自己'),
    ('acct',    'staff_self_service', 'full', '有員工主檔時只看自己'),
    ('cashier', 'staff_self_service', 'full', '有員工主檔時只看自己'),
    ('manager', 'staff_self_service', 'full', '有員工主檔時只看自己'),
    ('staff',   'staff_self_service', 'full', '一般職員只看自己');

-- MCP tool grants
--
-- All five are false: the actor of an MCP token is not the person currently
-- at the UI. Even though self-service can only read the token holder's own
-- records, handing personal payslips and leave to AI by default has no
-- justification.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('get_my_home',             false, 'read_only',
     '讀自己的基本資料、出勤、特休與送單進度。MCP token 身分不等於畫面使用者，預設關閉。'),
    ('list_my_payslips',        false, 'read_only',
     '只讀 ctx.actor 自己的薪資單。個人薪資不預設交給 AI，預設關閉。'),
    ('list_my_leave_requests',  false, 'read_only',
     '只讀 ctx.actor 自己的請假單與特休餘額。MCP 預設關閉。'),
    ('request_leave',           false, 'needs_confirm',
     '替 ctx.actor 自己送請假單。token 綁定者與畫面使用者可能不同，預設關閉。'),
    ('list_my_expense_claims',  false, 'read_only',
     '只讀 ctx.actor 自己送出的費用報支與逐關進度。MCP 預設關閉。')
ON CONFLICT (tool_name) DO NOTHING;
