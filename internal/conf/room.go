package conf

type RoomConfig struct {
	MustPassword bool   `yaml:"must_password" hc:"must input password to create room" env:"ROOM_MUST_PASSWORD"`
	TTL          string `yaml:"ttl" hc:"set how long the room will be inactive before the memory will be reclaimed"`
}

func DefaultRoomConfig() RoomConfig {
	return RoomConfig{
		MustPassword: false,
		TTL:          "48h",
	}
}
