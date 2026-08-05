-- Separate "a person acting on screen" from "AI calling over MCP"
--
-- mcp_tool_grant used to govern both, with the result that switching
-- post_voucher off for AI also stopped the accountant posting. That was
-- wrong: the grants exist to limit autonomous AI actions, not people who
-- have an identity and can see the consequences.
--
-- With a channel column the audit trail can also answer the question that
-- matters most: who actually did this.

ALTER TABLE ai_audit_log
    ADD COLUMN channel text NOT NULL DEFAULT 'mcp'
        CHECK (channel IN ('ui', 'mcp'));

COMMENT ON COLUMN ai_audit_log.channel IS
    'ui=已登入的人在畫面上操作；mcp=AI 透過 MCP 呼叫。只有 mcp 受 mcp_tool_grant 管制。';

-- The new column defaults to MCP. A production install preloads no audit
-- records, so no backfill is needed.

-- Two new operations.
--   list_accounts  accounts have to be findable before an entry is written,
--                  by AI as well.
--   create_voucher creates an unposted voucher directly. For AI that goes
--                  further than a draft, so it is off by default; accounting
--                  staff on screen use the ui channel and are unaffected.
INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('list_accounts',  true,  'read_only', '讀會計科目表'),
    ('create_voucher', false, 'high_risk', '直接開立待過帳傳票。AI 請改用 draft_voucher。')
ON CONFLICT (tool_name) DO NOTHING;
