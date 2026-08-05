-- Laysi One: base chart of accounts and MCP tool grants
--
-- The file name follows the existing migration numbering, but it creates
-- only the accounts and tool switches the system needs to run. It preloads
-- no company's bank accounts, vouchers or audit records.

-- Accounts

INSERT INTO account (code, name, kind, normal_side, sort_order) VALUES
    ('1101', '現金',           'asset',     'debit',  10),
    ('1102', '銀行存款',       'asset',     'debit',  20),
    ('1131', '應收帳款',       'asset',     'debit',  30),
    ('1251', '存貨',           'asset',     'debit',  40),
    ('1451', '預付費用',       'asset',     'debit',  50),
    ('1601', '辦公設備',       'asset',     'debit',  60),
    ('2101', '應付帳款',       'liability', 'credit', 110),
    ('2171', '應付費用',       'liability', 'credit', 120),
    ('2117', '代扣款',         'liability', 'credit', 130),
    ('2211', '代收營業稅',     'liability', 'credit', 140),
    ('3101', '股本',           'equity',    'credit', 210),
    ('3201', '保留盈餘',       'equity',    'credit', 220),
    ('4111', '銷貨收入',       'revenue',   'credit', 310),
    ('5111', '銷貨成本',       'expense',   'debit',  410),
    ('6111', '薪資支出',       'expense',   'debit',  510),
    ('6115', '租金支出',       'expense',   'debit',  520),
    ('6118', '水電費',         'expense',   'debit',  530),
    ('6122', '旅費',           'expense',   'debit',  540),
    ('6131', '郵電費',         'expense',   'debit',  550),
    ('6134', '郵電費（快遞）', 'expense',   'debit',  560),
    ('6142', '廣告費',         'expense',   'debit',  570),
    ('6152', '折舊費用',       'expense',   'debit',  580),
    ('6188', '修繕費',         'expense',   'debit',  590),
    ('6199', '其他費用',       'expense',   'debit',  600);

-- MCP tool grants
-- post_voucher is off by default: letting AI post entries on its own is
-- something this system deliberately does not do.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('get_dashboard',     true,  'read_only',     '讀總覽數字'),
    ('get_trial_balance', true,  'read_only',     '讀試算表、科目餘額'),
    ('get_cash_position', true,  'read_only',     '讀各銀行餘額與現金水位'),
    ('search_vouchers',   true,  'read_only',     '查傳票、憑證明細'),
    ('list_ai_audit',     true,  'read_only',     '讀 AI 稽核軌跡'),
    ('list_regulations',  true,  'read_only',     '讀目前適用的法規版本'),
    ('draft_voucher',     true,  'needs_confirm', '草擬分錄，存成待確認傳票'),
    ('post_voucher',      false, 'high_risk',     '直接過帳。預設關閉，要開請想清楚。');
