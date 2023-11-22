package handlers

import (
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
		resp := make(model.AdminSettingsResp, len(settings.GroupSettings))
		for sg, v := range settings.GroupSettings {
			if resp[string(sg)] == nil {
				resp[string(sg)] = make(gin.H, len(v))
			}
			for _, s2 := range v {
				resp[string(sg)][s2.Name()] = s2.Interface()
			}
		}
		ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
		return
	}

	s, ok := settings.GroupSettings[dbModel.SettingGroup(group)]
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("group not found"))
		return
	}
	resp := make(gin.H, len(s))
	for _, v := range s {
		resp[v.Name()] = v.Interface()
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}

func Users(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, id
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			scopes = append(scopes, db.WhereUsernameLikeOrIDIn(keyword, db.GerUsersIDByIDLike(keyword)))
		case "name":
			scopes = append(scopes, db.WhereUsernameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDIn(db.GerUsersIDByIDLike(keyword)))
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllUserCount(scopes...),
		"list":  genUserListResp(db.GetAllUsers(append(scopes, db.Paginate(page, pageSize))...)),
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

func GetRoomUsers(ctx *gin.Context) {
	id := ctx.Query("id")
	if len(id) != 32 {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("room id error"))
		return
	}

	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{
		db.PreloadRoomUserRelations(db.WhereRoomID(id)),
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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, id
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			scopes = append(scopes, db.WhereUsernameLikeOrIDIn(keyword, db.GerUsersIDByIDLike(keyword)))
		case "name":
			scopes = append(scopes, db.WhereUsernameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDIn(db.GerUsersIDByIDLike(keyword)))
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllUserCount(scopes...),
		"list":  genRoomUserListResp(db.GetAllUsers(append(scopes, db.Paginate(page, pageSize))...)),
	}))
}

func genRoomUserListResp(us []*dbModel.User) []*model.RoomUsersResp {
	resp := make([]*model.RoomUsersResp, len(us))
	for i, v := range us {
		resp[i] = &model.RoomUsersResp{
			UserID:      v.ID,
			Username:    v.Username,
			Role:        v.Role,
			JoinAt:      v.RoomUserRelations[0].CreatedAt.UnixMilli(),
			RoomID:      v.RoomUserRelations[0].RoomID,
			Status:      v.RoomUserRelations[0].Status,
			Permissions: v.RoomUserRelations[0].Permissions,
		}
	}
	return resp
}

func ApprovePendingUser(ctx *gin.Context) {
	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	user, err := db.GetUserByID(req.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !user.IsPending() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user is not pending"))
		return
	}

	err = db.SetRoleByID(req.ID, dbModel.RoleUser)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func BanUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)
	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	u, err := db.GetUserByID(req.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if u.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot ban root"))
		return
	}

	if u.IsAdmin() && !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot ban admin"))
		return
	}

	err = op.SetRoleByID(req.ID, dbModel.RoleBanned)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UnBanUser(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	u, err := db.GetUserByID(req.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !u.IsBanned() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user is not banned"))
		return
	}

	err = op.SetRoleByID(req.ID, dbModel.RoleUser)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func Rooms(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)

	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
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
		case "creator":
			scopes = append(scopes, db.WhereCreatorIDIn(db.GerUsersIDByUsernameLike(keyword)))
		case "creatorId":
			scopes = append(scopes, db.WhereCreatorID(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllRoomsCount(scopes...),
		"list":  genRoomListResp(append(scopes, db.Paginate(page, pageSize))...),
	}))
}

func GetUserRooms(ctx *gin.Context) {
	id := ctx.Query("id")
	if len(id) != 32 {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user id error"))
		return
	}
	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
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

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllRoomsCount(scopes...),
		"list":  genRoomListResp(append(scopes, db.Paginate(page, pageSize))...),
	}))
}

func ApprovePendingRoom(ctx *gin.Context) {
	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	room, err := db.GetRoomByID(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !room.IsPending() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("room is not pending"))
		return
	}

	err = db.SetRoomStatus(req.Id, dbModel.RoomStatusActive)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func BanRoom(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)
	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := db.GetRoomByID(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	creator, err := db.GetUserByID(r.CreatorID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if creator.IsAdmin() && !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("cannot ban admin"))
		return
	}

	err = op.SetRoomStatus(req.Id, dbModel.RoomStatusBanned)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UnBanRoom(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	req := model.RoomIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := db.GetRoomByID(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !r.IsBanned() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("room is not banned"))
		return
	}

	err = op.SetRoomStatus(req.Id, dbModel.RoomStatusActive)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func AddUser(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)

	req := model.AddUserReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	_, err := op.CreateOrLoadUser(req.Username, req.Password, db.WithRole(req.Role))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DeleteUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := model.UserIDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	u, err := db.GetUserByID(req.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if u.IsAdmin() && !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("cannot delete admin"))
		return
	}

	if err := op.DeleteUserByID(req.ID); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
