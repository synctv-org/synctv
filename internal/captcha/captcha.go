package captcha

import (
	"github.com/mojocn/base64Captcha"
)

var Captcha *base64Captcha.Captcha

func init() {
	Captcha = base64Captcha.NewCaptcha(base64Captcha.DefaultDriverDigit, base64Captcha.DefaultMemStore)
}
