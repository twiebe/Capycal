/// Resolved authentication method for a CalDAV client.
#[derive(Clone)]
pub struct ResolvedAuth {
    pub username: String,
    pub password: String,
}
