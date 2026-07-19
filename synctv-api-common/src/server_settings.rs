pub const DEFAULT_PROJECT_URL: &str = "https://github.com/synctv-org/synctv";

#[derive(Debug, Clone)]
pub struct ApiServerSettings {
    pub bind_address: String,
    pub project_url: String,
    pub trusted_proxies: Vec<String>,
    pub cors_allowed_origins: Vec<String>,
    pub grpc_max_message_size_bytes: usize,
    pub grpc_compression_enabled: bool,
    pub enable_reflection: bool,
}

impl Default for ApiServerSettings {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:8080".to_string(),
            project_url: DEFAULT_PROJECT_URL.to_string(),
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            enable_reflection: false,
        }
    }
}

impl ApiServerSettings {
    #[must_use]
    pub fn is_trusted_proxy(&self, ip: &std::net::IpAddr) -> bool {
        self.trusted_proxies.iter().any(|proxy| {
            proxy
                .parse::<ipnet::IpNet>()
                .is_ok_and(|network| network.contains(ip))
                || proxy
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|proxy_ip| proxy_ip == *ip)
        })
    }
}

pub fn validate_cors_origin(origin: &str) -> Result<(), String> {
    let parsed = url::Url::parse(origin)
        .map_err(|_| format!("CORS origin '{origin}' is not a valid URL"))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "CORS origin '{origin}' must use http:// or https://"
        ));
    }

    if parsed.host_str().is_none() {
        return Err(format!("CORS origin '{origin}' must include a host"));
    }

    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!(
            "CORS origin '{origin}' must not include a path, query, or fragment"
        ));
    }

    Ok(())
}
