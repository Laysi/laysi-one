-- An employee's dependants: the people covered under their health insurance
-- and the people claimed for the salary withholding table.
--
-- One list rather than two. A working spouse is not a health dependant but
-- may still be claimed, so the two facts belong to the same person and the
-- same row: `health_enrolled_from`/`health_enrolled_until` for the first,
-- `tax_dependent` for the second.
--
-- Health cover is a period rather than a flag because premiums are charged
-- by the month: recomputing an earlier month has to know who was covered
-- then, and a flag only knows who is covered now. `health_enrolled_until` is
-- the last day covered, so a period may start and end on the same day.
--
-- Siblings and the catch-all relation cannot be covered (National Health
-- Insurance Act art. 2) but can be claimed for withholding, so they are on
-- the list and the CHECK keeps them off health insurance.
--
-- Rows are replaced as a set by `update_employee`, the same as
-- `employee_bank_account`: nothing references their ids.

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
    CONSTRAINT employee_dependent_id_no_upper CHECK ((id_no = upper(id_no))),
    CONSTRAINT employee_dependent_until_needs_from CHECK (((health_enrolled_until IS NULL) OR (health_enrolled_from IS NOT NULL))),
    CONSTRAINT employee_dependent_enrolment_order CHECK (((health_enrolled_until IS NULL) OR (health_enrolled_until >= health_enrolled_from))),
    CONSTRAINT employee_dependent_health_relation CHECK (((health_enrolled_from IS NULL) OR (relation <> ALL (ARRAY['sibling'::text, 'other'::text]))))
);

-- Stored upper case, so this is a case-insensitive uniqueness rule. A blank
-- number is one nobody has collected yet and may repeat.
CREATE UNIQUE INDEX employee_dependent_id_no_idx ON public.employee_dependent USING btree (employee_id, id_no) WHERE (id_no <> ''::text);
CREATE INDEX employee_dependent_employee_idx ON public.employee_dependent USING btree (employee_id);

COMMENT ON TABLE employee_dependent IS
    '員工的眷屬。依附健保記加保日與退保日，列報扶養另記一個旗標，兩者是同一個人的兩個事實。';
COMMENT ON COLUMN employee_dependent.id_no IS
    '眷屬的身分證字號，健保加退保與扣繳申報要用到。寫入時驗檢查碼；稽核紀錄不留此欄的值。';
COMMENT ON COLUMN employee_dependent.health_enrolled_until IS
    '健保退保日，當天仍在保。留空表示仍在保。';
