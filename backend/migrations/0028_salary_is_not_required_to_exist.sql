-- A person exists before their salary is agreed.
--
-- 0008 wrote `CHECK (base_salary > 0)`, which made the amount a precondition
-- for the record. That put the first day of an installation into a knot: an
-- account needs an employee to reach self-service, an employee needed a salary,
-- and salary is HR's data while accounts are the administrator's. Nobody could
-- start.
--
-- Zero now means "not set yet". Payroll refuses such an employee by name rather
-- than paying zero, which is the check that actually matters; the three filed
-- insurance brackets were already `>= 0`.

ALTER TABLE employee DROP CONSTRAINT employee_base_salary_check;
ALTER TABLE employee ADD CONSTRAINT employee_base_salary_check CHECK (base_salary >= 0);

COMMENT ON COLUMN employee.base_salary IS
    '月底薪。0 表示尚未設定，薪資作業會擋下這樣的員工而不是發出 0 元薪資單。';
