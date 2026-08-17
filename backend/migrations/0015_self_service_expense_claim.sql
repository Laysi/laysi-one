-- Laysi One: staff submit their own expense claims
--
-- Until now an employee could see their claims and never create one. The gap
-- was not an oversight in the permission table: `submit_expense_claim` takes
-- a `claimant` field, so granting it to staff would also let them file a
-- claim under another name, and `request_payment` carries the payment
-- operations besides. `submit_my_expense_claim` takes no claimant and no
-- department — both come from the employee record behind ctx.employee_id —
-- which is why it can be added to the self-service permission every role
-- already holds instead of to `request_payment`.
--
-- No table changes. A self-service claim is an ordinary row in
-- `expense_claim` and goes through the same `approval_rule` steps; only the
-- way the claimant is established differs.

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim}' WHERE code = 'staff_self_service';

-- Off for MCP, like the other five. The identity bound to a token is not the
-- person at the screen, and a tool that commits the company to a cost on
-- behalf of whoever holds the token has no reason to be on by default.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('submit_my_expense_claim', false, 'needs_confirm',
     '以 ctx.employee_id 對應的員工身分送一張費用報支單。不指定申請人，但仍會產生待簽核單據，預設關閉。')
ON CONFLICT (tool_name) DO NOTHING;
