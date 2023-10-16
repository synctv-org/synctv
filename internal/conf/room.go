package conf

type RoomConfig struct {
	MustPassword bool `yaml:"must_password" lc:"must input password to create room (default: false)" env:"ROOM_MUST_PASSWORD"`
}

func DefaultRoomConfig() RoomConfig {
	return RoomConfig{
		MustPassword: false,
	}
}
