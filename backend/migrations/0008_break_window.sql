-- The working day had no unpaid break in it. work_start to work_end was the
-- whole of it, so a nine-to-six day with an hour for lunch counted nine
-- working hours, and leave taken across that hour was charged for it.
--
-- Overtime was measured as the distance past work_end alone, which is how the
-- break stayed missing for so long: nothing in the middle of the day reached
-- that figure, and neither did anything before its start -- a twelve-o'clock
-- shift worked from four in the morning until half past seven reported one
-- and a half hours of overtime against fifteen and a half hours of
-- attendance. Overtime is now attendance minus this break minus the normal
-- day, so the break has to be stated for it to be excluded.

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('break_start', '休息開始時間', '', '不計入工作時數的休息區間，格式 HH:MM。留空不扣除休息時間。', true, 92, now(), ''),
    ('break_end', '休息結束時間', '', '休息區間的結束時間，格式 HH:MM。兩端都填了才扣除。', true, 94, now(), '')
ON CONFLICT (code) DO NOTHING;

UPDATE company_setting
    SET note = '判定遲到與計算正常工時的基準，格式 HH:MM。留空不判定遲到，也不計加班時數。'
    WHERE code = 'work_start';

UPDATE company_setting
    SET note = '一天正常工時的結束時間，格式 HH:MM。留空不計加班時數。'
    WHERE code = 'work_end';
