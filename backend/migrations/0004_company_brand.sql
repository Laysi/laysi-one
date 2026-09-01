-- The company's own logo, and where it is shown.
--
-- 0001 is applied and frozen, so everything here is additive.
--
-- The image is a data URI in company_setting rather than an object in the
-- bucket: the sign-in screen holds no session, and company_setting is already
-- read by the two endpoints a signed-out browser can reach. A bucket object
-- would need a public route of its own to be visible there.

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('brand_logo', '公司標誌', '', '', true, 11, now(), ''),
    ('brand_display', '公司識別顯示方式', '名稱', '', true, 12, now(), '')
ON CONFLICT (code) DO NOTHING;

COMMENT ON COLUMN company_setting.value IS
    'brand_logo 是 data:image/…;base64 的圖檔內容，其餘設定是短字串。';
