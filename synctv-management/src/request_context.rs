#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestContext {
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}
