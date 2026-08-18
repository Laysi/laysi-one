-- Separate running the system from owning the business.
--
-- Until now `boss` held both: whoever installed the product could read every
-- employee's salary. The two concerns are now two roles and two risk floors.
-- `Risk::AdminOnly` covers permissions, tokens, company settings and logins;
-- `Risk::BossOnly` keeps payroll, withholding, budget and approval thresholds.
-- Neither floor can be lifted through role_permission, and no operation
-- changes an existing account's role, so an administrator cannot promote
-- themselves to owner without shell access to the server.

ALTER TABLE mcp_tool_grant
    DROP CONSTRAINT mcp_tool_grant_risk_check;
ALTER TABLE mcp_tool_grant
    ADD CONSTRAINT mcp_tool_grant_risk_check
    CHECK (risk IN ('read_only','needs_confirm','high_risk','boss_only','admin_only'));

INSERT INTO role (code, name, note, sort_order) VALUES
    ('admin', '系統管理員', '管理帳號、權限與系統設定，看不到薪資', 5)
ON CONFLICT (code) DO NOTHING;

UPDATE role
   SET note = '公司的最終決定者：薪資、預算與簽核門檻'
 WHERE code = 'boss';

-- Creating a login is now an operation rather than shell access only.
UPDATE permission
   SET op_names = '{update_company_setting,set_role_permission,issue_api_token,revoke_api_token,create_user_account}',
       note = '建立登入帳號、修改公司設定與角色權限，並簽發或撤銷 API／MCP Token'
 WHERE code = 'manage_access';

-- The administrator needs master data to finish first-time setup: bank
-- accounts, departments and the chart of accounts. set_approval_rule is in
-- the same permission but stays behind the owner floor.
--
-- The three read permissions are what makes the four system screens open at
-- all: without view_system the administrator cannot reach 「角色與核准」, which
-- is where the owner account gets created, and a fresh install would have
-- nobody able to finish it. None of them exposes an amount or a payslip.
INSERT INTO role_permission (role_code, permission_code, level, note, updated_by) VALUES
    ('admin', 'manage_access', 'full',
     '系統管理員負責帳號、權限與 Token。', 'migration 0023'),
    ('admin', 'manage_masters', 'full',
     '首次設定需要建立銀行帳戶、部門與科目；簽核門檻仍限負責人。', 'migration 0023'),
    ('admin', 'view_system', 'full',
     '開啟角色與核准、安全與營運兩個畫面。', 'migration 0023'),
    ('admin', 'view_masters', 'full',
     '開啟基礎設定與首次設定精靈。', 'migration 0023'),
    ('admin', 'view_audit', 'full',
     '開啟 MCP 與 AI 的稽核軌跡。', 'migration 0023')
ON CONFLICT (role_code, permission_code) DO UPDATE
    SET level = EXCLUDED.level,
        note = EXCLUDED.note,
        updated_by = EXCLUDED.updated_by;

-- The owner keeps the business, not the controls. 0011 granted boss every
-- permission; manage_access is the one that moves.
DELETE FROM role_permission
 WHERE role_code = 'boss'
   AND permission_code = 'manage_access';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('create_user_account', false, 'admin_only',
     '建立登入帳號。對 AI 關閉：發帳號是人的決定。')
ON CONFLICT (tool_name) DO UPDATE
    SET risk = EXCLUDED.risk,
        note = EXCLUDED.note;

UPDATE mcp_tool_grant
   SET risk = 'admin_only'
 WHERE tool_name IN ('update_company_setting', 'set_role_permission', 'issue_api_token');

-- list_chart_of_accounts carries balances: it runs the same query as the three
-- statements. Leaving it in view_masters would hand the administrator the
-- company's trial balance, which is the same objection as payroll one level up.
-- The chart's structure stays readable through list_accounts; the numbers move
-- to the permission that already owns the statements.
UPDATE permission
   SET op_names = '{list_accounts,get_company_settings,list_dimensions}',
       note = '會計科目、維度與公司設定的唯讀入口；科目餘額在 view_reports'
 WHERE code = 'view_masters';

UPDATE permission
   SET op_names = '{get_dashboard,get_trial_balance,get_balance_sheet,get_income_statement,get_cash_flow,list_chart_of_accounts}',
       note = '總覽、試算表、三張財務報表與科目餘額；現金與銀行作業另由 manage_banking 管理'
 WHERE code = 'view_reports';

UPDATE security_policy
   SET detail = '負責人看得到金額但改不了權限，系統管理員改得了權限但看不到薪資與科目餘額；建立傳票的人不能自行核准。'
 WHERE code = 'separation_of_duties';
