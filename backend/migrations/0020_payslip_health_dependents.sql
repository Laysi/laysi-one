-- How many dependants a payslip charged health insurance for.
--
-- A payslip keeps the brackets it was computed on, so the payslip screen can
-- recompute it under the same rules and say whether the stored figures still
-- agree. The employee's health deduction now also depends on how many
-- dependants were covered at the end of the period, and that is read from a
-- list HR keeps editing: a dependant registered later with an earlier
-- enrolment date would make every earlier payslip disagree with a
-- recomputation, although each one charged what was on file when it was
-- computed. So the count is stored beside the brackets.
--
-- The count charged, after the cap, not the count covered. Payslips written
-- before dependants were recorded charged none, which is what the default
-- says.

ALTER TABLE payslip ADD COLUMN health_dependents integer DEFAULT 0 NOT NULL;
ALTER TABLE payslip ADD CONSTRAINT payslip_health_dependents_check CHECK ((health_dependents >= 0));

COMMENT ON COLUMN payslip.health_dependents IS
    '本張薪資單的健保自付額計入的眷口數（已套用上限），不含本人。';
