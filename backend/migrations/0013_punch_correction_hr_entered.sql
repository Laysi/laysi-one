-- Which of the two paths wrote a correction.
--
-- HR entering a punch directly and an employee's request that someone
-- approved were told apart by `decided_by = created_by`. Both columns hold
-- display names, so the test also holds for an owner approving their own
-- request under owner_self_decide, and for any approver who shares the
-- requester's name. Which path a correction took is part of an attendance
-- record kept for five years, so it is written down when the row is written.

ALTER TABLE punch_correction ADD COLUMN hr_entered boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN punch_correction.hr_entered IS
    '人資直接補登為 true；員工送出的補登申請為 false，不論由誰核准。';

-- HR's correction writes created_at and decided_at in one statement, so the
-- two instants are equal; an approval always comes in a later transaction.
-- A setting that approved a request on submission leaves decided_by empty.
UPDATE punch_correction
   SET hr_entered = true
 WHERE decided_by <> ''
   AND decided_by = created_by
   AND decided_at = created_at;
