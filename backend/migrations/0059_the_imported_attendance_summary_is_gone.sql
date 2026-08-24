-- The imported monthly attendance summary is removed.
--
-- `attendance` had no write path anywhere: no operation, no import parser and
-- no seed ever inserted a row. Everything built on it therefore ran one
-- branch and only one. `get_attendance` returned an empty list and an empty
-- month picker; `reconciliation()` reported "nothing to compare with" on
-- every call; `run_payroll` fell back to an `overtime_pay` that was always
-- zero; and three screens carried an empty section describing a timekeeping
-- system that does not exist. Same shape as `hr_flow_step` in 0058: a table
-- nothing writes is a constant wearing a table.
--
-- The month picker on 「出勤與休假」 read its months from this table, so it was
-- always empty even where punches existed. It now reads the months that have
-- punches.
--
-- 0057 already replaced the one thing the column was for: overtime pay comes
-- from an approved request priced by the ruleset of the day worked. Keeping
-- the import alongside it meant one question with two answers, one of which
-- nothing could ever supply.

UPDATE permission
   SET op_names = '{get_hr_dashboard,list_employees,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches,list_punch_corrections,list_overtime_requests}'
 WHERE code = 'view_hr';

DELETE FROM mcp_tool_grant WHERE tool_name = 'get_attendance';

DROP TABLE attendance;
