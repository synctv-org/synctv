package conf

type RoomConfig struct {
	MustPassword bool `yaml:"must_password" hc:"must input password to create room" env:"ROOM_MUST_PASSWORD"`
}

func DefaultRoomConfig() RoomConfig {
	return RoomConfig{
		MustPassword: false,
	}
}
