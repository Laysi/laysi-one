-- Someone hired after payroll ran must still be paid for the days worked.
--
-- payroll_run.period was UNIQUE, so a period could be calculated exactly once.
-- A person hired on the 20th when payroll ran on the 19th was skipped as "not
-- yet employed in this period" and then had no way in: the period was closed,
-- and a reversing entry corrects the ledger without reopening it. Those days
-- were unpayable.
--
-- What must not happen is paying the same person twice in one period, and that
-- is a different rule, already enforced elsewhere: run_payroll now skips anyone
-- who already holds a payslip for the period, and payslip UNIQUE (run_id,
-- employee_id) stops a single run from doing it. Dropping this constraint
-- leaves each supplementary run as its own row with its own voucher and its own
-- rule versions, which is what a separate payment actually is.

ALTER TABLE payroll_run DROP CONSTRAINT payroll_run_period_key;
CREATE INDEX payroll_run_period_idx ON payroll_run (period);

COMMENT ON COLUMN payroll_run.period IS
    '薪資期別。同一期可以有多次計算：先算過的人不會再算一次，後來到職或當時漏算的人各自成為一次補算。';
