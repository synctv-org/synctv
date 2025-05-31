package handlers

import (
	"errors"
	"math/rand/v2"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
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
	"github.com/zijiren233/gencontainer/synccache"
	"gorm.io/gorm"
)

func Me(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(&model.UserInfoResp{
		ID:        user.ID,
		Username:  user.Username,
		Role:      user.Role,
		CreatedAt: user.CreatedAt.UnixMilli(),
		Email:     user.Email.String(),
	}))
}

func LoginUser(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	req := model.LoginUserReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	var user *synccache.Entry[*op.User]
	var err error
	switch {
	case req.Username != "":
		user, err = op.LoadOrInitUserByUsername(req.Username)
	case req.Email != "":
		user, err = op.LoadOrInitUserByEmail(req.Email)
	default:
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("username or email is required"),
		)
		return
	}

	if err != nil {
		log.Errorf("failed to load user: %v", err)
		if errors.Is(err, db.NotFoundError(db.ErrUserNotFound)) {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewAPIErrorResp(err))
			return
		}
		if errors.Is(err, op.ErrUserBanned) || errors.Is(err, op.ErrUserPending) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewAPIErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	if ok := user.Value().CheckPassword(req.Password); !ok {
		log.Errorf("password incorrect")
		ctx.AbortWithStatusJSON(
			http.StatusForbidden,
			model.NewAPIErrorStringResp("password incorrect"),
		)
		return
	}

	handleUserToken(ctx, user.Value())
}

func handleUserToken(ctx *gin.Context, user *op.User) {
	log := middlewares.GetLogger(ctx)

	token, err := middlewares.NewAuthUserToken(user)
	if err != nil {
		if errors.Is(err, middlewares.ErrUserBanned) ||
			errors.Is(err, middlewares.ErrUserPending) {
			ctx.AbortWithStatusJSON(http.StatusOK, model.NewAPIDataResp(gin.H{
				"message": err.Error(),
				"role":    user.Role,
			}))
			return
		}
		log.Errorf("failed to generate token: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"token": token,
		"role":  user.Role,
	}))
}

func LogoutUser(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	err := op.CompareAndDeleteUser(user)
	if err != nil {
		log.Errorf("failed to logout: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserRooms(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("failed to get page and max: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

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

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, creator
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			ids, err := db.GerUsersIDByUsernameLike(keyword)
			if err != nil {
				log.Errorf("failed to get all rooms count: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereRoomNameLikeOrCreatorInOrIDLike(keyword, ids, keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	total, err := db.GetAllRoomsCount(scopes...)
	if err != nil {
		log.Errorf("failed to get all rooms count: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	desc := ctx.DefaultQuery("order", "desc") == "desc"
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
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("not support sort"),
		)
		return
	}

	list, err := genRoomListResp(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("failed to get all rooms: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func UserJoinedRooms(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("failed to get page and max: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	scopes := []func(db *gorm.DB) *gorm.DB{
		func(db *gorm.DB) *gorm.DB {
			return db.
				InnerJoins(
					"JOIN room_members ON rooms.id = room_members.room_id AND room_members.user_id = ? AND rooms.creator_id != ?",
					user.ID,
					user.ID,
				)
		},
		func(db *gorm.DB) *gorm.DB {
			return db.Preload("RoomMembers", func(db *gorm.DB) *gorm.DB {
				return db.Where("user_id = ?", user.ID)
			})
		},
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			scopes = append(scopes, db.WhereRoomNameLikeOrIDLike(keyword, keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	total, err := db.GetAllRoomsCount(scopes...)
	if err != nil {
		log.Errorf("failed to get all rooms count: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	desc := ctx.DefaultQuery("order", "desc") == "desc"
	switch ctx.DefaultQuery("sort", "name") {
	case "createdAt":
		if desc {
			scopes = append(scopes, db.OrderByRoomCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByRoomCreatedAtAsc)
		}
	case "name":
		if desc {
			scopes = append(scopes, db.OrderByDesc("rooms.name"))
		} else {
			scopes = append(scopes, db.OrderByAsc("rooms.name"))
		}
	default:
		log.Errorf("not support sort")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("not support sort"),
		)
		return
	}

	list, err := genJoinedRoomListResp(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("failed to get joined rooms: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func UserCheckJoinedRoom(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	id, err := middlewares.GetRoomIDFromContext(ctx)
	if err != nil {
		log.Errorf("failed to get room id: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("id is invalid"))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(id)
	if err != nil {
		log.Errorf("login room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewAPIErrorResp(err))
		return
	}
	room := roomE.Value()

	status, err := room.LoadMemberStatus(user.ID)
	if err != nil {
		log.Errorf("get room member status failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"joined": status != dbModel.RoomMemberStatusNotJoined,
		"status": status,
		"room": &model.CheckRoomResp{
			Name:         room.Name,
			Status:       room.Status,
			CreatorID:    room.CreatorID,
			Creator:      op.GetUserName(room.CreatorID),
			NeedPassword: room.NeedPassword(),
			ViewerCount:  op.ViewerCount(room.ID),
			EnabledGuest: room.EnabledGuest(),
		},
	}))
}

func SetUsername(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.SetUsernameReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.SetUsername(req.Username)
	if err != nil {
		log.Errorf("failed to set username: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SetUserPassword(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.SetUserPasswordReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.SetPassword(req.Password)
	if err != nil {
		log.Errorf("failed to set password: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	handleUserToken(ctx, user)
}

func UserBindProviders(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	up, err := db.GetBindProviders(user.ID)
	if err != nil {
		log.Errorf("failed to get bind providers: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	m := providers.EnabledProvider()

	resp := make(model.UserBindProviderResp, len(up))

	for _, v := range up {
		if _, ok := m.Load(v.Provider); ok {
			resp[v.Provider] = struct {
				ProviderUserID string `json:"providerUserId"`
				CreatedAt      int64  `json:"createdAt"`
			}{
				ProviderUserID: v.ProviderUserID,
				CreatedAt:      v.CreatedAt.UnixMilli(),
			}
		}
	}

	m.Range(func(p provider.OAuth2Provider, _ struct{}) bool {
		if _, ok := resp[p]; !ok {
			resp[p] = struct {
				ProviderUserID string `json:"providerUserId"`
				CreatedAt      int64  `json:"createdAt"`
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
	// user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	id, data, _, err := captcha.Captcha.Generate()
	if err != nil {
		log.Errorf("failed to generate captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(&model.GetUserBindEmailStep1CaptchaResp{
		CaptchaID:     id,
		CaptchaBase64: data,
	}))
}

func SendUserBindEmailCaptcha(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.UserSendBindEmailCaptchaReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if !captcha.Captcha.Verify(
		req.CaptchaID,
		req.Answer,
		true,
	) {
		log.Errorf("captcha verify failed")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("captcha verify failed"),
		)
		return
	}

	if user.Email.String() == req.Email {
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("this email same as current email"),
		)
		return
	}

	_, err := op.LoadOrInitUserByEmail(req.Email)
	if err == nil {
		log.Errorf("email already bind")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("email already bind"),
		)
		return
	}

	if err := user.SendBindCaptchaEmail(req.Email); err != nil {
		log.Errorf("failed to send email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserBindEmail(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.UserBindEmailReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if ok, err := user.VerifyBindCaptchaEmail(req.Email, req.Captcha); err != nil || !ok {
		log.Errorf("email captcha verify failed")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("email captcha verify failed"),
		)
		return
	}

	err := user.BindEmail(req.Email)
	if err != nil {
		log.Errorf("failed to bind email: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserUnbindEmail(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	err := user.UnbindEmail()
	if err != nil {
		log.Errorf("failed to unbind email: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func GetUserSignupEmailStep1Captcha(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	id, data, _, err := captcha.Captcha.Generate()
	if err != nil {
		log.Errorf("failed to generate captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(&model.GetUserBindEmailStep1CaptchaResp{
		CaptchaID:     id,
		CaptchaBase64: data,
	}))
}

func SendUserSignupEmailCaptcha(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	if settings.DisableUserSignup.Get() {
		log.Errorf("user signup disabled")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("user signup disabled"),
		)
		return
	} else if email.DisableUserSignup.Get() {
		log.Errorf("email signup disabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("email signup disabled"))
		return
	}

	req := model.SendUserSignupEmailCaptchaReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if !captcha.Captcha.Verify(
		req.CaptchaID,
		req.Answer,
		true,
	) {
		log.Errorf("captcha verify failed")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("captcha verify failed"),
		)
		return
	}

	if email.EmailSignupWhiteListEnable.Get() {
		_, after, found := strings.Cut(req.Email, "@")
		if !found {
			log.Errorf("email format error")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("email format error"),
			)
			return
		}
		if !slices.Contains(
			strings.Split(email.EmailSignupWhiteList.Get(), ","),
			after,
		) {
			log.Errorf("email(%s) sub(%s) not in white list", req.Email, after)
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("email not in white list"),
			)
			return
		}
	}

	_, err := op.LoadOrInitUserByEmail(req.Email)
	if err == nil {
		log.Errorf("email already exists")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("email already exists"),
		)
		return
	}

	if err := email.SendSignupCaptchaEmail(req.Email); err != nil {
		log.Errorf("failed to send email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserSignupEmail(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	if settings.DisableUserSignup.Get() {
		log.Errorf("user signup disabled")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("user signup disabled"),
		)
		return
	} else if email.DisableUserSignup.Get() {
		log.Errorf("email signup disabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("email signup disabled"))
		return
	}

	req := model.UserSignupEmailReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ok, err := email.VerifySignupCaptchaEmail(req.Email, req.Captcha)
	if err != nil {
		log.Errorf("failed to verify email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}
	if !ok {
		log.Errorf("email captcha verify failed")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("email captcha verify failed"),
		)
		return
	}

	var user *op.UserEntry
	if settings.SignupNeedReview.Get() || email.SignupNeedReview.Get() {
		user, err = op.CreateUserWithEmail(
			req.Email,
			req.Password,
			req.Email,
			db.WithRole(dbModel.RolePending),
		)
	} else {
		user, err = op.CreateUserWithEmail(req.Email, req.Password, req.Email)
	}
	if err != nil {
		log.Errorf("failed to create user: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	handleUserToken(ctx, user.Value())
}

func GetUserRetrievePasswordEmailStep1Captcha(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	id, data, _, err := captcha.Captcha.Generate()
	if err != nil {
		log.Errorf("failed to generate captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(&model.GetUserBindEmailStep1CaptchaResp{
		CaptchaID:     id,
		CaptchaBase64: data,
	}))
}

func SendUserRetrievePasswordEmailCaptcha(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	req := model.SendUserRetrievePasswordEmailCaptchaReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if !captcha.Captcha.Verify(
		req.CaptchaID,
		req.Answer,
		true,
	) {
		log.Errorf("captcha verify failed")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("captcha verify failed"),
		)
		return
	}

	user, err := op.LoadOrInitUserByEmail(req.Email)
	if err != nil {
		log.Errorf("failed to load or init user by email: %v", err)
		time.Sleep(time.Duration(rand.IntN(1500)) + time.Second*3)
		ctx.Status(http.StatusNoContent)
		return
	}

	host := settings.HOST.Get()
	if host == "" {
		host = (&url.URL{
			Scheme: "http",
			Host:   ctx.Request.Host,
		}).String()
	}
	if host == "" {
		log.Error("failed to get host on send retrieve password email")
		ctx.AbortWithStatusJSON(
			http.StatusInternalServerError,
			model.NewAPIErrorStringResp("failed to get host"),
		)
		return
	}

	if err := user.Value().SendRetrievePasswordCaptchaEmail(host); err != nil {
		log.Errorf("failed to send email captcha: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserRetrievePasswordEmail(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	req := model.UserRetrievePasswordEmailReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	userE, err := op.LoadOrInitUserByEmail(req.Email)
	if err != nil {
		log.Errorf("failed to get user by email: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}
	user := userE.Value()

	if ok, err := user.VerifyRetrievePasswordCaptchaEmail(req.Email, req.Captcha); err != nil ||
		!ok {
		log.Errorf("email captcha verify failed")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("email captcha verify failed"),
		)
		return
	}

	err = user.SetPassword(req.Password)
	if err != nil {
		log.Errorf("failed to set password: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	handleUserToken(ctx, user)
}

func UserDeleteRoom(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.IDReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.Errorf("failed to get room: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	room := roomE.Value()

	if room.CreatorID != user.ID {
		log.Errorf("not creator")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewAPIErrorStringResp("not creator"))
		return
	}

	err = op.DeleteRoomWithRoomEntry(roomE)
	if err != nil {
		log.Errorf("failed to delete room: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserSignupPassword(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	if settings.DisableUserSignup.Get() {
		log.Errorf("user signup disabled")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("user signup disabled"),
		)
		return
	} else if !settings.EnablePasswordSignup.Get() {
		log.Errorf("password signup disabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("password signup disabled"))
		return
	}

	var req model.UserSignupPasswordReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	var user *op.UserEntry
	var err error
	if settings.SignupNeedReview.Get() || settings.PasswordSignupNeedReview.Get() {
		user, err = op.CreateUser(req.Username, req.Password, db.WithRole(dbModel.RolePending))
	} else {
		user, err = op.CreateUser(req.Username, req.Password)
	}
	if err != nil {
		log.Errorf("failed to create user: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	handleUserToken(ctx, user.Value())
}

func UserExitRoom(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.IDReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.Errorf("failed to get room: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	room := roomE.Value()

	err = room.DeleteMember(user.ID)
	if err != nil {
		log.Errorf("failed to delete room member: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
