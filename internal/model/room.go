package model

import (
	"time"

	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type RoomStatus uint8

const (
	RoomStatusBanned  RoomStatus = 1
	RoomStatusPending RoomStatus = 2
	RoomStatusActive  RoomStatus = 3
)

func (r RoomStatus) String() string {
	switch r {
	case RoomStatusBanned:
		return "banned"
	case RoomStatusPending:
		return "pending"
	case RoomStatusActive:
		return "active"
	default:
		return "unknown"
	}
}

type Room struct {
	ID             string `gorm:"primaryKey;type:char(32)" json:"id"`
	CreatedAt      time.Time
	UpdatedAt      time.Time
	Status         RoomStatus    `gorm:"not null;default:2"`
	Name           string        `gorm:"not null;uniqueIndex;type:varchar(32)"`
	Settings       *RoomSettings `gorm:"foreignKey:ID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE" json:"settings"`
	CreatorID      string        `gorm:"index;type:char(32)"`
	HashedPassword []byte
	RoomMembers    []*RoomMember `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies         []*Movie      `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
}

func (r *Room) BeforeCreate(tx *gorm.DB) error {
	if r.ID == "" {
		r.ID = utils.SortUUID()
	}
	return nil
}

func (r *Room) NeedPassword() bool {
	return len(r.HashedPassword) != 0
}

func (r *Room) CheckPassword(password string) bool {
	return !r.NeedPassword() || bcrypt.CompareHashAndPassword(r.HashedPassword, stream.StringToBytes(password)) == nil
}

func (r *Room) IsBanned() bool {
	return r.Status == RoomStatusBanned
}

func (r *Room) IsPending() bool {
	return r.Status == RoomStatusPending
}

func (r *Room) IsActive() bool {
	return r.Status == RoomStatusActive
}

type RoomSettings struct {
	ID                     string               `gorm:"primaryKey;type:char(32)" json:"-"`
	UpdatedAt              time.Time            `gorm:"autoUpdateTime" json:"-"`
	Hidden                 bool                 `gorm:"default:false" json:"hidden"`
	DisableJoinNewUser     bool                 `gorm:"default:false" json:"disable_join_new_user"`
	JoinNeedReview         bool                 `gorm:"default:false" json:"join_need_review"`
	UserDefaultPermissions RoomMemberPermission `json:"user_default_permissions"`
	DisableGuest           bool                 `gorm:"default:false" json:"disable_guest"`
	GuestPermissions       RoomMemberPermission `json:"guest_permissions"`

	CanGetMovieList     bool `gorm:"default:true" json:"can_get_movie_list"`
	CanAddMovie         bool `gorm:"default:true" json:"can_add_movie"`
	CanDeleteMovie      bool `gorm:"default:true" json:"can_delete_movie"`
	CanEditMovie        bool `gorm:"default:true" json:"can_edit_movie"`
	CanSetCurrentMovie  bool `gorm:"default:true" json:"can_set_current_movie"`
	CanSetCurrentStatus bool `gorm:"default:true" json:"can_set_current_status"`
	CanSendChatMessage  bool `gorm:"default:true" json:"can_send_chat_message"`
}

func DefaultRoomSettings() *RoomSettings {
	return &RoomSettings{
		Hidden:                 false,
		DisableJoinNewUser:     false,
		JoinNeedReview:         false,
		UserDefaultPermissions: DefaultPermissions,
		DisableGuest:           false,
		GuestPermissions:       NoPermission,

		CanGetMovieList:     true,
		CanAddMovie:         true,
		CanDeleteMovie:      true,
		CanEditMovie:        true,
		CanSetCurrentMovie:  true,
		CanSetCurrentStatus: true,
		CanSendChatMessage:  true,
	}
}
