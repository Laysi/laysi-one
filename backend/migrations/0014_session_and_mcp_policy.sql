-- Three policy values move out of the environment and onto a screen.
--
-- They are decisions somebody makes about this installation, not deployment
-- wiring: how long a session may sit idle, how long it may live at all, and
-- how many calls a minute one MCP token gets. Nothing in this product asks a
-- person to edit a file on the host to change a decision.
--
-- They go in company_setting rather than a table of their own: it already has
-- the label, the note, the editable flag, the AdminOnly write path and the
-- screen. A parallel key-value table would be the same machinery twice.
--
-- SESSION_COOKIE_SECURE is not here. It is derived from the request instead:
-- a proxy that terminates TLS says so with X-Forwarded-Proto, and a flag
-- somebody has to remember to flip when the site moves to HTTPS is a flag that
-- ships a session cookie in the clear.
--
-- CORS_ALLOWED_ORIGIN and MCP_ALLOWED_ORIGINS stay in the environment. The
-- first only matters to the Vite dev server; in production the frontend is
-- same-origin behind the web container and no CORS header is involved. The
-- second guards a port whoever deploys chose to publish, which is the same
-- layer as the port number.

INSERT INTO company_setting (code, label, value, note, editable, sort_order, updated_at, updated_by) VALUES
    ('session_idle_minutes', '閒置多久要重新登入（分鐘）', '60',
     '超過這段時間沒有操作就要重新登入。每一次操作都重新計時。', true, 200, now(), 'migration 0014'),
    ('session_max_hours', '一次登入最長時數', '168',
     '從登入那一刻起算的上限，期間有沒有操作都一樣。', true, 210, now(), 'migration 0014'),
    ('mcp_rate_limit_per_minute', 'MCP 每分鐘呼叫上限', '60',
     '單一 Token 每分鐘可呼叫的次數，填 0 表示不限。', true, 220, now(), 'migration 0014')
ON CONFLICT (code) DO NOTHING;
