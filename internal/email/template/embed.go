package email_template

import _ "embed"

var (
	//go:embed test.mjml
	TestMjml []byte

	//go:embed captcha.mjml
	CaptchaMjml []byte

	//go:embed retrieve_password.mjml
	RetrievePasswordMjml []byte
)
