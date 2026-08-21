-- Document numbers restarted whenever the period changed.
--
-- `doc_sequence` was keyed on the prefix alone and carried a single `period`
-- column: taking a number for a different period reset the counter to 1 and
-- overwrote the period. Working forward that is right — a new month starts at
-- 001 — but going back is not. Post a depreciation voucher dated 09-30, then
-- run August payroll (voucher dated 08-31), and the counter resets to 1 and
-- hands out `V2608-001`, which already exists. `voucher.no` is UNIQUE, so
-- payroll fails with a constraint violation and there is no way round it from
-- the screen.
--
-- The counter is per (prefix, period), so it becomes the key. The existing
-- rows only know the last period each prefix touched, so they are rebuilt
-- from the numbers actually issued: for every document table that draws from
-- this sequence, the highest number seen in each period plus one.
--
-- Leave requests numbered before 0039 used the Republic era (`LV11507-001`),
-- five digits where the pattern wants four, so they do not match and are
-- skipped. That is correct: `LV2608` cannot collide with `LV11508`.

DELETE FROM doc_sequence;

ALTER TABLE doc_sequence DROP CONSTRAINT doc_sequence_pkey;
ALTER TABLE doc_sequence ADD PRIMARY KEY (prefix, period);

INSERT INTO doc_sequence (prefix, period, next_no)
SELECT prefix, period, MAX(seq) + 1
  FROM (
        SELECT no        AS doc FROM voucher
  UNION ALL SELECT batch_no       FROM ap_payment_batch
  UNION ALL SELECT claim_no       FROM expense_claim
  UNION ALL SELECT doc_no         FROM document
  UNION ALL SELECT batch_no       FROM import_batch
  UNION ALL SELECT movement_no    FROM employee_movement
  UNION ALL SELECT leave_no       FROM leave_request
       ) AS issued
  CROSS JOIN LATERAL (
        SELECT substring(doc from '^([A-Z]+)[0-9]{4}-[0-9]+$')      AS prefix,
               substring(doc from '^[A-Z]+([0-9]{4})-[0-9]+$')      AS period,
               (substring(doc from '^[A-Z]+[0-9]{4}-([0-9]+)$'))::int AS seq
       ) AS parts
 WHERE parts.prefix IS NOT NULL
 GROUP BY prefix, period;
