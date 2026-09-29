-- Dependants kept on the employee record.
--
-- One list holds two facts about each person: whether they are covered by
-- this employee's health insurance, and whether the employee claims them for
-- income tax. A spouse in work is claimed but not covered, so the two cannot
-- be one flag. Coverage is a date range rather than a flag because the
-- premium is charged by the month, and recomputing an earlier period has to
-- know who was covered then.
--
-- Siblings and other relatives cannot be covered as health insurance
-- dependants (全民健康保險法第 2 條); they may still be claimed for tax.

CREATE TABLE employee_dependent (
    id serial,
    employee_id integer NOT NULL,
    name text NOT NULL,
    relation text NOT NULL,
    id_no text DEFAULT ''::text NOT NULL,
    birthday date,
    health_enrolled_from date,
    health_enrolled_until date,
    tax_dependent boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text DEFAULT ''::text NOT NULL,
    CONSTRAINT employee_dependent_pkey PRIMARY KEY (id),
    CONSTRAINT employee_dependent_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE,
    CONSTRAINT employee_dependent_name_check CHECK ((btrim(name) <> ''::text)),
    CONSTRAINT employee_dependent_relation_check CHECK ((relation = ANY (ARRAY['spouse'::text, 'parent'::text, 'grandparent'::text, 'child'::text, 'grandchild'::text, 'sibling'::text, 'other'::text]))),
    CONSTRAINT employee_dependent_until_needs_from CHECK (((health_enrolled_until IS NULL) OR (health_enrolled_from IS NOT NULL))),
    CONSTRAINT employee_dependent_enrolment_order CHECK (((health_enrolled_until IS NULL) OR (health_enrolled_until >= health_enrolled_from))),
    CONSTRAINT employee_dependent_health_relation CHECK (((health_enrolled_from IS NULL) OR (relation <> ALL (ARRAY['sibling'::text, 'other'::text])))),
    CONSTRAINT employee_dependent_id_no_upper CHECK ((id_no = upper(id_no)))
);
-- Stored upper case, so the index below compares ids the way the check digit
-- reads them: `a123456789` and `A123456789` are one person. A blank id number
-- is one not collected yet, and two of those are not the same person.
CREATE UNIQUE INDEX employee_dependent_id_no_idx ON public.employee_dependent USING btree (employee_id, id_no) WHERE (id_no <> ''::text);
COMMENT ON TABLE employee_dependent IS
    '員工的眷屬。健保依附記加保日與退保日，列報扶養另記一個旗標；兩者是同一個人的兩個事實。';
COMMENT ON COLUMN employee_dependent.id_no IS
    '眷屬的身分證字號，健保加退保與扣繳申報要用到。寫入時驗檢查碼；稽核紀錄不留此欄的值。';
