-- 「預算與管理」 answered a fresh installation with 「尚無現行的預算版本，請先建立
-- 一版」 and then offered nothing to press: no operation created a version, so
-- the sentence named a step that could not be taken. `create_budget_version`
-- is that step.
--
-- It sits on the same permission and the same floor as `set_budget`. A version
-- is the denominator every utilisation figure is measured against, which makes
-- it the owner's call, and it stays off for AI for the same reason changing a
-- budget amount does: nothing should be able to move its own baseline.

UPDATE permission
   SET op_names = '{set_budget,create_budget_version}'
 WHERE code = 'manage_budget';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('create_budget_version', false, 'boss_only',
     '建立年度預算版本。對 AI 關閉：預算是比較基準，不應讓 AI 自己造一個。')
ON CONFLICT (tool_name) DO NOTHING;
