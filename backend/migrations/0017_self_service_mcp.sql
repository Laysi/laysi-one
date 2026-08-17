-- Laysi One: employees issue their own MCP tokens
--
-- `issue_api_token` is BossOnly and takes `actor` and `actor_role` as input,
-- which together make it unusable for self-service twice over: staff cannot
-- call it, and if they could they would be naming whose authority the token
-- carries. Both are the wrong shape rather than the wrong permission.
--
-- Connecting an assistant to your own work is not a privilege of rank. The
-- three operations added here let anyone with a linked employee record mint a
-- token that acts as themselves, list the tokens they minted, and revoke one.
-- A token issued this way carries the issuer's own role, so it can do exactly
-- what that person can do and never more; a clerk's token is a clerk.
--
-- Ownership needs a key that is not a name. `api_token.actor` is a display
-- name and two people can share one, so a new column records which employee a
-- self-issued token belongs to. Tokens issued by the owner through
-- `issue_api_token` leave it NULL: those represent a person but belong to the
-- installation.

ALTER TABLE api_token
    ADD COLUMN owner_employee_id int REFERENCES employee(id) ON DELETE SET NULL;

COMMENT ON COLUMN api_token.owner_employee_id IS
    '自助核發時的所屬員工。NULL 表示由負責人代發，不屬於任何個人。';

CREATE INDEX api_token_owner_idx ON api_token (owner_employee_id, revoked_at);

-- Permissions

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token}' WHERE code = 'staff_self_service';

UPDATE permission SET note = '只回目前登入帳號所連結員工本人的首頁、薪資、休假、打卡、報支進度與自己核發的 MCP Token' WHERE code = 'staff_self_service';

-- MCP tool grants
--
-- All three are false, and `issue_my_api_token` most firmly. A token is an
-- identity; a tool that lets an assistant mint one would let it extend its own
-- reach past the token it was given, and revoking that token would no longer
-- end the access. Minting stays something a person does at a screen.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('issue_my_api_token', false, 'needs_confirm',
     '以 ctx.employee_id 對應的員工身分核發 MCP Token。讓 AI 自行發 Token 等於讓它替自己延長權限，預設關閉。'),
    ('list_my_api_tokens', false, 'read_only',
     '讀本人自助核發的 Token 與本人在 MCP 上可用的工具清單。預設關閉。'),
    ('revoke_my_api_token', false, 'needs_confirm',
     '撤銷本人自助核發的 Token。預設關閉；人在畫面上撤銷不受此限。')
ON CONFLICT (tool_name) DO NOTHING;
