package handlers

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"slices"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/maruel/natural"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/email"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"google.golang.org/grpc/connectivity"
	"gorm.io/gorm"
)

func AdminEditSettings(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	req := model.AdminSettingsReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	for k, v := range req {
		err := settings.SetValue(k, v)
		if err != nil {
			log.Errorf("set value error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}

func AdminSettings(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	group := ctx.Param("group")
	switch group {
	case "oauth2":
		const groupPrefix = dbModel.SettingGroupOauth2

		settingGroups := make(map[string]map[string]settings.Setting)
		for sg, v := range settings.GroupSettings {
			if strings.HasPrefix(sg, groupPrefix) {
				settingGroups[sg] = v
			}
		}

		resp := make(model.AdminSettingsResp, len(settingGroups))
		for k, v := range settingGroups {
			if resp[k] == nil {
				resp[k] = make(gin.H, len(v))
			}

			for k2, s := range v {
				resp[k][k2] = s.Interface()
			}
		}

		ctx.JSON(http.StatusOK, model.NewAPIDataResp(resp))
	case "":
		resp := make(model.AdminSettingsResp, len(settings.GroupSettings))
		for sg, v := range settings.GroupSettings {
			if resp[sg] == nil {
				resp[sg] = make(gin.H, len(v))
			}

			for _, s2 := range v {
				resp[sg][s2.Name()] = s2.Interface()
			}
		}

		ctx.JSON(http.StatusOK, model.NewAPIDataResp(resp))
	default:
		s, ok := settings.GroupSettings[group]
		if !ok {
			log.Error("group not found")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("group not found"),
			)

			return
		}

		data := make(map[string]any, len(s))
		for _, v := range s {
			data[v.Name()] = v.Interface()
		}

		resp := model.AdminSettingsResp{group: data}

		ctx.JSON(http.StatusOK, model.NewAPIDataResp(resp))
	}
}

func AdminGetUsers(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get page and max error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	scopes := []func(db *gorm.DB) *gorm.DB{}

	switch ctx.Query("role") {
	case "admin":
		scopes = append(scopes, db.WhereRole(dbModel.RoleAdmin))
	case "user":
		scopes = append(scopes, db.WhereRole(dbModel.RoleUser))
	case "pending":
		scopes = append(scopes, db.WhereRole(dbModel.RolePending))
	case "banned":
		scopes = append(scopes, db.WhereRole(dbModel.RoleBanned))
	case "root":
		scopes = append(scopes, db.WhereRole(dbModel.RoleRoot))
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, id
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.Errorf("get users id by id like error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
				return
			}

			scopes = append(scopes, db.WhereUsernameLikeOrIDIn(keyword, ids))
		case "name":
			scopes = append(scopes, db.WhereUsernameLike(keyword))
		case "id":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.Errorf("get users id by id like error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
				return
			}

			scopes = append(scopes, db.WhereIDIn(ids))
		}
	}

	total, err := db.GetUserCount(scopes...)
	if err != nil {
		log.Errorf("get all user count error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
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
			scopes = append(scopes, db.OrderByDesc("username"))
		} else {
			scopes = append(scopes, db.OrderByAsc("username"))
		}
	default:
		log.Error("not support sort")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("not support sort"),
		)

		return
	}

	list, err := db.GetUsers(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("get all users error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  genUserListResp(list),
	}))
}

func genUserListResp(us []*dbModel.User) []*model.UserInfoResp {
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

func AdminGetRoomMembers(ctx *gin.Context) {
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get room users failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	scopes := []func(db *gorm.DB) *gorm.DB{}

	switch ctx.DefaultQuery("status", "active") {
	case "pending":
		scopes = append(scopes, db.WhereRoomMemberStatus(dbModel.RoomMemberStatusPending))
	case "banned":
		scopes = append(scopes, db.WhereRoomMemberStatus(dbModel.RoomMemberStatusBanned))
	case "active":
		scopes = append(scopes, db.WhereRoomMemberStatus(dbModel.RoomMemberStatusActive))
	}

	switch ctx.DefaultQuery("role", "") {
	case "admin":
		scopes = append(scopes, db.WhereRoomMemberRole(dbModel.RoomMemberRoleAdmin))
	case "member":
		scopes = append(scopes, db.WhereRoomMemberRole(dbModel.RoomMemberRoleMember))
	case "creator":
		scopes = append(scopes, db.WhereRoomMemberRole(dbModel.RoomMemberRoleCreator))
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, id
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.Errorf("get room users failed: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
				return
			}

			scopes = append(scopes, db.WhereUsernameLikeOrIDIn(keyword, ids))
		case "name":
			scopes = append(scopes, db.WhereUsernameLike(keyword))
		case "id":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.Errorf("get room users failed: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
				return
			}

			scopes = append(scopes, db.WhereIDIn(ids))
		}
	}

	scopes = append(scopes, func(db *gorm.DB) *gorm.DB {
		return db.
			InnerJoins("JOIN room_members ON users.id = room_members.user_id").
			Where("room_members.room_id = ?", room.ID)
	}, db.PreloadRoomMembers(
		db.WhereRoomID(room.ID),
	))

	total, err := db.GetUserCount(scopes...)
	if err != nil {
		log.Errorf("get room users failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	desc := ctx.DefaultQuery("order", "desc") == "desc"
	switch ctx.DefaultQuery("sort", "name") {
	case "join":
		if desc {
			scopes = append(scopes, db.OrderByUsersCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByUsersCreatedAtAsc)
		}
	case "name":
		if desc {
			scopes = append(scopes, db.OrderByDesc("username"))
		} else {
			scopes = append(scopes, db.OrderByAsc("username"))
		}
	default:
		log.Errorf("get room users failed: not support sort")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("not support sort"),
		)

		return
	}

	list, err := db.GetUsers(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("get room users failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  genRoomMemberListResp(list, room),
	}))
}

func genRoomMemberListResp(us []*dbModel.User, room *op.Room) []*model.RoomMembersResp {
	resp := make([]*model.RoomMembersResp, len(us))
	for i, v := range us {
		permissions := v.RoomMembers[0].Permissions
		if room.IsGuest(v.ID) {
			permissions = room.Settings.GuestPermissions
		}

		resp[i] = &model.RoomMembersResp{
			UserID:           v.ID,
			Username:         v.Username,
			JoinAt:           v.RoomMembers[0].CreatedAt.UnixMilli(),
			OnlineCount:      room.UserOnlineCount(v.ID),
			Role:             v.RoomMembers[0].Role,
			Status:           v.RoomMembers[0].Status,
			RoomID:           v.RoomMembers[0].RoomID,
			Permissions:      permissions,
			AdminPermissions: v.RoomMembers[0].AdminPermissions,
		}
	}

	return resp
}

func AdminApprovePendingUser(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	userE, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("get user by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	user := userE.Value()

	if !user.IsPending() {
		log.Error("user is not pending")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("user is not pending"),
		)

		return
	}

	err = user.SetUserRole()
	if err != nil {
		log.Errorf("set role by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminBanUser(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if req.ID == user.ID {
		log.Error("cannot ban self")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("cannot ban self"),
		)

		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("load or init user by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if u.Value().IsRoot() {
		log.Error("cannot ban root")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("cannot ban root"),
		)

		return
	}

	if u.Value().IsAdmin() && !user.IsRoot() {
		log.Error("cannot ban admin")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("cannot ban admin"),
		)

		return
	}

	err = u.Value().Ban()
	if err != nil {
		log.Errorf("set role error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUnBanUser(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("load or init user by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if !u.Value().IsBanned() {
		log.Error("user is not banned")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("user is not banned"),
		)

		return
	}

	err = u.Value().Unban()
	if err != nil {
		log.Errorf("set role error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminGetRooms(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get page and max error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	scopes := []func(db *gorm.DB) *gorm.DB{}

	switch ctx.Query("status") {
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
				log.Errorf("get users id by username like error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
				return
			}

			scopes = append(scopes, db.WhereRoomNameLikeOrCreatorInOrIDLike(keyword, ids, keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "creator":
			ids, err := db.GerUsersIDByUsernameLike(keyword)
			if err != nil {
				log.Errorf("get users id by username like error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
				return
			}

			scopes = append(scopes, db.WhereCreatorIDIn(ids))
		case "creatorId":
			scopes = append(scopes, db.WhereCreatorID(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	total, err := db.GetAllRoomsCount(scopes...)
	if err != nil {
		log.Errorf("get all rooms count error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
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
		log.Error("not support sort")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("not support sort"),
		)

		return
	}

	list, err := genRoomListResp(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("gen room list resp error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func AdminGetUserRooms(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	id := ctx.Query("id")
	if len(id) != 32 {
		log.Error("user id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("user id error"))
		return
	}

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get page and max error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	scopes := []func(db *gorm.DB) *gorm.DB{
		db.WhereCreatorID(id),
	}

	switch ctx.Query("status") {
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
			scopes = append(scopes, db.WhereRoomNameLikeOrIDLike(keyword, keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	total, err := db.GetAllRoomsCount(scopes...)
	if err != nil {
		log.Errorf("get all rooms count error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
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
		log.Error("not support sort")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("not support sort"),
		)

		return
	}

	list, err := genRoomListResp(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("gen room list resp error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func AdminGetUserJoinedRooms(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	id := ctx.Query("id")
	if len(id) != 32 {
		log.Error("user id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("user id error"))
		return
	}

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
					id,
					id,
				)
		},
		func(db *gorm.DB) *gorm.DB {
			return db.Preload("RoomMembers", func(db *gorm.DB) *gorm.DB {
				return db.Where("user_id = ?", id)
			})
		},
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
		log.Errorf("failed to get all rooms: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func AdminApprovePendingRoom(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.Errorf("get room by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	room := roomE.Value()

	if !room.IsPending() {
		log.Error("room is not pending")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("room is not pending"),
		)

		return
	}

	err = room.SetStatus(dbModel.RoomStatusActive)
	if err != nil {
		log.Errorf("set room status error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminBanRoom(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.Errorf("get room by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	room := roomE.Value()

	if room.CreatorID != user.ID {
		creatorE, err := op.LoadOrInitUserByID(room.CreatorID)
		if err != nil {
			log.Errorf("get user by id error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}

		creator := creatorE.Value()

		if creator.IsRoot() {
			log.Error("cannot ban root")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("cannot ban root"),
			)

			return
		}

		if creator.IsAdmin() && !user.IsRoot() {
			log.Error("cannot ban admin")
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorStringResp("cannot ban admin"),
			)

			return
		}
	}

	err = room.SetStatus(dbModel.RoomStatusBanned)
	if err != nil {
		log.Errorf("set room status error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUnBanRoom(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.Errorf("get room by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	room := roomE.Value()

	if !room.IsBanned() {
		log.Error("room is not banned")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("room is not banned"),
		)

		return
	}

	err = room.SetStatus(dbModel.RoomStatusActive)
	if err != nil {
		log.Errorf("set room status error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminAddUser(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.AddUserReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if req.Role == dbModel.RoleRoot && !user.IsRoot() {
		log.Error("cannot add root user")
		ctx.AbortWithStatusJSON(
			http.StatusForbidden,
			model.NewAPIErrorStringResp("you cannot add root user"),
		)

		return
	}

	_, err := op.CreateUser(req.Username, req.Password, db.WithRole(req.Role))
	if err != nil {
		log.Errorf("create user error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminDeleteUser(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("load or init user by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if u.Value().ID == user.ID {
		log.Error("cannot delete yourself")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("cannot delete yourself"),
		)

		return
	}

	if u.Value().IsRoot() {
		log.Error("cannot delete root")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("cannot delete root"),
		)

		return
	}

	if u.Value().IsAdmin() && !user.IsRoot() {
		log.Error("cannot delete admin")
		ctx.AbortWithStatusJSON(
			http.StatusForbidden,
			model.NewAPIErrorStringResp("cannot delete admin"),
		)

		return
	}

	if err := op.DeleteUserByID(req.ID); err != nil {
		log.Errorf("delete user by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminDeleteRoom(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.Errorf("get room by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	room := roomE.Value()

	if room.CreatorID != user.ID {
		u, err := op.LoadOrInitUserByID(room.CreatorID)
		if err != nil {
			log.Errorf("get user by id error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}

		creator := u.Value()

		if creator.IsRoot() {
			log.Error("cannot delete root's room")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("cannot delete root's room"),
			)

			return
		}

		if creator.IsAdmin() && !user.IsRoot() {
			log.Error("cannot delete admin's room")
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorStringResp("cannot delete admin's room"),
			)

			return
		}
	}

	if err := op.DeleteRoomByID(req.ID); err != nil {
		log.Errorf("delete room by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUserPassword(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.AdminUserPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp(err.Error()))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("load or init user by id error: %v", err)
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("user not found"),
		)

		return
	}

	if u.Value().ID != user.ID {
		if u.Value().IsRoot() {
			log.Error("cannot change root password")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("cannot change root password"),
			)

			return
		}

		if u.Value().IsAdmin() && !user.IsRoot() {
			log.Error("cannot change admin password")
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorStringResp("cannot change admin password"),
			)

			return
		}
	}

	if err := u.Value().SetPassword(req.Password); err != nil {
		log.Errorf("set password error: %v", err)
		ctx.AbortWithStatusJSON(
			http.StatusInternalServerError,
			model.NewAPIErrorStringResp(err.Error()),
		)

		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUsername(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.AdminUsernameReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp(err.Error()))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("load or init user by id error: %v", err)
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("user not found"),
		)

		return
	}

	if u.Value().ID != user.ID {
		if u.Value().IsRoot() {
			log.Error("cannot change root username")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("cannot change root username"),
			)

			return
		}

		if u.Value().IsAdmin() && !user.IsRoot() {
			log.Error("cannot change admin username")
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorStringResp("cannot change admin username"),
			)

			return
		}
	}

	if err := u.Value().SetUsername(req.Username); err != nil {
		log.Errorf("set username error: %v", err)
		ctx.AbortWithStatusJSON(
			http.StatusInternalServerError,
			model.NewAPIErrorStringResp(err.Error()),
		)

		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminRoomPassword(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	req := model.AdminRoomPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp(err.Error()))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.Errorf("load or init room by id error: %v", err)
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("room not found"),
		)

		return
	}

	room := roomE.Value()

	if room.CreatorID != user.ID {
		creator, err := op.LoadOrInitUserByID(room.CreatorID)
		if err != nil {
			log.Errorf("load or init user by id error: %v", err)
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("room creator not found"),
			)

			return
		}

		if creator.Value().IsRoot() {
			log.Error("cannot change root room password")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("cannot change root room password"),
			)

			return
		}

		if creator.Value().IsAdmin() && !user.IsRoot() {
			log.Error("cannot change admin room password")
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorStringResp("cannot change admin room password"),
			)

			return
		}
	}

	if err := room.SetPassword(req.Password); err != nil {
		log.Errorf("set password error: %v", err)
		ctx.AbortWithStatusJSON(
			http.StatusInternalServerError,
			model.NewAPIErrorStringResp(err.Error()),
		)

		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminGetVendorBackends(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	conns := vendor.LoadConns()

	page, size, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get page and max error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	s := slices.Collect(maps.Keys(conns))
	l := len(s)

	var resp []*model.GetVendorBackendResp
	if (page-1)*size <= l {
		slices.SortStableFunc(s, func(a, b string) int {
			if a == b {
				return 0
			}

			if natural.Less(a, b) {
				return -1
			}

			return 1
		})

		if l > size {
			l = size
		}

		resp = make([]*model.GetVendorBackendResp, 0, l)
		for _, v := range s[(page-1)*size : (page-1)*size+l] {
			resp = append(resp, &model.GetVendorBackendResp{
				Info:   conns[v].Info,
				Status: conns[v].Conn.GetState(),
			})
		}
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": l,
		"list":  resp,
	}))
}

func AdminAddVendorBackend(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	var req model.AddVendorBackendReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if err := vendor.AddVendorBackend(ctx, (*dbModel.VendorBackend)(&req)); err != nil {
		log.Errorf("add vendor backend error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminDeleteVendorBackends(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if err := vendor.DeleteVendorBackends(ctx, req.Endpoints); err != nil {
		log.Errorf("delete vendor backends error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUpdateVendorBackends(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	var req model.AddVendorBackendReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if err := vendor.UpdateVendorBackend(ctx, (*dbModel.VendorBackend)(&req)); err != nil {
		log.Errorf("update vendor backend error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminReconnectVendorBackends(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	conns := vendor.LoadConns()
	for _, v := range req.Endpoints {
		if c, ok := conns[v]; ok {
			if s := c.Conn.GetState(); s != connectivity.Ready {
				c.Conn.Connect()
				c.Conn.ResetConnectBackoff()

				if len(req.Endpoints) == 1 {
					ctx2, cf := context.WithTimeout(ctx, time.Second*5)
					defer cf()

					c.Conn.WaitForStateChange(ctx2, s)
				}
			}
		} else {
			log.WithField("endpoint", v).Error("endpoint not found")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp(fmt.Sprintf("endpoint %s not found", v)))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}

func AdminEnableVendorBackends(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if err := vendor.EnableVendorBackends(ctx, req.Endpoints); err != nil {
		log.Errorf("enable vendor backends error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminDisableVendorBackends(ctx *gin.Context) {
	// user := middlewares.GetUserEntry(ctx)
	log := middlewares.GetLogger(ctx)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if err := vendor.DisableVendorBackends(ctx, req.Endpoints); err != nil {
		log.Errorf("disable vendor backends error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminSendTestEmail(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.SendTestEmailReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if req.Email == "" {
		if err := user.SendTestEmail(); err != nil {
			log.Errorf("failed to send test email: %v", err)

			if errors.Is(err, op.ErrEmailUnbound) {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			} else {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
			}

			return
		}
	} else {
		if err := email.SendTestEmail(user.Username, req.Email); err != nil {
			log.Errorf("failed to send test email: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}
