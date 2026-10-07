//! Helpers shared by the integration tests that need a database of their own.

/// `url` with its database replaced by `name`, keeping any query string.
pub fn with_database(url: &str, name: &str) -> String {
    let (head, query) = url
        .split_once('?')
        .map_or((url, None), |(head, query)| (head, Some(query)));
    let authority = head.find("://").map_or(0, |i| i + 3);
    let host = head[authority..]
        .rfind('@')
        .map_or(authority, |i| authority + i + 1);
    let base = head[host..].find('/').map_or(head, |i| &head[..host + i]);
    match query {
        Some(query) => format!("{base}/{name}?{query}"),
        None => format!("{base}/{name}"),
    }
}
