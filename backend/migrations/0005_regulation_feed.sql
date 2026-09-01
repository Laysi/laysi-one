-- Amended rates arrive over HTTPS, not in a new image.
--
-- 0001 is applied and frozen, so everything here is additive.
--
-- `refresh_regulations` fetches the vendor's published amendments and writes
-- the ones this database has not seen. It joins the permission that already
-- governs 「法規版本」: reading, saving and deleting a version are the same
-- job, and the identity floor on the operation is what keeps it to the
-- administrator.

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_regulations', '法規版本', '查看每個領域現行與歷來的費率、級距與上限；修改與更新限系統管理員', '{list_regulations,save_regulation,delete_regulation,refresh_regulations}', 56)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('refresh_regulations', false, 'admin_only', '取回廠商發布的修法版本並寫入。對 AI 關閉：採用哪一版費率是人的判斷。', '2026-09-02 00:00:00+00')
ON CONFLICT (tool_name) DO NOTHING;

COMMENT ON TABLE reg_update IS
    '已套用的法規更新，一列一版。鍵是發布端給的識別字，記住的是修法本身而不是它寫下的那一列，所以管理員刪掉的版本不會在下一次更新時回來。';
