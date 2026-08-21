-- The fixed asset module could be read but never written.
--
-- `list_fixed_assets`, `preview_depreciation` and `run_depreciation` shipped
-- in 0005 with no way to put an asset on the register: no operation, no MCP
-- tool, no import source. A company that bought a machine could do nothing on
-- that screen, while the screen went on explaining what the depreciation
-- voucher would look like.
--
-- `create_fixed_asset` writes the register row and the acquisition voucher in
-- one transaction, the same shape as `create_ap_bill`: cost before tax to the
-- asset account, deductible input tax to 1284, the total credited to whatever
-- account the money left. It is `needs_confirm` and on for AI for the same
-- reason `run_depreciation` is: the voucher it produces is `pending` and a
-- person still has to post it.
--
-- Bought on credit, the purchase belongs on the payables screen with 1601 as
-- its debit account; the asset then points at that bill's voucher instead of
-- getting one of its own. Crediting 2101 here would put a payable in the
-- ledger with no row in 應付帳款, and that screen reconciles the two.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('create_fixed_asset', true, 'needs_confirm',
     '登記固定資產並產生取得傳票（借資產科目與 1284 進項稅額／貸付款科目）')
ON CONFLICT (tool_name) DO NOTHING;

UPDATE permission
   SET op_names = '{list_fixed_assets,create_fixed_asset,preview_depreciation,run_depreciation}',
       note = '登記資產、查看資產、預覽折舊並產生一期待過帳折舊傳票'
 WHERE code = 'manage_assets';
