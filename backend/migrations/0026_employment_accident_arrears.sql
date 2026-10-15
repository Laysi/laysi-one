-- Employment insurance, occupational accident insurance and the arrears wage
-- payment fund.
--
-- Employment insurance is withheld from pay beside labour insurance and billed
-- apart from it. Not everyone in labour insurance is in it: an owner never is,
-- and some foreign nationals and people already drawing an old-age benefit are
-- not either. The roster records who is; an owner is left out whatever the
-- column says, because the identity already answers it.
--
-- Occupational accident insurance is the company's alone. Its rate is the
-- commuting rate in the rule library plus the industry rate the bureau assigns
-- to the company, which for an employer of fifty or more is experience rated
-- every year. That rate is a fact the company files, like the brackets on the
-- roster, so it is a setting the administrator keeps; blank means nobody has
-- entered it yet and the premium is not computed. A payslip keeps the rate it
-- used, so changing the setting next year does not make this year's payslips
-- disagree with their own recomputation; NULL means it was not computed.
--
-- The arrears wage payment fund is applied to the month's total insured salary
-- and rounded once, so it is a figure of the runs and not of any payslip. A
-- run paying people late records the period's fund less what the earlier runs
-- recorded, so the runs of a period add up to one computation.
--
-- Payslips and runs written before these columns existed charged none of the
-- three. Their amounts read as zero and their versions as NULL, which is how
-- the payslip screen tells them apart from a run that charged zero.

ALTER TABLE employee ADD COLUMN employment_insured boolean DEFAULT true NOT NULL;

COMMENT ON COLUMN employee.employment_insured IS
    '是否參加就業保險。受僱者預設參加；負責人一律不參加，不論本欄。';

ALTER TABLE payslip ADD COLUMN employment_employee numeric(18,2) DEFAULT 0 NOT NULL;
ALTER TABLE payslip ADD COLUMN employment_employer numeric(18,2) DEFAULT 0 NOT NULL;
ALTER TABLE payslip ADD COLUMN accident_employer numeric(18,2) DEFAULT 0 NOT NULL;
ALTER TABLE payslip ADD COLUMN accident_rate numeric;

ALTER TABLE payslip ADD CONSTRAINT payslip_employment_employee_check
    CHECK ((employment_employee >= (0)::numeric));
ALTER TABLE payslip ADD CONSTRAINT payslip_employment_employer_check
    CHECK ((employment_employer >= (0)::numeric));
ALTER TABLE payslip ADD CONSTRAINT payslip_accident_employer_check
    CHECK ((accident_employer >= (0)::numeric));
ALTER TABLE payslip ADD CONSTRAINT payslip_accident_rate_check
    CHECK ((accident_rate IS NULL) OR (accident_rate >= (0)::numeric));
ALTER TABLE payslip ADD CONSTRAINT payslip_accident_needs_a_rate
    CHECK ((accident_rate IS NOT NULL) OR (accident_employer = (0)::numeric));

ALTER TABLE payslip DROP CONSTRAINT payslip_deduction_is_the_sum;
ALTER TABLE payslip ADD CONSTRAINT payslip_deduction_is_the_sum
    CHECK ((deduction = (((labor_employee + employment_employee) + health_employee) + income_tax)));

COMMENT ON COLUMN payslip.employment_employee IS
    '就業保險員工自付額，與勞保普通事故分開取整。';
COMMENT ON COLUMN payslip.accident_employer IS
    '職業災害保險費，全額由公司負擔。';
COMMENT ON COLUMN payslip.accident_rate IS
    '計算當時公司設定的職災行業別災害費率（小數，不含上下班災害費率）；空值表示未計職災保險費。';

ALTER TABLE payroll_run ADD COLUMN wage_arrears numeric(18,2) DEFAULT 0 NOT NULL;
ALTER TABLE payroll_run ADD COLUMN employment_version text;
ALTER TABLE payroll_run ADD COLUMN accident_version text;
ALTER TABLE payroll_run ADD COLUMN wage_arrears_version text;

ALTER TABLE payroll_run ADD CONSTRAINT payroll_run_wage_arrears_check
    CHECK ((wage_arrears >= (0)::numeric));
ALTER TABLE payroll_run ADD CONSTRAINT payroll_run_extra_versions_together
    CHECK (((employment_version IS NULL) = (accident_version IS NULL))
       AND ((employment_version IS NULL) = (wage_arrears_version IS NULL)));

COMMENT ON COLUMN payroll_run.wage_arrears IS
    '本次計算記下的積欠工資墊償基金。整期受僱者的勞保投保薪資（以勞保上限計，不足月按日數折算）合計乘費率、取整一次，負責人不計；同一期分次計算時，本次記整期金額減去先前各次已記的部分。';
COMMENT ON COLUMN payroll_run.employment_version IS
    '就業保險所依的 employment_insurance 版本；空值表示記錄之前的薪資作業，當時未計就保、職災與墊償。';

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('accident_industry_rate', '職災保險行業別災害費率（%）', '', '', true, 118, now(), '')
ON CONFLICT (code) DO NOTHING;
