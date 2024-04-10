package handlers

import (
	"math/rand"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/captcha"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/email"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"golang.org/x/exp/slices"
	"gorm.io/gorm"
)

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&model.UserInfoResp{
		ID:        user.ID,
		Username:  user.Username,
		Role:      user.Role,
		CreatedAt: user.CreatedAt.UnixMilli(),
		Email:     user.Email,
	}))
}

func LoginUser(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.LoginUserReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	user, err := op.LoadUserByUsername(req.Username)
	if err != nil {
		log.Errorf("failed to load user: %v", err)
		if err == op.ErrUserBanned || err == op.ErrUserPending {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	if ok := user.Value().CheckPassword(req.Password); !ok {
		log.Errorf("password incorrect")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("password incorrect"))
		return
	}

	token, err := middlewares.NewAuthUserToken(user.Value())
	if err != nil {
		log.Errorf("failed to generate token: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"token": token,
	}))
}

func LogoutUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	err := op.CompareAndDeleteUser(user)
	if err != nil {
		log.Errorf("failed to logout: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserRooms(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("failed to get page and max: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{
		db.WhereCreatorID(user.ID),
	}

	switch ctx.DefaultQuery("status", "active") {
	case "active":
		scopes = append(scopes, db.WhereStatus(dbModel.RoomStatusActive))
	case "pending":
		scopes = append(scopes, db.WhereStatus(dbModel.RoomStatusPending))
	case "banned":
		scopes = append(scopes, db.WhereStatus(dbModel.RoomStatusBanned))
	}

	switch ctx.DefaultQuery("sort", "name") {
	case "createdAt":
		if desc {
			scopes = append(scopes, db.OrderByCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByCreatedAtAsc)
		}
	case "name":
		if desc {
			scopes = append(scopes, db.OrderByDesc("name"))
		} else {
			scopes = append(scopes, db.OrderByAsc("name"))
		}
	default:
		log.Errorf("not support sort")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, creator
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			scopes = append(scopes, db.WhereRoomNameLikeOrCreatorInOrIDLike(keyword, db.GerUsersIDByUsernameLike(keyword), keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllRoomsCount(scopes...),
		"list":  genRoomListResp(append(scopes, db.Paginate(page, pageSize))...),
	}))
}

func SetUsername(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.SetUsernameReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err := user.SetUsername(req.Username)
	if err != nil {
		log.Errorf("failed to set username: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SetUserPassword(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.SetUserPasswordReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err := user.SetPassword(req.Password)
	if err != nil {
		log.Errorf("failed to set password: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthUserToken(user)
	if err != nil {
		log.Errorf("failed to generate token: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"token": token,
	}))
}

func UserBindProviders(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	up, err := db.GetBindProviders(user.ID)
	if err != nil {
		log.Errorf("failed to get bind providers: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	m := providers.EnabledProvider()

	resp := make(model.UserBindProviderResp, len(up))

	for _, v := range up {
		if _, ok := m.Load(v.Provider); ok {
			resp[v.Provider] = struct {
				ProviderUserID string "json:\"providerUserID\""
				CreatedAt      int64  "json:\"createdAt\""
			}{
				ProviderUserID: v.ProviderUserID,
				CreatedAt:      v.CreatedAt.UnixMilli(),
			}
		}
	}

	m.Range(func(p provider.OAuth2Provider, pi struct{}) bool {
		if _, ok := resp[p]; !ok {
			resp[p] = struct {
				ProviderUserID string "json:\"providerUserID\""
				CreatedAt      int64  "json:\"createdAt\""
			}{
				ProviderUserID: "",
				CreatedAt:      0,
			}
		}
		return true
	})

	ctx.JSON(http.StatusOK, resp)
}

func GetUserBindEmailStep1Captcha(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	id, data, _, err := captcha.Captcha.Generate()
	if err != nil {
		log.Errorf("failed to generate captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&model.GetUserBindEmailStep1CaptchaResp{
		CaptchaID:     id,
		CaptchaBase64: data,
	}))
}

func SendUserBindEmailCaptcha(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.UserSendBindEmailCaptchaReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !captcha.Captcha.Verify(
		req.CaptchaID,
		req.Answer,
		true,
	) {
		log.Errorf("captcha verify failed")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("captcha verify failed"))
		return
	}

	if user.Email == req.Email {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("this email same as current email"))
		return
	}

	_, err := op.LoadOrInitUserByEmail(req.Email)
	if err == nil {
		log.Errorf("email already bind")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("email already bind"))
		return
	}

	if err := user.SendBindCaptchaEmail(req.Email); err != nil {
		log.Errorf("failed to send email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserBindEmail(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.UserBindEmailReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if ok, err := user.VerifyBindCaptchaEmail(req.Email, req.Captcha); err != nil || !ok {
		log.Errorf("email captcha verify failed")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("email captcha verify failed"))
		return
	}

	err := user.BindEmail(req.Email)
	if err != nil {
		log.Errorf("failed to bind email: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserUnbindEmail(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	err := user.UnbindEmail()
	if err != nil {
		log.Errorf("failed to unbind email: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func GetUserSignupEmailStep1Captcha(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	id, data, _, err := captcha.Captcha.Generate()
	if err != nil {
		log.Errorf("failed to generate captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&model.GetUserBindEmailStep1CaptchaResp{
		CaptchaID:     id,
		CaptchaBase64: data,
	}))
}

func SendUserSignupEmailCaptcha(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	if settings.DisableUserSignup.Get() || email.DisableUserSignup.Get() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user signup disabled"))
		return
	}

	req := model.SendUserSignupEmailCaptchaReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !captcha.Captcha.Verify(
		req.CaptchaID,
		req.Answer,
		true,
	) {
		log.Errorf("captcha verify failed")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("captcha verify failed"))
		return
	}

	if email.EmailSignupWhiteListEnable.Get() {
		_, after, found := strings.Cut(req.Email, "@")
		if !found {
			log.Errorf("email format error")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("email format error"))
			return
		}
		if !slices.Contains(
			strings.Split(email.EmailSignupWhiteList.Get(), ","),
			after,
		) {
			log.Errorf("email(%s) sub(%s) not in white list", req.Email, after)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("email not in white list"))
			return
		}
	}

	_, err := op.LoadOrInitUserByEmail(req.Email)
	if err == nil {
		log.Errorf("email already exists")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("email already exists"))
		return
	}

	if err := email.SendSignupCaptchaEmail(req.Email); err != nil {
		log.Errorf("failed to send email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserSignupEmail(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	if settings.DisableUserSignup.Get() || email.DisableUserSignup.Get() {
		log.Errorf("user signup disabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user signup disabled"))
		return
	}

	req := model.UserSignupEmailReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ok, err := email.VerifySignupCaptchaEmail(req.Email, req.Captcha)
	if err != nil {
		log.Errorf("failed to verify email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	if !ok {
		log.Errorf("email captcha verify failed")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("email captcha verify failed"))
		return
	}

	var user *op.UserEntry
	if settings.SignupNeedReview.Get() || email.SignupNeedReview.Get() {
		user, err = op.CreateUserWithEmail(req.Email, req.Password, req.Email, db.WithRole(dbModel.RolePending))
	} else {
		user, err = op.CreateUserWithEmail(req.Email, req.Password, req.Email)
	}
	if err != nil {
		log.Errorf("failed to create user: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthUserToken(user.Value())
	if err != nil {
		log.Errorf("failed to generate token: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"token": token,
	}))
}

func GetUserRetrievePasswordEmailStep1Captcha(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	id, data, _, err := captcha.Captcha.Generate()
	if err != nil {
		log.Errorf("failed to generate captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&model.GetUserBindEmailStep1CaptchaResp{
		CaptchaID:     id,
		CaptchaBase64: data,
	}))
}

func SendUserRetrievePasswordEmailCaptcha(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.SendUserRetrievePasswordEmailCaptchaReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !captcha.Captcha.Verify(
		req.CaptchaID,
		req.Answer,
		true,
	) {
		log.Errorf("captcha verify failed")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("captcha verify failed"))
		return
	}

	user, err := op.LoadOrInitUserByEmail(req.Email)
	if err != nil {
		log.Errorf("failed to load or init user by email: %v", err)
		time.Sleep(time.Duration(rand.Intn(1500)) + time.Second*3)
		ctx.Status(http.StatusNoContent)
		return
	}

	host := HOST.Get()
	if host == "" {
		host = (&url.URL{
			Scheme: "http",
			Host:   ctx.Request.Host,
		}).String()
	}
	if host == "" {
		log.Error("failed to get host on send retrieve password email")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("failed to get host"))
		return
	}

	if err := user.Value().SendRetrievePasswordCaptchaEmail(host); err != nil {
		log.Errorf("failed to send email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserRetrievePasswordEmail(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.UserRetrievePasswordEmailReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	userE, err := op.LoadOrInitUserByEmail(req.Email)
	if err != nil {
		log.Errorf("failed to get user by email: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	user := userE.Value()

	if ok, err := user.VerifyRetrievePasswordCaptchaEmail(req.Email, req.Captcha); err != nil || !ok {
		log.Errorf("email captcha verify failed")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("email captcha verify failed"))
		return
	}

	err = user.SetPassword(req.Password)
	if err != nil {
		log.Errorf("failed to set password: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthUserToken(user)
	if err != nil {
		log.Errorf("failed to generate token: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"token": token,
	}))
}
