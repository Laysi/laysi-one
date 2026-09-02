-- Editing an unposted voucher.
--
-- Until now a draft awaiting confirmation could only be posted as drafted or
-- deleted whole, so correcting the account an AI guessed meant re-keying the
-- entry. `update_voucher` joins the permission that already holds the other
-- two ways of undoing an unposted entry.

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('adjust_entry', '調整分錄（會計師）', '查閱既有傳票、直接開立待過帳分錄，並修改或刪除開錯而尚未過帳的傳票', '{search_vouchers,create_voucher,update_voucher,discard_voucher}', 40)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('update_voucher', true, 'needs_confirm', '修改還沒過帳的傳票，整份分錄改為送出的內容。', '2026-09-02 00:00:00+00')
ON CONFLICT (tool_name) DO NOTHING;
