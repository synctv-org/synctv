use super::*;

fn format_socket_address(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.starts_with('[') && host.ends_with(']') {
        format!("{host}:{port}")
    } else if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

impl AppConfig {
    /// Get database URL
    #[must_use]
    pub fn database_url(&self) -> String {
        if !self.database.url.trim().is_empty() {
            return self.database.url.clone();
        }

        if self.database.host.trim().is_empty()
            || self.database.port == 0
            || self.database.username.trim().is_empty()
            || self.database.name.trim().is_empty()
        {
            return String::new();
        }

        build_url_from_split_parts(
            "postgresql",
            &self.database.host,
            self.database.port,
            Some(&self.database.username),
            Some(&self.database.password),
            Some(&self.database.name),
        )
    }

    /// Get optional read-only database URL.
    #[must_use]
    pub fn database_read_url(&self) -> Option<String> {
        let read_url = self.database.read_url.trim();
        if !read_url.is_empty() {
            return Some(read_url.to_string());
        }
        let read_host = self.database.read_host.trim();
        if read_host.is_empty() {
            return None;
        }
        let primary_url = self.database_url();
        if primary_url.trim().is_empty() {
            return None;
        }
        let Ok(mut url) = url::Url::parse(&primary_url) else {
            return None;
        };
        if url.set_host(Some(read_host)).is_err() {
            return None;
        }
        let read_port = if self.database.read_port == 0 {
            self.database.port
        } else {
            self.database.read_port
        };
        if read_port != 0 && url.set_port(Some(read_port)).is_err() {
            return None;
        }
        Some(url.to_string())
    }

    /// Get Redis URL
    #[must_use]
    pub fn redis_url(&self) -> String {
        if !self.redis.url.trim().is_empty() {
            return self.redis.url.clone();
        }

        if self.redis.host.trim().is_empty() || self.redis.port == 0 {
            return String::new();
        }

        if !self.redis.username.is_empty() {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                Some(&self.redis.username),
                Some(&self.redis.password),
                Some(&self.redis.database.to_string()),
            )
        } else if !self.redis.password.is_empty() {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                Some(""),
                Some(&self.redis.password),
                Some(&self.redis.database.to_string()),
            )
        } else {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                None,
                None,
                Some(&self.redis.database.to_string()),
            )
        }
    }

    /// Whether cross-replica cluster runtime is enabled.
    ///
    /// `cluster.enabled` is the single source of truth for activating
    /// multi-replica coordination (leader election, cluster pub/sub,
    /// discovery, and other cross-node runtime services). Standalone
    /// deployments may still configure Redis for caching and other
    /// non-cluster features.
    #[must_use]
    pub const fn cluster_runtime_enabled(&self) -> bool {
        self.cluster.enabled
    }

    /// Get unified API address
    #[must_use]
    pub fn api_address(&self) -> String {
        format_socket_address(&self.server.host, self.server.port)
    }

    /// Get the RTMP listener address.
    #[must_use]
    pub fn livestream_address(&self) -> String {
        format_socket_address(&self.server.host, self.livestream.rtmp_port)
    }

    /// Get dedicated metrics address.
    #[must_use]
    pub fn metrics_address(&self) -> String {
        format_socket_address(&self.metrics.host, self.metrics.port)
    }

    #[must_use]
    pub fn health_address(&self) -> String {
        format_socket_address(&self.health.host, self.health.port)
    }

    #[must_use]
    pub fn cluster_address(&self) -> String {
        format_socket_address(&self.cluster.host, self.cluster.port)
    }

    #[must_use]
    pub fn advertise_cluster_address(&self) -> String {
        let host = if self.cluster.advertise_host.trim().is_empty() {
            self.advertise_host()
        } else {
            self.cluster.advertise_host.clone()
        };
        let port = if self.cluster.advertise_port == 0 {
            self.cluster.port
        } else {
            self.cluster.advertise_port
        };
        format_socket_address(&host, port)
    }

    /// Get the dedicated management endpoint used by local/operational CLI commands.
    #[must_use]
    pub fn management_endpoint(&self) -> String {
        match self.management.transport {
            ManagementTransport::Tcp => format!("http://127.0.0.1:{}", self.management.port),
            ManagementTransport::Unix => format!("unix://{}", self.management.unix_socket_path),
        }
    }

    /// Get the management listener target for logs and binding.
    #[must_use]
    pub fn management_bind_target(&self) -> String {
        match self.management.transport {
            ManagementTransport::Tcp => format!("127.0.0.1:{}", self.management.port),
            ManagementTransport::Unix => self.management.unix_socket_path.clone(),
        }
    }

    /// Resolve the advertise host for cluster node registration.
    ///
    /// Priority: `server.advertise_host` initialization value > system hostname
    /// > `server.host`. This address must be routable from other nodes in
    /// > distributed mode.
    #[must_use]
    pub fn advertise_host(&self) -> String {
        if !self.server.advertise_host.is_empty() {
            return self.server.advertise_host.clone();
        }

        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| self.server.host.clone())
    }

    fn local_publish_host(&self) -> String {
        let host = match self.server.host.trim() {
            "" | "0.0.0.0" => "127.0.0.1".to_string(),
            "::" | "[::]" => "::1".to_string(),
            host => host.to_string(),
        };

        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        }
    }

    /// Get the public API address advertised by livestream metadata.
    #[must_use]
    pub fn advertise_api_address(&self) -> String {
        format_socket_address(&self.advertise_host(), self.server.port)
    }

    /// Public RTMP host for publisher-facing URLs.
    #[must_use]
    pub fn public_rtmp_host(&self) -> String {
        self.public_rtmp_host_without_internal_advertise_fallback()
    }

    pub(super) fn public_rtmp_host_without_internal_advertise_fallback(&self) -> String {
        if !self.livestream.public_rtmp_host.is_empty() {
            return self.livestream.public_rtmp_host.clone();
        }

        self.local_publish_host()
    }
}
