-- Where a call came from, and how long the record of it is kept.
--
-- client_ip is text rather than inet because the value arrives in a proxy
-- header that nothing else validates. The server parses it and binds NULL
-- when it is not an address, so the column holds either a real address or
-- nothing; text keeps a malformed header from failing the audit write, which
-- would lose the record of the very request worth keeping.
ALTER TABLE ai_audit_log ADD COLUMN client_ip text;

COMMENT ON COLUMN ai_audit_log.client_ip IS
    '呼叫來源位址，取自前置代理的 X-Forwarded-For。代理沒有帶這個標頭時為空。';

-- Rows older than this are removed once a day. 1825 days is five years, the
-- period the Labor Standards Act requires attendance records to be kept, and
-- an audit record about attendance outlives nothing it describes.
INSERT INTO company_setting
    (code, label, value, note, editable, sort_order, updated_by)
VALUES
    ('audit_keep_days', '稽核紀錄保存天數', '1825',
     '超過這個天數的稽核紀錄每天清除一次。留空表示永久保存。',
     true, 230, 'migration 0003')
ON CONFLICT (code) DO NOTHING;

-- The log now holds sign-ins and password changes as well as tool calls, so
-- both places that name it on screen stop calling it the AI's.
UPDATE mcp_tool_grant SET note = '讀稽核紀錄'
 WHERE tool_name = 'list_ai_audit';

UPDATE permission
   SET note = '查看每次操作與登入的呼叫者、來源位址、參數摘要、結果與被拒絕紀錄'
 WHERE code = 'view_audit';
