package conf

type RtmpConfig struct {
	Enable bool   `yaml:"enable" env:"RTMP_ENABLE"`
	Port   uint16 `yaml:"port" lc:"default use server port" env:"RTMP_PORT"`

	CustomPublishHost string `yaml:"custom_publish_host" lc:"default use http header host" env:"RTMP_CUSTOM_PUBLISH_HOST"`
	RtmpPlayer        bool   `yaml:"rtmp_player" hc:"can watch live streams through the RTMP protocol (without authentication, insecure)." env:"RTMP_PLAYER"`
}

func DefaultRtmpConfig() RtmpConfig {
	return RtmpConfig{
		Enable:            true,
		Port:              0,
		CustomPublishHost: "",
		RtmpPlayer:        false,
	}
}
