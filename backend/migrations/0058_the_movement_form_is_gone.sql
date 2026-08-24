-- The employee movement form is removed.
--
-- Four kinds of movement were on offer; two of them (a pay change and a
-- transfer) ended by writing one column that the roster's edit drawer writes
-- in one step, and nothing required the form to be used, so the seven
-- checkboxes were paperwork beside a shortcut. A hire movement wrote nothing
-- at all. Only the leaving flow did work no other operation did, and that work
-- moves to `update_employee`: setting a status of 'left' now closes the
-- person's logins, sessions and MCP tokens in the same transaction. The
-- roster and the access were already meant to be one event; they are now one
-- write.
--
-- `hr_flow_step` also had no write path anywhere in the code, so its 15 rows
-- were constants wearing a table.

UPDATE permission
   SET op_names = '{decide_leave_request,create_employee,update_employee,correct_punch,decide_punch_correction,decide_overtime_request}'
 WHERE code = 'manage_hr';

DELETE FROM mcp_tool_grant
 WHERE tool_name IN (
    'list_employee_movements',
    'record_employee_movement',
    'complete_movement_step'
 );

DROP TABLE employee_movement_step;
DROP TABLE employee_movement;
DROP TABLE hr_flow_step;
