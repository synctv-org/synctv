package model

import (
	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	dbModel "github.com/synctv-org/synctv/internal/model"
)

type RoomMembersResp struct {
	UserID           string                       `json:"userId"`
	Username         string                       `json:"username"`
	JoinAt           int64                        `json:"joinAt"`
	IsOnline         bool                         `json:"isOnline"`
	Role             dbModel.RoomMemberRole       `json:"role"`
	Status           dbModel.RoomMemberStatus     `json:"status"`
	RoomID           string                       `json:"roomId"`
	Permissions      dbModel.RoomMemberPermission `json:"permissions"`
	AdminPermissions dbModel.RoomAdminPermission  `json:"adminPermissions"`
}

type RoomApproveMemberReq = UserIDReq
type RoomBanMemberReq = UserIDReq
type RoomUnbanMemberReq = UserIDReq

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
