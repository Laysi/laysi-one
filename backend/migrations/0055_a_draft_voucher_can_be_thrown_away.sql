-- A voucher typed in by mistake had only one way out: post it, then void it.
--
-- Two entries in the ledger and an audit trail describing a transaction that
-- never happened, because the screen offered no way to delete a draft. Voiding
-- is for a posted voucher that should not exist; a draft that should not exist
-- has not reached the ledger and can simply go.
--
-- The permission follows adjust_entry rather than post_voucher: whoever may
-- open a draft may throw one away, and that is a smaller authority than
-- posting. Deny-by-default is preserved, no role gains anything new.

UPDATE permission
   SET op_names = '{search_vouchers,create_voucher,discard_voucher}',
       note = '查閱既有傳票、直接開立待過帳分錄，並刪除開錯而尚未過帳的傳票'
 WHERE code = 'adjust_entry';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('discard_voucher', false, 'needs_confirm',
     '刪除後無法復原，AI 預設不得自行判定一張傳票是開錯的。');
