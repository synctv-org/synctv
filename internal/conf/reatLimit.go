package conf

//nolint:tagliatelle
type RateLimitConfig struct {
	Enable                bool   `env:"SERVER_RATE_LIMIT_ENABLE"                   lc:"default: false" yaml:"enable"`
	Period                string `env:"SERVER_RATE_LIMIT_PERIOD"                                       yaml:"period"`
	Limit                 int64  `env:"SERVER_RATE_LIMIT_LIMIT"                                        yaml:"limit"`
	TrustForwardHeader    bool   `env:"SERVER_RATE_LIMIT_TRUST_FORWARD_HEADER"     lc:"default: false" yaml:"trust_forward_header"     hc:"configure the limiter to trust X-Real-IP and X-Forwarded-For headers. Please be advised that using this option could be insecure (ie: spoofed) if your reverse proxy is not configured properly to forward a trustworthy client IP."`
	TrustedClientIPHeader string `env:"SERVER_RATE_LIMIT_TRUSTED_CLIENT_IP_HEADER"                     yaml:"trusted_client_ip_header" hc:"configure the limiter to use a custom header to obtain user IP. Please be advised that using this option could be insecure (ie: spoofed) if your reverse proxy is not configured properly to forward a trustworthy client IP."`
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
