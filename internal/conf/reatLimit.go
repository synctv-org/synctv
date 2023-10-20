package conf

type RateLimitConfig struct {
	Enable                bool   `yaml:"enable" lc:"default: false" env:"SERVER_RATE_LIMIT_ENABLE"`
	Period                string `yaml:"period" env:"SERVER_RATE_LIMIT_PERIOD"`
	Limit                 int64  `yaml:"limit" env:"SERVER_RATE_LIMIT_LIMIT"`
	TrustForwardHeader    bool   `yaml:"trust_forward_header" lc:"default: false" hc:"configure the limiter to trust X-Real-IP and X-Forwarded-For headers. Please be advised that using this option could be insecure (ie: spoofed) if your reverse proxy is not configured properly to forward a trustworthy client IP." env:"SERVER_TRUST_FORWARD_HEADER"`
	TrustedClientIPHeader string `yaml:"trusted_client_ip_header" hc:"configure the limiter to use a custom header to obtain user IP. Please be advised that using this option could be insecure (ie: spoofed) if your reverse proxy is not configured properly to forward a trustworthy client IP." env:"SERVER_TRUSTED_CLIENT_IP_HEADER"`
}

func DefaultRateLimitConfig() RateLimitConfig {
	return RateLimitConfig{
		Enable:                false,
		Period:                "1m",
		Limit:                 300,
		TrustForwardHeader:    false,
		TrustedClientIPHeader: "",
	}
}
