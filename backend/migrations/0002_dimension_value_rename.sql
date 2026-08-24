-- Departments become editable: update_dimension_value renames one or takes it
-- out of the pickers. Until now the settings screen could only add, so a typo
-- was permanent and showed up on every employee and every P&L by department.
--
-- Sits with the other master-data operations. `active` already exists on
-- dimension_value; nothing could write it.

UPDATE permission
   SET op_names = '{create_account,update_account,create_bank_account,create_dimension_value,update_dimension_value,set_approval_rule}'
 WHERE code = 'manage_masters';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('update_dimension_value', false, 'needs_confirm', '改名或停用分類項目，對 AI 預設關閉。', now());
