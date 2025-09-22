package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

func RoomMembers(ctx *gin.Context) {
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get room users failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	scopes := []func(db *gorm.DB) *gorm.DB{}

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

func RoomAdminMembers(ctx *gin.Context) {
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
			Joins("JOIN room_members ON users.id = room_members.user_id").
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

func RoomAdminApproveMember(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomApproveMemberReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room approve user req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.ApproveRoomPendingMember(room, req.ID)
	if err != nil {
		log.Errorf("approve room user failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomAdminDeleteMember(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomApproveMemberReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room delete user req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.DeleteRoomMember(room, req.ID)
	if err != nil {
		log.Errorf("delete room user failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomAdminBanMember(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomBanMemberReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room ban user req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.BanRoomMember(room, req.ID)
	if err != nil {
		log.Errorf("ban room user failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomAdminUnbanMember(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomUnbanMemberReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room unban user req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.UnbanRoomMember(room, req.ID)
	if err != nil {
		log.Errorf("unban room user failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomSetMemberPermissions(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomSetMemberPermissionsReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room set user permissions req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.SetMemberPermissions(room, req.ID, req.Permissions)
	if err != nil {
		log.Errorf("set room user permissions failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomSetAdmin(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomSetAdminReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room set admin req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.SetRoomAdmin(room, req.ID, req.AdminPermissions)
	if err != nil {
		log.Errorf("set room admin failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomSetMember(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomSetMemberReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room set user req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.SetRoomMember(room, req.ID, req.Permissions)
	if err != nil {
		log.Errorf("set room user failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomSetAdminPermissions(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	room := middlewares.GetRoomEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	var req model.RoomSetAdminPermissionsReq
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("decode room set admin permissions req failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := user.SetRoomAdminPermissions(room, req.ID, req.AdminPermissions)
	if err != nil {
		log.Errorf("set room admin permissions failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
