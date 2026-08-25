-- An amended punch used to leave no trace of what it amended: punch_in_tx
-- overwrites attendance_punch in place, and HR's own correct_punch wrote no
-- request row at all. Article 30 of the Labor Standards Act requires the
-- attendance record to be kept for five years, and a record that can be
-- rewritten without a trail is the thing an inspection asks about.
--
-- So every amendment now lands here, from either direction: an employee's
-- request that was approved, or HR entering it directly (status 'approved'
-- on arrival). `replaced_at` carries the value the write overwrote.

ALTER TABLE punch_correction
    ADD COLUMN replaced_at timestamp with time zone;

COMMENT ON COLUMN punch_correction.replaced_at IS
    '這次補登覆蓋掉的原打卡時刻。NULL 表示該時段原本沒有紀錄。';

COMMENT ON TABLE punch_correction IS
    '打卡補登紀錄：員工送出的申請，以及人資直接補登（寫入時即為已核准）。核准才寫進 attendance_punch，被覆蓋的原時刻留在 replaced_at。';
