-- Whether the owner may decide the leave, overtime, punch-correction and
-- expense requests they submitted themselves.
--
-- Everyone else's requests reach the owner, who holds every approval
-- permission, so only the owner's own can stall. The seed keeps a second
-- signature; the value is the word the screen prints.

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('owner_self_decide', '負責人自行核決', '由他人簽核', '', true, 115, now(), '')
ON CONFLICT (code) DO NOTHING;
