package vendorBilibili

import (
	"errors"
	"net/http"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/bilibili"
)

func NewQRCode(ctx *gin.Context) {
	r, err := vendor.LoadBilibiliClient("").NewQRCode(ctx, &bilibili.Empty{})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.JSON(http.StatusOK, model.NewApiDataResp(r))
}

type QRCodeLoginReq struct {
	Key string `json:"key"`
}

func (r *QRCodeLoginReq) Validate() error {
	if r.Key == "" {
		return errors.New("key is empty")
	}
	return nil
}

func (r *QRCodeLoginReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func LoginWithQR(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := QRCodeLoginReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	resp, err := vendor.LoadBilibiliClient("").LoginWithQRCode(ctx, &bilibili.LoginWithQRCodeReq{
		Key: req.Key,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	switch resp.Status {
	case bilibili.QRCodeStatus_EXPIRED:
		ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
			"status": "expired",
		}))
		return
	case bilibili.QRCodeStatus_SCANNED:
		ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
			"status": "scanned",
		}))
		return
	case bilibili.QRCodeStatus_NOTSCANNED:
		ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
			"status": "notScanned",
		}))
		return
	case bilibili.QRCodeStatus_SUCCESS:
		_, err = db.CreateOrSaveBilibiliVendor(user.ID, &dbModel.BilibiliVendor{
			Cookies: resp.Cookies,
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
			"status": "success",
		}))
	default:
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("unknown status"))
		return
	}
}

func NewCaptcha(ctx *gin.Context) {
	r, err := vendor.LoadBilibiliClient("").NewCaptcha(ctx, &bilibili.Empty{})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.JSON(http.StatusOK, model.NewApiDataResp(r))
}

type SMSReq struct {
	Token     string `json:"token"`
	Challenge string `json:"challenge"`
	Validate_ string `json:"validate"`
	Telephone string `json:"telephone"`
}

func (r *SMSReq) Validate() error {
	if r.Token == "" {
		return errors.New("token is empty")
	}
	if r.Challenge == "" {
		return errors.New("challenge is empty")
	}
	if r.Validate_ == "" {
		return errors.New("validate is empty")
	}
	if r.Telephone == "" {
		return errors.New("telephone is empty")
	}
	return nil
}

func (r *SMSReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func NewSMS(ctx *gin.Context) {
	var req SMSReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := vendor.LoadBilibiliClient("").NewSMS(ctx, &bilibili.NewSMSReq{
		Phone:     req.Telephone,
		Token:     req.Token,
		Challenge: req.Challenge,
		Validate:  req.Validate_,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"captchaKey": r.CaptchaKey,
	}))
}

type SMSLoginReq struct {
	Telephone  string `json:"telephone"`
	CaptchaKey string `json:"captchaKey"`
	Code       string `json:"code"`
}

func (r *SMSLoginReq) Validate() error {
	if r.Telephone == "" {
		return errors.New("telephone is empty")
	}
	if r.CaptchaKey == "" {
		return errors.New("captchaKey is empty")
	}
	if r.Code == "" {
		return errors.New("code is empty")
	}
	return nil
}

func (r *SMSLoginReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func LoginWithSMS(ctx *gin.Context) {
	var req SMSLoginReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	c, err := vendor.LoadBilibiliClient("").LoginWithSMS(ctx, &bilibili.LoginWithSMSReq{
		Phone:      req.Telephone,
		CaptchaKey: req.CaptchaKey,
		Code:       req.Code,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	user := ctx.MustGet("user").(*op.User)
	_, err = db.CreateOrSaveBilibiliVendor(user.ID, &dbModel.BilibiliVendor{
		Cookies: c.Cookies,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.Status(http.StatusNoContent)
}

func Logout(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)
	err := db.DeleteBilibiliVendor(user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	user.BilibiliCache().Clear()
	ctx.Status(http.StatusNoContent)
}
