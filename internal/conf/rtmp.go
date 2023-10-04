package conf

type RtmpConfig struct {
	Enable bool   `yaml:"enable" lc:"enable rtmp server (default: true)" env:"RTMP_ENABLE"`
	Port   uint16 `yaml:"port" lc:"rtmp server port (default use server port)" env:"RTMP_PORT"`

	CustomPublishHost string `yaml:"custom_publish_host" lc:"publish host (default use http header host)" env:"RTMP_CUSTOM_PUBLISH_HOST"`
	RtmpPlayer        bool   `yaml:"rtmp_player" lc:"enable rtmp player (default: false)" env:"RTMP_PLAYER"`
	HlsPlayer         bool   `yaml:"hls_player" lc:"enable hls player (default: false)" env:"HLS_PLAYER"`
}

func DefaultRtmpConfig() RtmpConfig {
	return RtmpConfig{
		Enable:            true,
		Port:              0,
		CustomPublishHost: "",
		RtmpPlayer:        false,
		HlsPlayer:         false,
	}
}
