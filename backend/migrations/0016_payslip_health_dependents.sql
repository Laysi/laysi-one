-- Dependants a payslip's health insurance deduction was charged for.
--
-- The employee pays one premium for themselves and one for each dependant
-- covered on the last day of the period, up to the `health_dependents` cap.
-- The count is kept as it stood when the slip was computed, like the insured
-- brackets beside it: the payslip screen recomputes each slip from its stored
-- facts under the rules, and a dependant registered later must not make an
-- earlier slip disagree with itself. The cap is a rule and is not stored.
--
-- Every slip written before this column existed was charged for nobody, so
-- zero is what those slips were computed with.

ALTER TABLE payslip
    ADD COLUMN health_dependents integer DEFAULT 0 NOT NULL,
    ADD CONSTRAINT payslip_health_dependents_check CHECK ((health_dependents >= 0));

COMMENT ON COLUMN payslip.health_dependents IS
    '本期健保自付額計入的在保眷屬人數（期末日在保者，未套用計費上限）。';
