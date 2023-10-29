package handlers

import (
	"errors"
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/model"
	"gorm.io/gorm"
)

func EditAdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)

	req := model.AdminSettingsReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	for k, v := range req {
		err := settings.SetValue(k, v)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}

func AdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	group := ctx.Param("group")
	if group == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("group is required"))
		return
	}

	s, ok := settings.GroupSettings[dbModel.SettingGroup(group)]
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("group not found"))
		return
	}
	resp := make(gin.H, len(s))
	for _, v := range s {
		i, err := v.Interface()
		if err != nil {
			ctx.AbortWithError(http.StatusInternalServerError, err)
			return
		}
		resp[v.Name()] = i
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}

func Users(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	order := ctx.Query("order")
	if order == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("order is required"))
		return
	}

	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("sort", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{}

	if keyword := ctx.Query("keyword"); keyword != "" {
		scopes = append(scopes, db.WhereUserNameLike(keyword))
	}

	switch order {
	case "createdAt":
		if desc {
			scopes = append(scopes, db.OrderByCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByCreatedAtAsc)
		}
	case "name":
		if desc {
			scopes = append(scopes, db.OrderByDesc("username"))
		} else {
			scopes = append(scopes, db.OrderByAsc("username"))
		}
	case "id":
		if desc {
			scopes = append(scopes, db.OrderByIDDesc)
		} else {
			scopes = append(scopes, db.OrderByIDAsc)
		}
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support order"))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllUserCountWithRole(dbModel.RoleUser, scopes...),
		"list":  genUserListResp(dbModel.RoleUser, append(scopes, db.Paginate(page, pageSize))...),
	}))
}

func genUserListResp(role dbModel.Role, scopes ...func(db *gorm.DB) *gorm.DB) []*model.UserInfoResp {
	us := db.GetAllUserWithRoleUser(role, scopes...)
	resp := make([]*model.UserInfoResp, len(us))
	for i, v := range us {
		resp[i] = &model.UserInfoResp{
			ID:        v.ID,
			Username:  v.Username,
			Role:      v.Role,
			CreatedAt: v.CreatedAt.UnixMilli(),
		}
	}
	return resp
}

func PendingUsers(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	order := ctx.Query("order")
	if order == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("order is required"))
		return
	}

	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("sort", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{}

	if keyword := ctx.Query("keyword"); keyword != "" {
		scopes = append(scopes, db.WhereUserNameLike(keyword))
	}

	switch order {
	case "createdAt":
		if desc {
			scopes = append(scopes, db.OrderByCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByCreatedAtAsc)
		}
	case "name":
		if desc {
			scopes = append(scopes, db.OrderByDesc("username"))
		} else {
			scopes = append(scopes, db.OrderByAsc("username"))
		}
	case "id":
		if desc {
			scopes = append(scopes, db.OrderByIDDesc)
		} else {
			scopes = append(scopes, db.OrderByIDAsc)
		}
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support order"))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllUserCountWithRole(dbModel.RolePending, scopes...),
		"list":  genUserListResp(dbModel.RolePending, append(scopes, db.Paginate(page, pageSize))...),
	}))
}

func ApprovePendingUser(Authorization, userID string) error {
	user, err := op.GetUserById(userID)
	if err != nil {
		return err
	}
	if !user.IsPending() {
		return errors.New("user is not pending")
	}
	if err := user.SetRole(dbModel.RoleUser); err != nil {
		return err
	}
	return nil
}

func BanUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	u, err := op.GetUserById(req.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if u.ID == user.ID {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot ban yourself"))
		return
	}
	if u.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot ban root user"))
		return
	}

	err = u.SetRole(dbModel.RoleBanned)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
