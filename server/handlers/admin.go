package handlers

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/maruel/natural"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/email"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"golang.org/x/exp/maps"
	"google.golang.org/grpc/connectivity"
	"gorm.io/gorm"
)

func EditAdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.AdminSettingsReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	for k, v := range req {
		err := settings.SetValue(k, v)
		if err != nil {
			log.WithError(err).Error("set value error")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}

func AdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

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

		ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
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

		ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
	default:
		s, ok := settings.GroupSettings[dbModel.SettingGroup(group)]
		if !ok {
			log.Error("group not found")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("group not found"))
			return
		}
		data := make(map[string]any, len(s))
		for _, v := range s {
			data[v.Name()] = v.Interface()
		}

		resp := model.AdminSettingsResp{dbModel.SettingGroup(group): data}

		ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
	}

}

func Users(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.WithError(err).Error("get page and max error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, id
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.WithError(err).Error("get users id by id like error")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereUsernameLikeOrIDIn(keyword, ids))
		case "name":
			scopes = append(scopes, db.WhereUsernameLike(keyword))
		case "id":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.WithError(err).Error("get users id by id like error")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereIDIn(ids))
		}
	}

	total, err := db.GetAllUserCount(scopes...)
	if err != nil {
		log.WithError(err).Error("get all user count error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	list, err := db.GetAllUsers(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.WithError(err).Error("get all users error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
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
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get room users failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, id
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.Errorf("get room users failed: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereUsernameLikeOrIDIn(keyword, ids))
		case "name":
			scopes = append(scopes, db.WhereUsernameLike(keyword))
		case "id":
			ids, err := db.GerUsersIDByIDLike(keyword)
			if err != nil {
				log.Errorf("get room users failed: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereIDIn(ids))
		}
	}
	scopes = append(scopes, func(db *gorm.DB) *gorm.DB {
		return db.
			InnerJoins("JOIN room_members ON users.id = room_members.user_id").
			Where("room_members.room_id = ?", room.ID)
	}, db.PreloadRoomMembers())

	total, err := db.GetAllUserCount(scopes...)
	if err != nil {
		log.Errorf("get room users failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	list, err := db.GetAllUsers(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("get room users failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": total,
		"list":  genRoomMemberListResp(list, room),
	}))
}

func genRoomMemberListResp(us []*dbModel.User, room *op.Room) []*model.RoomMembersResp {
	resp := make([]*model.RoomMembersResp, len(us))
	for i, v := range us {
		resp[i] = &model.RoomMembersResp{
			UserID:           v.ID,
			Username:         v.Username,
			JoinAt:           v.RoomMembers[0].CreatedAt.UnixMilli(),
			OnlineCount:      room.UserOnlineCount(v.ID),
			Role:             v.RoomMembers[0].Role,
			Status:           v.RoomMembers[0].Status,
			RoomID:           v.RoomMembers[0].RoomID,
			Permissions:      v.RoomMembers[0].Permissions,
			AdminPermissions: v.RoomMembers[0].AdminPermissions,
		}
	}
	return resp
}

func ApprovePendingUser(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	userE, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.WithError(err).Error("get user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	user := userE.Value()

	if !user.IsPending() {
		log.Error("user is not pending")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user is not pending"))
		return
	}

	err = user.SetUserRole()
	if err != nil {
		log.WithError(err).Error("set role by id error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func BanUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.WithError(err).Error("load or init user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if u.Value().IsRoot() {
		log.Error("cannot ban root")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot ban root"))
		return
	}

	if u.Value().IsAdmin() && !user.IsRoot() {
		log.Error("cannot ban admin")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot ban admin"))
		return
	}

	err = u.Value().Ban()
	if err != nil {
		log.WithError(err).Error("set role error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UnBanUser(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.WithError(err).Error("load or init user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !u.Value().IsBanned() {
		log.Error("user is not banned")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user is not banned"))
		return
	}

	err = u.Value().Unban()
	if err != nil {
		log.WithError(err).Error("set role error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func Rooms(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.WithError(err).Error("get page and max error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{}

	switch ctx.Query("status") {
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
		log.Error("not support sort")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, creator
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			ids, err := db.GerUsersIDByUsernameLike(keyword)
			if err != nil {
				log.WithError(err).Error("get users id by username like error")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereRoomNameLikeOrCreatorInOrIDLike(keyword, ids, keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "creator":
			ids, err := db.GerUsersIDByUsernameLike(keyword)
			if err != nil {
				log.WithError(err).Error("get users id by username like error")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
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
		log.WithError(err).Error("get all rooms count error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	list, err := genRoomListResp(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.WithError(err).Error("gen room list resp error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func GetUserRooms(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	id := ctx.Query("id")
	if len(id) != 32 {
		log.Error("user id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user id error"))
		return
	}
	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.WithError(err).Error("get page and max error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
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
		log.WithError(err).Error("get all rooms count error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	list, err := genRoomListResp(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.WithError(err).Error("gen room list resp error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return

	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func ApprovePendingRoom(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	room, err := db.GetRoomByID(req.Id)
	if err != nil {
		log.WithError(err).Error("get room by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !room.IsPending() {
		log.Error("room is not pending")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("room is not pending"))
		return
	}

	err = db.SetRoomStatus(req.Id, dbModel.RoomStatusActive)
	if err != nil {
		log.WithError(err).Error("set room status error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func BanRoom(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := db.GetRoomByID(req.Id)
	if err != nil {
		log.WithError(err).Error("get room by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	creator, err := db.GetUserByID(r.CreatorID)
	if err != nil {
		log.WithError(err).Error("get user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if creator.IsRoot() {
		log.Error("cannot ban root")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot ban root"))
		return
	}

	if creator.IsAdmin() && !user.IsRoot() {
		log.Error("cannot ban admin")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("cannot ban admin"))
		return
	}

	err = op.SetRoomStatusByID(req.Id, dbModel.RoomStatusBanned)
	if err != nil {
		log.WithError(err).Error("set room status error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UnBanRoom(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := db.GetRoomByID(req.Id)
	if err != nil {
		log.WithError(err).Error("get room by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !r.IsBanned() {
		log.Error("room is not banned")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("room is not banned"))
		return
	}

	err = op.SetRoomStatusByID(req.Id, dbModel.RoomStatusActive)
	if err != nil {
		log.WithError(err).Error("set room status error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AddUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.AddUserReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Role == dbModel.RoleRoot && !user.IsRoot() {
		log.Error("cannot add root user")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("you cannot add root user"))
		return
	}

	_, err := op.CreateUser(req.Username, req.Password, db.WithRole(req.Role))
	if err != nil {
		log.WithError(err).Error("create user error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DeleteUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.WithError(err).Error("load or init user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if u.Value().IsRoot() {
		log.Error("cannot delete root")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot delete root"))
		return
	}

	if u.Value().IsAdmin() && !user.IsRoot() {
		log.Error("cannot delete admin")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("cannot delete admin"))
		return
	}

	if err := op.DeleteUserByID(req.ID); err != nil {
		log.WithError(err).Error("delete user by id error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUserPassword(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.AdminUserPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(err.Error()))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.WithError(err).Error("load or init user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user not found"))
		return
	}

	if u.Value().IsRoot() {
		log.Error("cannot change root password")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot change root password"))
		return
	}

	if u.Value().IsAdmin() && !user.IsRoot() {
		log.Error("cannot change admin password")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("cannot change admin password"))
		return
	}

	if err := u.Value().SetPassword(req.Password); err != nil {
		log.WithError(err).Error("set password error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp(err.Error()))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUsername(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.AdminUsernameReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(err.Error()))
		return
	}

	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.WithError(err).Error("load or init user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user not found"))
		return
	}

	if u.Value().IsRoot() {
		log.Error("cannot change root username")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot change root username"))
		return
	}

	if u.Value().IsAdmin() && !user.IsRoot() {
		log.Error("cannot change admin username")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("cannot change admin username"))
		return
	}

	if err := u.Value().SetUsername(req.Username); err != nil {
		log.WithError(err).Error("set username error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp(err.Error()))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminRoomPassword(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.AdminRoomPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(err.Error()))
		return
	}

	r, err := op.LoadOrInitRoomByID(req.ID)
	if err != nil {
		log.WithError(err).Error("load or init room by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("room not found"))
		return
	}

	creator, err := op.LoadOrInitUserByID(r.Value().CreatorID)
	if err != nil {
		log.WithError(err).Error("load or init user by id error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("room creator not found"))
		return
	}

	if creator.Value().IsRoot() {
		log.Error("cannot change root room password")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot change root room password"))
		return
	}

	if creator.Value().IsAdmin() && !user.IsRoot() {
		log.Error("cannot change admin room password")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("cannot change admin room password"))
		return
	}

	if err := r.Value().SetPassword(req.Password); err != nil {
		log.WithError(err).Error("set password error")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp(err.Error()))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminGetVendorBackends(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	conns := vendor.LoadConns()
	page, size, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.WithError(err).Error("get page and max error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	s := maps.Keys(conns)
	l := len(s)
	var resp []*model.GetVendorBackendResp
	if (page-1)*size <= l {
		slices.SortStableFunc(s, func(a, b string) int {
			if a == b {
				return 0
			}
			if natural.Less(a, b) {
				return -1
			} else {
				return 1
			}
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

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": l,
		"list":  resp,
	}))
}

func AdminAddVendorBackend(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.AddVendorBackendReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := vendor.AddVendorBackend(ctx, (*dbModel.VendorBackend)(&req)); err != nil {
		log.WithError(err).Error("add vendor backend error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminDeleteVendorBackends(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := vendor.DeleteVendorBackends(ctx, req.Endpoints); err != nil {
		log.WithError(err).Error("delete vendor backends error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminUpdateVendorBackends(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.AddVendorBackendReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := vendor.UpdateVendorBackend(ctx, (*dbModel.VendorBackend)(&req)); err != nil {
		log.WithError(err).Error("update vendor backend error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminReconnectVendorBackends(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
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
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(fmt.Sprintf("endpoint %s not found", v)))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}

func AdminEnableVendorBackends(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := vendor.EnableVendorBackends(ctx, req.Endpoints); err != nil {
		log.WithError(err).Error("enable vendor backends error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AdminDisableVendorBackends(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.VendorBackendEndpointsReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := vendor.DisableVendorBackends(ctx, req.Endpoints); err != nil {
		log.WithError(err).Error("disable vendor backends error")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SendTestEmail(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	var req model.SendTestEmailReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Email == "" {
		if err := user.SendTestEmail(); err != nil {
			log.Errorf("failed to send test email: %v", err)
			if errors.Is(err, op.ErrEmailUnbound) {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			} else {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			}
			return
		}
	} else {
		if err := email.SendTestEmail(user.Username, req.Email); err != nil {
			log.Errorf("failed to send test email: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}
