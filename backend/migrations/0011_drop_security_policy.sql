-- Drop get_security_policy: the 控制規則 card it fed was read-only restatement.
--
-- Two of its four rows were constants compiled into the module, and the other
-- two only repeated what 「MCP 與 AI」 and 「角色與核准」 already show next to
-- the switches that change them. Nothing on that card could be acted on where
-- it was drawn. Same test 0059-0062 applied to tables with no write path.
--
-- 「安全與營運」 now opens on list_api_tokens, which is in the same permission.

UPDATE permission
   SET name = '看安全與系統狀態',
       detail = '查看權限對照表、Token 後設資料與系統健康狀態',
       op_names = '{list_roles,list_api_tokens,get_system_health}'
 WHERE code = 'view_system';

DELETE FROM mcp_tool_grant WHERE tool_name = 'get_security_policy';
