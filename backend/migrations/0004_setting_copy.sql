-- Wording of the settings screen.
--
-- The seeded label and note are screen text, and several of them restated
-- their own label ("會計年度起始月份，填 1 到 12"), described a constraint the
-- widget already enforces, or spelled a unit differently from the row above.
-- What survives is the part no control on screen can show: what an empty
-- value means, and what cannot be taken back.
--
-- Only `value` is ever written by an operation, so label and note can be
-- restated here without losing an administrator's edit.
UPDATE company_setting SET label = '閒置登出（分鐘）', note = ''
  WHERE code = 'session_idle_minutes';

UPDATE company_setting SET label = '單次登入上限（小時）', note = ''
  WHERE code = 'session_max_hours';

UPDATE company_setting SET label = '遲到寬限（分鐘）', note = '留空等於 0。'
  WHERE code = 'late_grace_minutes';

UPDATE company_setting SET label = '補登申請處理方式', note = ''
  WHERE code = 'punch_correction_policy';

UPDATE company_setting SET note = '出勤日期與單據日期依這個時區判定。有打卡紀錄之後不能變更。'
  WHERE code = 'business_utc_offset';

UPDATE company_setting SET note = '' WHERE code IN ('fiscal_year', 'currency');

UPDATE company_setting SET note = '執行率超過這個百分比就示警；留空不示警。'
  WHERE code = 'budget_alert_percent';

UPDATE company_setting SET note = '上班時間之後這幾分鐘內到班都算準時，下班時間順延相同分鐘數。留空不採彈性工時。'
  WHERE code = 'flex_window_minutes';

UPDATE company_setting SET note = '留空不判定遲到，也不計加班時數。' WHERE code = 'work_start';
UPDATE company_setting SET note = '留空不計加班時數。' WHERE code = 'work_end';
UPDATE company_setting SET note = '留空不扣除休息時間。' WHERE code = 'break_start';
UPDATE company_setting SET note = '兩端都填了才扣除。' WHERE code = 'break_end';

UPDATE company_setting SET note = '以單一 Token 計算，填 0 不限。'
  WHERE code = 'mcp_rate_limit_per_minute';
