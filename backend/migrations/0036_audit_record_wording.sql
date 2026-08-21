-- 「軌跡」 is a translation artefact. Every screen that shows this table now
-- calls it 「稽核紀錄」; the seeded permission label and grant notes are read
-- out on 「角色與核准」 and 「MCP 與 AI」, so they are the same copy and follow.

UPDATE permission
   SET label = '看稽核紀錄',
       note = replace(note, '稽核軌跡', '稽核紀錄')
 WHERE code = 'view_audit';

UPDATE mcp_tool_grant
   SET note = replace(note, '稽核軌跡', '稽核紀錄')
 WHERE note LIKE '%稽核軌跡%';

UPDATE role_permission
   SET note = replace(note, '稽核軌跡', '稽核紀錄')
 WHERE note LIKE '%稽核軌跡%';
