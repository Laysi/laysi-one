-- Pushing the bulletin board to the company's chat tool.
--
-- The address is one string with one current value and no effective date,
-- which is what `company_setting` holds; a table of its own would carry the
-- same single row plus a second screen to edit it. Empty is the state every
-- installation starts in and means nothing is pushed.

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('notice_webhook_url', '公告推送網址', '', '發布公告或規章時推送到聊天室，留空不推送', true, 120, now(), '')
ON CONFLICT (code) DO NOTHING;
