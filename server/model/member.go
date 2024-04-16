package model

import (
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

type RoomSetMemberReq struct {
	UserIDReq
	Permissions dbModel.RoomMemberPermission `json:"permissions"`
}

type RoomSetAdminPermissionsReq struct {
	UserIDReq
	AdminPermissions dbModel.RoomAdminPermission `json:"adminPermissions"`
}
