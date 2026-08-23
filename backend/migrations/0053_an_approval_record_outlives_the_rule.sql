-- A signature is history; the rule it was taken under is configuration.
--
-- expense_claim_step.step_no pointed at approval_rule(step_no) with no ON
-- DELETE, which tied the two together in both directions. Deleting a step
-- somebody had already signed was refused by the constraint, and a claim whose
-- remaining steps were deleted could not record the signature that opens its
-- voucher, because every number it could use belonged to a rule that no longer
-- existed. Neither is what the reference was for: nothing reads
-- expense_claim_step by joining approval_rule, and the label of the step is
-- already written into the audit record when it is taken.
--
-- The number stays, as the ordinal it always was. UNIQUE (claim_id, step_no)
-- still holds, so one step is still signed once.

ALTER TABLE expense_claim_step DROP CONSTRAINT expense_claim_step_step_no_fkey;

COMMENT ON COLUMN expense_claim_step.step_no IS
    '簽核當下的關卡序號。簽核規則之後可能變更或刪除，這個數字不跟著動。';
