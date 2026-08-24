-- Three of the four dimensions have no consumer.
--
-- Only `department` is reachable from anything: `voucher_line.dept_id`,
-- `employee.dept_id`, `expense_claim.dept_id` and `budget.dept_id` all resolve
-- through `departments()`, which filters on `dimension_code = 'department'`,
-- and the two composite foreign keys pin the dimension to that value. Nothing
-- reads a project, a cost centre or a payment term — not a query, not a
-- report, not a screen — yet 「基礎設定」 offered an 「新增值」 button under each
-- of them. Values entered there could only ever sit in the table.
--
-- The delete cascades to `dimension_value`, but the four foreign keys above
-- have no cascade of their own: an install that somehow attached one of these
-- values to a voucher, an employee, a claim or a budget line fails this
-- migration rather than losing the reference.

DELETE FROM dimension WHERE code IN ('project', 'cost_center', 'payment_term');

UPDATE mcp_tool_grant SET note = '讀部門清單' WHERE tool_name = 'list_dimensions';
