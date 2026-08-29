-- A payroll run can be undone while its voucher is still unposted.
--
-- Changing a hire date after payroll ran left the payslip prorated on the old
-- date with no way back: run_payroll skips anyone already holding a payslip
-- for the period, and the voucher belongs to payroll, so discard_voucher
-- refuses it and names the page that owns it. That page had no such action.
-- void_payroll_run is it, and it stops at the ledger: a posted amount is
-- corrected by a difference in a later period, never by a deletion.
INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('void_payroll_run', false, 'high_risk', '刪除一整期的薪資單、扣繳紀錄與待過帳傳票。對 AI 關閉。', now())
ON CONFLICT (tool_name) DO NOTHING;

-- The whole row is restated rather than appended to, because the two tests
-- that hold the registry, permission and grant name sets equal read the
-- migrations as text and only see an INSERT.
INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_payroll', '執行薪資作業', '計算一期薪資並產生待過帳薪資傳票，並在傳票過帳前作廢重算', '{run_payroll,void_payroll_run}', 93)
ON CONFLICT (code) DO UPDATE SET label = EXCLUDED.label, note = EXCLUDED.note, op_names = EXCLUDED.op_names;
