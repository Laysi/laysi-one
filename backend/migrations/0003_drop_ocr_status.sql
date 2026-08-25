-- Drop document_version.ocr_status. There is no OCR engine and no plan for
-- one, so the column only ever held 'pending' or 'not_applicable' — a value
-- derived from the content type at upload and never read back for a decision.
-- The document screen printed it as a stat card and a per-row label, both of
-- which say the same thing on every install.

ALTER TABLE document_version DROP COLUMN ocr_status;

-- The MCP switch's note is read on screen; it named a status that no longer
-- exists.
UPDATE mcp_tool_grant
   SET note = '讀文件清單與 RustFS 用量'
 WHERE tool_name = 'list_documents';
