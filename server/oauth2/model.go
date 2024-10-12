package auth

type CallbackType string

const (
	CallbackTypeAuth CallbackType = "auth"
	CallbackTypeBind CallbackType = "bind"
)
