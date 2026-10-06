-- Whether a month's salary is paid within that month or early in the next.
--
-- The payroll screen's default period, run_payroll without a period and the
-- HR dashboard's reminder all name the period this makes due. The seed is
-- what every installation did before the row existed; the value is the word
-- the screen prints.

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('payroll_timing', '發薪時點', '當月', '', true, 117, now(), '')
ON CONFLICT (code) DO NOTHING;
