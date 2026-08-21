-- Laysi One: an employee can have more than one bank account
--
-- 0008 gave `employee` a single `bank_account` text column, so a person had
-- exactly one account and it was whatever string somebody typed. Both halves
-- fail in practice: people keep a salary account separate from the one that
-- receives expense reimbursements, and a free-text column cannot answer
-- "which account does the salary go to" once there are two of them.
--
-- The bank name and the account number are separate columns because they are
-- separate facts and the screens print them apart -- self-service shows the
-- bank and the last digits, never the whole number. Splitting one string on
-- the first digit is a guess; storing them apart is not.
--
-- `is_payroll` marks the destination of the monthly transfer. It is a partial
-- unique index rather than a column on `employee`, so a person cannot end up
-- pointing at an account belonging to somebody else, and a listed account
-- cannot be silently dropped while `employee` still names it.
--
-- The screens call it 薪轉帳戶, which is what a Taiwanese payroll clerk calls
-- it. 0008 seeded the hiring checklist with a different word for the same
-- thing, and it is updated below rather than left to read as a second kind of
-- account.

CREATE TABLE employee_bank_account (
    id          serial PRIMARY KEY,
    employee_id int  NOT NULL REFERENCES employee(id) ON DELETE CASCADE,
    bank_name   text NOT NULL DEFAULT '',
    -- An account with no number is not an account.
    account_no  text NOT NULL CHECK (btrim(account_no) <> ''),
    -- Where the monthly payroll transfer goes. At most one per person; a
    -- person with no account at all is allowed, because the roster accepts an
    -- employee before their paperwork arrives (0028).
    is_payroll  boolean NOT NULL DEFAULT false,
    -- What the other accounts are for, in the words of whoever keeps the
    -- roster. Free text: the system reads only `is_payroll`.
    note        text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL DEFAULT '',
    UNIQUE (employee_id, bank_name, account_no)
);

CREATE UNIQUE INDEX employee_payroll_account_idx
    ON employee_bank_account (employee_id) WHERE is_payroll;

COMMENT ON TABLE employee_bank_account IS
    '員工的銀行帳戶，一人可有多筆；標為薪轉帳戶的至多一筆。';

-- Move what the single column held. The old value was one string with no
-- agreed shape, so the split is a best effort: everything before the first
-- digit is the bank, the rest is the number. A value with no digit at all
-- cannot be an account number and is kept whole, which leaves it visible on
-- the roster to be corrected rather than dropped.
INSERT INTO employee_bank_account (employee_id, bank_name, account_no, is_payroll, created_by)
SELECT id,
       btrim(substring(bank_account from '^[^0-9]*')),
       btrim(substring(bank_account from '[0-9].*$')),
       true,
       created_by
  FROM employee
 WHERE bank_account ~ '[0-9]';

INSERT INTO employee_bank_account (employee_id, bank_name, account_no, is_payroll, created_by)
SELECT id, '', btrim(bank_account), true, created_by
  FROM employee
 WHERE bank_account !~ '[0-9]' AND btrim(bank_account) <> '';

ALTER TABLE employee DROP COLUMN bank_account;

UPDATE hr_flow_step
   SET note = '勞動契約、個資表、薪轉帳戶'
 WHERE kind = 'hire' AND step_no = 1 AND note = '勞動契約、個資表、匯薪帳戶';
