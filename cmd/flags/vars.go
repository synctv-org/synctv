package flags

var (
	// Global
	EnvNoPrefix bool
	SkipEnvFlag bool
	Global      GlobalFlags

	// Server
	Server ServerFlags
)

const (
	EnvPrefix = "SYNCTV_"
)
