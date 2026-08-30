-- Two axes the roster could not express: how a person is paid, and how they
-- are insured.
--
-- `employment_type` already held `part_time`, but nothing read it: a part
-- timer was paid a monthly salary prorated by calendar day, which is not what
-- an hourly worker earns. Pay basis is the axis payroll needs, and it is not
-- the same axis: a part timer may be on a monthly salary, and a full timer
-- may not. `base_salary` carries the hourly rate when the basis is hourly.
--
-- A company's own owner is insured differently from the people they employ:
-- health insurance is theirs in full, and pension contributions do not apply
-- because the Labour Pension Act covers employees. The shares live in the
-- `owner_insurance` ruleset, not here; this column only says which set of
-- shares applies to this person.
ALTER TABLE employee
    ADD COLUMN pay_basis text DEFAULT 'monthly' NOT NULL,
    ADD COLUMN insured_as text DEFAULT 'employee' NOT NULL,
    ADD CONSTRAINT employee_pay_basis_check
        CHECK (pay_basis = ANY (ARRAY['monthly'::text, 'hourly'::text])),
    ADD CONSTRAINT employee_insured_as_check
        CHECK (insured_as = ANY (ARRAY['employee'::text, 'owner'::text]));

COMMENT ON COLUMN employee.pay_basis IS
    '計薪基準。monthly 時 base_salary 是月底薪，按曆日比例計薪；hourly 時 base_salary 是時薪，依打卡工時計薪。';
COMMENT ON COLUMN employee.insured_as IS
    '投保身分。owner 為公司負責人：健保保費全額自付，勞退不強制提繳。分擔比率在 owner_insurance 規則，不在這裡。';
