package email

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/url"
	"text/template"
	"time"

	"github.com/Boostport/mjml-go"
	log "github.com/sirupsen/logrus"
	email_template "github.com/synctv-org/synctv/internal/email/template"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/synccache"
	"github.com/zijiren233/stream"
)

var (
	ErrEmailNotEnabled                                      = errors.New("email is not enabled")
	emailCaptcha       *synccache.SyncCache[string, string] = synccache.NewSyncCache[string, string](time.Minute * 5)
)

var (
	EnableEmail = settings.NewBoolSetting(
		"enable_email",
		false,
		model.SettingGroupEmail,
		settings.WithAfterSetBool(func(bs settings.BoolSetting, b bool) {
			if !b {
				closeSmtpPool()
			}
		}),
	)
	DisableUserSignup = settings.NewBoolSetting(
		"email_disable_user_signup",
		false,
		model.SettingGroupEmail,
	)
	SignupNeedReview = settings.NewBoolSetting(
		"email_signup_need_review",
		false,
		model.SettingGroupEmail,
	)
	EmailSignupWhiteListEnable = settings.NewBoolSetting(
		"email_signup_white_list_enable",
		false,
		model.SettingGroupEmail,
	)
	EmailSignupWhiteList = settings.NewStringSetting(
		"email_signup_white_list",
		`gmail.com,qq.com,163.com,yahoo.com,sina.com,126.com,outlook.com,yeah.net,foxmail.com`,
		model.SettingGroupEmail,
	)
)

var (
	testTemplate             *template.Template
	captchaTemplate          *template.Template
	retrievePasswordTemplate *template.Template
)

func init() {
	body, err := mjml.ToHTML(
		context.Background(),
		stream.BytesToString(email_template.TestMjml),
		mjml.WithMinify(true),
	)
	if err != nil {
		log.Fatalf("mjml test template error: %v", err)
	}
	t, err := template.New("").Parse(body)
	if err != nil {
		log.Fatalf("parse test template error: %v", err)
	}
	testTemplate = t

	body, err = mjml.ToHTML(
		context.Background(),
		stream.BytesToString(email_template.CaptchaMjml),
		mjml.WithMinify(true),
	)
	if err != nil {
		log.Fatalf("mjml captcha template error: %v", err)
	}
	t, err = template.New("").Parse(body)
	if err != nil {
		log.Fatalf("parse captcha template error: %v", err)
	}
	captchaTemplate = t

	body, err = mjml.ToHTML(
		context.Background(),
		stream.BytesToString(email_template.RetrievePasswordMjml),
		mjml.WithMinify(true),
	)
	if err != nil {
		log.Fatalf("mjml retrieve password template error: %v", err)
	}
	t, err = template.New("").Parse(body)
	if err != nil {
		log.Fatalf("parse retrieve password template error: %v", err)
	}
	retrievePasswordTemplate = t
}

type testPayload struct {
	Username string
	Year     int
}

type captchaPayload struct {
	Captcha string

	Year int
}

type retrievePasswordPayload struct {
	Host string
	Url  string

	Year int
}

func SendBindCaptchaEmail(userID, userEmail string) error {
	if !EnableEmail.Get() {
		return ErrEmailNotEnabled
	}

	if userID == "" {
		return errors.New("user id is empty")
	}

	if userEmail == "" {
		return errors.New("email is empty")
	}

	pool, err := getSmtpPool()
	if err != nil {
		return err
	}

	entry, loaded := emailCaptcha.LoadOrStore(
		fmt.Sprintf("bind:%s:%s", userID, userEmail),
		utils.RandString(6),
		time.Minute*5,
	)
	if loaded {
		entry.SetExpiration(time.Now().Add(time.Minute * 5))
	}

	out := bytes.NewBuffer(nil)
	err = captchaTemplate.Execute(out, captchaPayload{
		Captcha: entry.Value(),
		Year:    time.Now().Year(),
	})
	if err != nil {
		return err
	}

	return pool.SendEmail(
		[]string{userEmail},
		"SyncTV Verification Code",
		out.String(),
	)
}

func VerifyBindCaptchaEmail(userID, userEmail, captcha string) (bool, error) {
	if !EnableEmail.Get() {
		return false, ErrEmailNotEnabled
	}

	if userID == "" {
		return false, errors.New("user id is empty")
	}

	if userEmail == "" {
		return false, errors.New("email is empty")
	}

	if captcha == "" {
		return false, errors.New("captcha is empty")
	}

	key := fmt.Sprintf("bind:%s:%s", userID, userEmail)

	if emailCaptcha.CompareValueAndDelete(
		key,
		captcha,
	) {
		return true, nil
	}

	return false, nil
}

func SendTestEmail(username, email string) error {
	if email == "" {
		return errors.New("email is empty")
	}

	pool, err := getSmtpPool()
	if err != nil {
		return err
	}

	out := bytes.NewBuffer(nil)
	err = testTemplate.Execute(out, testPayload{
		Username: username,
		Year:     time.Now().Year(),
	})
	if err != nil {
		return err
	}

	return pool.SendEmail(
		[]string{email},
		"SyncTV Test Email",
		out.String(),
	)
}

func SendSignupCaptchaEmail(email string) error {
	if !EnableEmail.Get() {
		return ErrEmailNotEnabled
	}

	if email == "" {
		return errors.New("email is empty")
	}

	pool, err := getSmtpPool()
	if err != nil {
		return err
	}

	entry, loaded := emailCaptcha.LoadOrStore(
		fmt.Sprintf("signup:%s", email),
		utils.RandString(6),
		time.Minute*5,
	)
	if loaded {
		entry.SetExpiration(time.Now().Add(time.Minute * 5))
	}

	out := bytes.NewBuffer(nil)
	err = captchaTemplate.Execute(out, captchaPayload{
		Captcha: entry.Value(),
		Year:    time.Now().Year(),
	})
	if err != nil {
		return err
	}

	return pool.SendEmail(
		[]string{email},
		"SyncTV Signup Verification Code",
		out.String(),
	)
}

func VerifySignupCaptchaEmail(email, captcha string) (bool, error) {
	if !EnableEmail.Get() {
		return false, ErrEmailNotEnabled
	}

	if email == "" {
		return false, errors.New("email is empty")
	}

	if captcha == "" {
		return false, errors.New("captcha is empty")
	}

	if emailCaptcha.CompareValueAndDelete(
		fmt.Sprintf("signup:%s", email),
		captcha,
	) {
		return true, nil
	}

	return false, nil
}

func SendRetrievePasswordCaptchaEmail(userID, email, host string) error {
	if !EnableEmail.Get() {
		return ErrEmailNotEnabled
	}

	if userID == "" {
		return errors.New("user id is empty")
	}

	if email == "" {
		return errors.New("email is empty")
	}

	u, err := url.Parse(host)
	if err != nil {
		return err
	}
	u.Path = `/web/auth/reset`

	pool, err := getSmtpPool()
	if err != nil {
		return err
	}

	entry, loaded := emailCaptcha.LoadOrStore(
		fmt.Sprintf("retrieve_password:%s:%s", userID, email),
		utils.RandString(6),
		time.Minute*5,
	)
	if loaded {
		entry.SetExpiration(time.Now().Add(time.Minute * 5))
	}

	q := u.Query()
	q.Set("captcha", entry.Value())
	q.Set("email", email)
	u.RawQuery = q.Encode()

	out := bytes.NewBuffer(nil)
	err = retrievePasswordTemplate.Execute(out, retrievePasswordPayload{
		Host: host,
		Url:  u.String(),
		Year: time.Now().Year(),
	})
	if err != nil {
		return err
	}

	return pool.SendEmail(
		[]string{email},
		"SyncTV Retrieve Password Verification Code",
		out.String(),
	)
}

func VerifyRetrievePasswordCaptchaEmail(userID, email, captcha string) (bool, error) {
	if !EnableEmail.Get() {
		return false, ErrEmailNotEnabled
	}

	if userID == "" {
		return false, errors.New("user id is empty")
	}

	if email == "" {
		return false, errors.New("email is empty")
	}

	if captcha == "" {
		return false, errors.New("captcha is empty")
	}

	if emailCaptcha.CompareValueAndDelete(
		fmt.Sprintf("retrieve_password:%s:%s", userID, email),
		captcha,
	) {
		return true, nil
	}

	return false, nil
}
