-- The settings screen drops its explanatory notes.
--
-- Every stored note either restated its own label, spelled out a range the
-- picker already limits, or described what an empty value means in wording
-- nobody read. The controls answer all three: fiscal_year is a 1-12 picker,
-- the clock fields are hour and minute selects, and a setting left empty
-- shows 「（尚未設定）」. Labels carry their unit instead.
UPDATE company_setting SET note = '';

UPDATE company_setting SET label = '稽核紀錄保存（天）' WHERE code = 'audit_keep_days';
UPDATE company_setting SET label = '預算提醒門檻（%）' WHERE code = 'budget_alert_percent';
UPDATE company_setting SET label = '彈性上班區間（分鐘）' WHERE code = 'flex_window_minutes';
UPDATE company_setting SET label = 'MCP 每分鐘呼叫上限' WHERE code = 'mcp_rate_limit_per_minute';
