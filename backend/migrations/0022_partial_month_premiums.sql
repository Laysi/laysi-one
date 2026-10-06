-- Premiums for a month not insured in full, and the rule versions behind them.
--
-- Labour insurance and pension are charged by the day in the month a person is
-- enrolled or withdrawn, and health insurance only when the person is insured
-- on the month's last day, so recomputing a payslip needs both ends of the days
-- of the period that person was insured here. They are stored beside the
-- brackets rather than read from the hiring and leaving dates again: a leaving
-- date corrected later must not make an earlier payslip disagree with its own
-- recomputation. NULL on a payslip computed before the span was kept; those
-- charged the whole month.
--
-- A run records every rule version its amounts came from. The dependant cap
-- and the day count are rules of their own, so they get a column each; NULL on
-- a run computed before they were recorded.

ALTER TABLE payslip ADD COLUMN insured_from date;
ALTER TABLE payslip ADD COLUMN insured_to date;
ALTER TABLE payslip ADD CONSTRAINT payslip_insured_span_check
    CHECK (((insured_from IS NULL) = (insured_to IS NULL)) AND insured_from <= insured_to);

COMMENT ON COLUMN payslip.insured_from IS
    '本期在本公司投保的第一天（到職日與期初取較晚者）；空值表示依整月計算的舊薪資單。';
COMMENT ON COLUMN payslip.insured_to IS
    '本期在本公司投保的最後一天（離職日與期末取較早者）；空值表示依整月計算的舊薪資單。';

ALTER TABLE payroll_run ADD COLUMN dependents_version text;
ALTER TABLE payroll_run ADD COLUMN partial_month_version text;

COMMENT ON COLUMN payroll_run.dependents_version IS
    '計算健保眷口數上限所依的 health_dependents 版本；空值表示記錄之前的薪資作業。';
COMMENT ON COLUMN payroll_run.partial_month_version IS
    '到職與離職當月按日計費所依的 partial_month 版本；空值表示記錄之前的薪資作業。';
