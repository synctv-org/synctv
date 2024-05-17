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
	ENV_PREFIX = "SYNCTV_"
)
