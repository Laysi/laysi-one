-- The account a connector token was authorised from.
--
-- `/mcp` accepts a token only while its owner holds an active account under
-- the role the token acts as. `owner_employee_id` cannot name that owner for
-- an account with no roster row, the bootstrap `admin` above all, so a
-- connector authorised from one looked like a token issued on the
-- installation's behalf and outlived its account.
--
-- No foreign key: a deleted account has to leave an id that matches nothing,
-- which refuses the token. ON DELETE SET NULL would turn the token into an
-- installation token instead, and CASCADE would drop the row the token list
-- still shows.

ALTER TABLE api_token ADD COLUMN owner_user_id integer;

COMMENT ON COLUMN api_token.owner_user_id IS
    '以外部連接器授權時的登入帳號。該帳號停用、刪除或變更角色後，這組 Token 立刻失效。';

-- Connectors authorised earlier cannot be traced back to their account: the
-- row keeps only a display name, which two accounts can share, and an
-- employee, who can hold more than one account. Left on the employee they
-- would come back with any later account of that person, which a connector
-- bound to its account never does. They are revoked; the connector asks to be
-- authorised again on its next call.
--
-- The audit row written at authorisation tells them apart from installation
-- and self-issued tokens, but audit rows age out once retention is shortened,
-- and the write is best effort. A connector from an account with no roster
-- row is the one that would then pass as an installation token, so a token in
-- the shape every connector has (MCP, no owner, no rotation period, the scope
-- a connector writes, acting as the one who created it) is revoked unless the
-- audit also holds the `issue_api_token` row that issued it, whose summary
-- names the token and its prefix in this form since that operation first
-- existed. The self-issue and security screens write other shapes. A
-- connector that names an employee and has lost its audit row stays on the
-- employee, where it acts as no more than that person's current role.
UPDATE api_token t SET revoked_at = now(), revoked_by = '系統更新'
 WHERE t.revoked_at IS NULL
   AND (t.id IN (SELECT (a.params->>'token_id')::int FROM ai_audit_log a
                  WHERE a.tool_name = 'oauth_token'
                    AND a.params->>'token_id' ~ '^[0-9]+$')
        OR (t.kind = 'mcp'
            AND t.owner_employee_id IS NULL
            AND t.scope = '與本人相同'
            AND t.rotate_days IS NULL
            AND t.created_by = t.actor
            AND NOT EXISTS (SELECT 1 FROM ai_audit_log a
                             WHERE a.tool_name = 'issue_api_token'
                               AND a.outcome = 'ok'
                               AND strpos(a.result_summary,
                                          t.name || '（' || t.prefix || '…）') > 0)));
