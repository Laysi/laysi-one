//! What upload and download links have in common: who holds one, and whether
//! that holder can still act.
//!
//! A link is a credential handed out by an operation and used later, without a
//! session, by whoever has the URL. It never outlives what its holder has
//! lost: every use checks the holder again, in the same terms `/mcp` and the
//! session check use, so revoking a token or suspending an account kills the
//! links it issued.

/// Who a link belongs to: a signed-in account or an MCP token, never both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkHolder {
    Account(i32),
    Token(i32),
}

impl LinkHolder {
    /// The `owner_user_id` and `owner_token_id` columns every link table has.
    pub(crate) fn columns(self) -> (Option<i32>, Option<i32>) {
        match self {
            LinkHolder::Account(id) => (Some(id), None),
            LinkHolder::Token(id) => (None, Some(id)),
        }
    }

    /// The holder a row names. The tables' CHECK rules out neither being
    /// set; a row that slipped past it belongs to nobody, which is what a
    /// token id of 0 matches.
    pub(crate) fn from_columns(user: Option<i32>, token: Option<i32>) -> Self {
        match (user, token) {
            (Some(id), _) => LinkHolder::Account(id),
            (None, Some(id)) => LinkHolder::Token(id),
            (None, None) => LinkHolder::Token(0),
        }
    }
}

/// Whether the holder of link row `b` can still act: an active account under
/// the role the link was issued to, or a token that is neither revoked nor
/// expired and whose owner still qualifies.
///
/// Expects the row aliased `b` with `owner_user_id`, `owner_token_id` and
/// `actor_role`, which both link tables have.
macro_rules! holder_active {
    () => {
        concat!(
            "(CASE
                WHEN b.owner_user_id IS NOT NULL THEN
                  EXISTS (SELECT 1 FROM user_account u
                           WHERE u.id = b.owner_user_id
                             AND u.status = 'active'
                             AND u.role_code = b.actor_role)
                ELSE
                  EXISTS (SELECT 1 FROM api_token t
                           WHERE t.id = b.owner_token_id
                             AND t.revoked_at IS NULL
                             AND t.actor_role = b.actor_role
                             AND (t.rotate_days IS NULL
                                  OR now() < t.created_at + make_interval(days => t.rotate_days))
                             AND ",
            token_owner_active!(),
            ")
              END)"
        )
    };
}
