-- reg_ruleset becomes the authority instead of a projection.
--
-- Rates, brackets and statutory ceilings were embedded in the binary and this
-- table was overwritten from them on every boot, so an installation that had
-- to apply a mid-year amendment had to wait for a release. The rule files are
-- now only the factory set: the server fills the table while it is empty,
-- then reads it back and computes from it. `save_regulation` writes one
-- version and reinstalls, so an amendment applies without a restart, and a
-- superseded version stays deleted.
--
-- Reading regulation and amending it are different jobs, so list_regulations
-- moves out of the tax permission into its own group: accounting and the
-- owner read the rates they file with, and the AdminOnly floor keeps the
-- writes with whoever runs the installation.

UPDATE permission
    SET op_names = '{get_vat_return,list_withholding,compute_withholding}',
        label = '稅務試算',
        note = '營業稅申報與各類所得扣繳明細及試算'
    WHERE code = 'manage_tax';

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_regulations', '法規版本', '查看每個領域現行與歷來的費率、級距與上限；修改限系統管理員', '{list_regulations,save_regulation,delete_regulation}', 56)
ON CONFLICT (code) DO NOTHING;

INSERT INTO role_permission (role_code, permission_code, level, note, updated_at, updated_by) VALUES
    ('admin', 'manage_regulations', 'full', '修法後由系統管理員新增版本。', now(), 'migration 0009'),
    ('boss', 'manage_regulations', 'full', '負責人保留這組操作的完整權限。', now(), 'migration 0009'),
    ('acct', 'manage_regulations', 'full', '會計申報前要查得到適用的費率。', now(), 'migration 0009')
ON CONFLICT (role_code, permission_code) DO NOTHING;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('save_regulation', false, 'admin_only', '改法規費率與級距。對 AI 關閉：修法是人的判斷。', now()),
    ('delete_regulation', false, 'admin_only', '刪除法規版本。對 AI 關閉。', now())
ON CONFLICT (tool_name) DO NOTHING;

UPDATE mcp_tool_grant SET note = '讀法規版本與每一版的內容' WHERE tool_name = 'list_regulations';
