-- Laysi One: flexible start and end times
--
-- 0016 gave the working day one start and one end, with `late_grace_minutes`
-- forgiving a few minutes at the edge. Grace and flexitime are not the same
-- thing and the difference is the whole point of this migration: grace
-- forgives a late arrival and still expects the same departure, while
-- flexitime moves the whole day. Someone starting an hour later under grace
-- is late; under flexitime they simply finish an hour later.
--
-- `flex_window_minutes` is how much later than `work_start` the day may
-- begin. The day's length never changes, so the expected end shifts by
-- exactly how late the arrival was, capped at the window. Arriving early does
-- not shift it the other way: nobody was asked to come early, and letting it
-- shorten the day would turn a favour into an obligation.
--
-- Empty means the company does not offer flexitime, which is the behaviour
-- every existing installation already has. Nothing is assumed on anyone's
-- behalf, the same way `work_start` being empty means lateness is not judged
-- at all rather than judged against a guess.
--
-- Company policy, not regulation. The Labor Standards Act sets limits on
-- total hours, not on when a company lets people start, so this belongs in
-- `company_setting` beside the other policy values and never in Rust.

INSERT INTO company_setting (code, label, value, note, editable, sort_order) VALUES
    ('flex_window_minutes', '彈性上班區間（分鐘）', '',
     '上班時間之後幾分鐘內開始上班都算準時，下班時間同步順延相同分鐘數，工作總時數不變。留空表示不採彈性工時。',
     true, 95)
ON CONFLICT (code) DO NOTHING;
