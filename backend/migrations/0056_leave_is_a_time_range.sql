-- Laysi One: leave is a span of time, not a number somebody types

-- 0008 stored a leave request as two dates plus a `days` figure the employee
-- typed in. Two facts were missing and a third could not be checked: nobody
-- could say a request ran from 13:30 to 18:00, half a day meant whatever the
-- requester decided it meant, and `days` was free to disagree with the dates
-- beside it. A request for one afternoon and a request for a fortnight looked
-- the same on screen.
--
-- The range is now the record: `from_at` and `to_at` are wall-clock local
-- timestamps, because a leave request is a statement about the working day
-- ("the 25th, 09:00 to 13:00"), not about an instant on a global clock. That
-- also keeps `from_on` and `to_on` derivable, so they become generated
-- columns and can no longer drift from the range they summarise -- every
-- reader, index and document number keeps working unchanged.
--
-- `hours` is what the range is worth in working time, and `days` is the same
-- quantity in the unit the annual leave entitlement is stated in. Both are
-- computed by `request_leave` from the working window and frozen at
-- submission: `company_setting.work_start` and `work_end` hold one current
-- value with no effective date, so recomputing an approved request later
-- would silently restate what was approved.
--
-- `days` may now round to zero: a fifteen-minute absence is real leave and
-- 0.01 of a day is not a useful number. `hours > 0` is the invariant that
-- matters, and `days` follows from it.

ALTER TABLE leave_request
    ADD COLUMN from_at timestamp,
    ADD COLUMN to_at   timestamp,
    ADD COLUMN hours   numeric(6,2);

-- Existing rows were day-granular, so their range is the whole working day.
-- The hours they were worth is the figure that was approved, restated in the
-- working window in force now; nothing else about them is known.
WITH work_window AS (
    SELECT
        COALESCE((SELECT NULLIF(btrim(value), '') FROM company_setting WHERE code = 'work_start')::time,
                 TIME '09:00') AS starts_at,
        COALESCE((SELECT NULLIF(btrim(value), '') FROM company_setting WHERE code = 'work_end')::time,
                 TIME '18:00') AS ends_at
)
UPDATE leave_request
   SET from_at = from_on + work_window.starts_at,
       to_at   = to_on   + work_window.ends_at,
       hours   = ROUND(days * (EXTRACT(EPOCH FROM (work_window.ends_at - work_window.starts_at)) / 3600)::numeric, 2)
  FROM work_window;

ALTER TABLE leave_request
    ALTER COLUMN from_at SET NOT NULL,
    ALTER COLUMN to_at   SET NOT NULL,
    ALTER COLUMN hours   SET NOT NULL,
    ADD CONSTRAINT leave_request_hours CHECK (hours > 0),
    ADD CONSTRAINT leave_request_span  CHECK (to_at > from_at);

ALTER TABLE leave_request
    DROP CONSTRAINT leave_request_range,
    DROP CONSTRAINT IF EXISTS leave_request_days_check,
    ALTER COLUMN days TYPE numeric(6,2),
    ADD CONSTRAINT leave_request_days CHECK (days >= 0);

ALTER TABLE leave_request DROP COLUMN from_on, DROP COLUMN to_on;

ALTER TABLE leave_request
    ADD COLUMN from_on date GENERATED ALWAYS AS (from_at::date) STORED,
    ADD COLUMN to_on   date GENERATED ALWAYS AS (to_at::date)   STORED;

-- Dropping from_on took the index with it.
CREATE INDEX leave_request_emp_idx ON leave_request (employee_id, from_on DESC);

COMMENT ON COLUMN leave_request.from_at IS
    '休假開始的當地時刻，牆上時鐘的時間。from_on 由它推導。';
COMMENT ON COLUMN leave_request.hours IS
    '這段時間落在上班時間內的工作時數，送出當下依上下班時間計算並定案。';
