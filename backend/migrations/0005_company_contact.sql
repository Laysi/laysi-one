-- The company's address and phone number.
--
-- 0001 is applied and frozen, so everything here is additive. Neither is
-- listed in CLEARABLE_SETTINGS: like the company name, a blank one means it
-- has not been filled in yet, not that the company has no address.

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('company_address', '公司地址', '', '', true, 22, now(), ''),
    ('company_phone', '公司電話', '', '', true, 24, now(), '')
ON CONFLICT (code) DO NOTHING;
