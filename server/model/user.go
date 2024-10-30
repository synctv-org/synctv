package model

import (
	"errors"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
)

var (
	ErrEmptyUserId            = errors.New("empty user id")
	ErrEmptyUsername          = errors.New("empty username")
	ErrUsernameTooLong        = errors.New("username too long")
	ErrUsernameHasInvalidChar = errors.New("username has invalid char")
)

type SetUserPasswordReq struct {
	Password string `json:"password"`
}

func (s *SetUserPasswordReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SetUserPasswordReq) Validate() error {
	if s.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(s.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alnumPrintReg.MatchString(s.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}

type LoginUserReq struct {
	Username string `json:"username"`
	Email    string `json:"email"`
	Password string `json:"password"`
}

func (l *LoginUserReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(l)
}

func (l *LoginUserReq) Validate() error {
	if l.Username != "" && l.Email != "" {
		return errors.New("only one of username or email should be provided")
	}

	if l.Username == "" && l.Email == "" {
		return errors.New("either username or email is required")
	}

	if l.Username != "" {
		if len(l.Username) > 32 {
			return ErrUsernameTooLong
		}
		if !alnumPrintHanReg.MatchString(l.Username) {
			return ErrUsernameHasInvalidChar
		}
	}

	if l.Email != "" {
		if !emailReg.MatchString(l.Email) {
			return errors.New("invalid email format")
		}
	}

	if l.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(l.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alnumPrintReg.MatchString(l.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}

type UserSignupPasswordReq struct {
	Username string `json:"username"`
	Password string `json:"password"`
}

func (u *UserSignupPasswordReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

func (u *UserSignupPasswordReq) Validate() error {
	if u.Username == "" {
		return errors.New("username is empty")
	}
	if len(u.Username) > 32 {
		return ErrUsernameTooLong
	}
	if !alnumPrintHanReg.MatchString(u.Username) {
		return ErrUsernameHasInvalidChar
	}
	if u.Password == "" {
		return FormatEmptyPasswordError("user")
	}
	if len(u.Password) > 32 {
		return ErrPasswordTooLong
	}
	if !alnumPrintReg.MatchString(u.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}

type UserInfoResp struct {
	ID        string       `json:"id"`
	Username  string       `json:"username"`
	Email     string       `json:"email"`
	CreatedAt int64        `json:"createdAt"`
	Role      dbModel.Role `json:"role"`
}

type SetUsernameReq struct {
	Username string `json:"username"`
}

func (s *SetUsernameReq) Validate() error {
	if s.Username == "" {
		return errors.New("username is empty")
	} else if len(s.Username) > 32 {
		return ErrUsernameTooLong
	} else if !alnumPrintHanReg.MatchString(s.Username) {
		return ErrUsernameHasInvalidChar
	}
	return nil
}

func (s *SetUsernameReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

type UserIDReq struct {
	ID string `json:"id"`
}

func (u *UserIDReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

func (u *UserIDReq) Validate() error {
	if len(u.ID) != 32 {
		return errors.New("id is required")
	}
	return nil
}

type UserBindProviderResp map[provider.OAuth2Provider]struct {
	ProviderUserID string `json:"providerUserID"`
	CreatedAt      int64  `json:"createdAt"`
}

type GetUserBindEmailStep1CaptchaResp struct {
	CaptchaID     string `json:"captchaID"`
	CaptchaBase64 string `json:"captchaBase64"`
}

type UserSendBindEmailCaptchaReq struct {
	Email     string `json:"email"`
	CaptchaID string `json:"captchaID"`
	Answer    string `json:"answer"`
}

func (u *UserSendBindEmailCaptchaReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

var (
	ErrEmailTooLong = errors.New("email is too long")
	ErrInvalidEmail = errors.New("invalid email")
)

func (u *UserSendBindEmailCaptchaReq) Validate() error {
	if u.Email == "" {
		return errors.New("email is empty")
	} else if len(u.Email) > 128 {
		return ErrEmailTooLong
	} else if !emailReg.MatchString(u.Email) {
		return ErrInvalidEmail
	}
	if u.CaptchaID == "" {
		return errors.New("captcha id is empty")
	}
	if u.Answer == "" {
		return errors.New("answer is empty")
	}
	return nil
}

type UserBindEmailReq struct {
	Email   string `json:"email"`
	Captcha string `json:"captcha"`
}

func (u *UserBindEmailReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

func (u *UserBindEmailReq) Validate() error {
	if u.Email == "" {
		return errors.New("email is empty")
	} else if len(u.Email) > 128 {
		return ErrEmailTooLong
	} else if !emailReg.MatchString(u.Email) {
		return ErrInvalidEmail
	}
	if u.Captcha == "" {
		return errors.New("captcha is empty")
	}
	return nil
}

type SendUserSignupEmailCaptchaReq = UserSendBindEmailCaptchaReq

type UserSignupEmailReq struct {
	UserBindEmailReq
	Password string `json:"password"`
}

func (u *UserSignupEmailReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

func (u *UserSignupEmailReq) Validate() error {
	if err := u.UserBindEmailReq.Validate(); err != nil {
		return err
	}
	if u.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(u.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alnumPrintReg.MatchString(u.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}

type SendUserRetrievePasswordEmailCaptchaReq = UserSendBindEmailCaptchaReq

type UserRetrievePasswordEmailReq struct {
	Email    string `json:"email"`
	Captcha  string `json:"captcha"`
	Password string `json:"password"`
}

func (u *UserRetrievePasswordEmailReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

func (u *UserRetrievePasswordEmailReq) Validate() error {
	if u.Captcha == "" {
		return errors.New("captcha is empty")
	}
	return nil
}
