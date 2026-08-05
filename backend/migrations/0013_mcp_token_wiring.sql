-- Laysi One: MCP now authenticates and takes identity from api_token rows
--
-- When 0007 created the integration settings, MCP was not yet wired to
-- api_token. This updates the integration status now that it is.

UPDATE integration
   SET note = 'Bearer 明文只用來計算 BLAKE3；身分取自未撤銷的 api_token 資料列，成功時更新最後使用時間'
 WHERE code = 'mcp';
