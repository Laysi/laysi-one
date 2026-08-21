-- People who had already left kept working logins.
--
-- Completing a leaving flow marked the employee `left` and stopped there, so
-- the login, its live sessions and any MCP token issued to that person all
-- carried on. The session query does check `user_account.status`, but nothing
-- had ever set it, an MCP token does not read it at all, and no scheduled
-- work revisits the effective date. Whoever left before this migration is
-- still signed in.
--
-- Approvals from here on close all three in the transaction that records the
-- departure. This closes the ones already open.
--
-- The last active administrator is the exception, here as in the operation:
-- no operation grants a role, so suspending it would leave the installation
-- with nobody able to open the accounts screen. Such an account keeps its
-- access and has to be handled by a person.

WITH departed AS (
    SELECT u.id, u.employee_id
      FROM user_account u
      JOIN employee e ON e.id = u.employee_id
     WHERE u.status = 'active'
       AND e.status = 'left'
       AND (u.role_code <> 'admin'
            OR EXISTS (SELECT 1
                         FROM user_account o
                         LEFT JOIN employee oe ON oe.id = o.employee_id
                        WHERE o.role_code = 'admin'
                          AND o.status = 'active'
                          AND o.id <> u.id
                          AND (oe.id IS NULL OR oe.status <> 'left')))
), closed AS (
    UPDATE user_account SET status = 'suspended'
     WHERE id IN (SELECT id FROM departed)
 RETURNING id, employee_id
), ended AS (
    UPDATE user_session
       SET revoked_at = now(), revoked_by = '離職結案'
     WHERE user_id IN (SELECT id FROM closed)
       AND revoked_at IS NULL
 RETURNING id
)
UPDATE api_token
   SET revoked_at = now(), revoked_by = '離職結案'
 WHERE revoked_at IS NULL
   AND owner_employee_id IN (SELECT employee_id FROM closed WHERE employee_id IS NOT NULL);
