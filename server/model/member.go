package model

import (
	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	dbModel "github.com/synctv-org/synctv/internal/model"
)

type RoomMembersResp struct {
	UserID           string                       `json:"userId"`
	Username         string                       `json:"username"`
	RoomID           string                       `json:"roomId"`
	JoinAt           int64                        `json:"joinAt"`
	OnlineCount      int                          `json:"onlineCount"`
	Permissions      dbModel.RoomMemberPermission `json:"permissions"`
	AdminPermissions dbModel.RoomAdminPermission  `json:"adminPermissions"`
	Role             dbModel.RoomMemberRole       `json:"role"`
	Status           dbModel.RoomMemberStatus     `json:"status"`
}

type (
	RoomApproveMemberReq = UserIDReq
	RoomBanMemberReq     = UserIDReq
	RoomUnbanMemberReq   = UserIDReq
)

type RoomSetMemberPermissionsReq struct {
	UserIDReq
	Permissions dbModel.RoomMemberPermission `json:"permissions"`
}

func (r *RoomSetMemberPermissionsReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

type RoomMeResp struct {
	UserID           string                       `json:"userId"`
	RoomID           string                       `json:"roomId"`
	JoinAt           int64                        `json:"joinAt"`
	Role             dbModel.RoomMemberRole       `json:"role"`
	Status           dbModel.RoomMemberStatus     `json:"status"`
	Permissions      dbModel.RoomMemberPermission `json:"permissions"`
	AdminPermissions dbModel.RoomAdminPermission  `json:"adminPermissions"`
}

type RoomSetAdminReq struct {
	UserIDReq
	AdminPermissions dbModel.RoomAdminPermission `json:"adminPermissions"`
}

func (r *RoomSetAdminReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

type RoomSetMemberReq struct {
	UserIDReq
	Permissions dbModel.RoomMemberPermission `json:"permissions"`
}

func (r *RoomSetMemberReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

type RoomSetAdminPermissionsReq struct {
	UserIDReq
	AdminPermissions dbModel.RoomAdminPermission `json:"adminPermissions"`
}

func (r *RoomSetAdminPermissionsReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}
