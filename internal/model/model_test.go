package model_test

import (
	"testing"

	"github.com/glebarez/sqlite"
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

func TestAutoMigrate(t *testing.T) {
	// db, err := gorm.Open(sqlite.Open("file::memory:?cache=shared"), &gorm.Config{})
	db, err := gorm.Open(sqlite.Open("./sqlite.db"), &gorm.Config{})
	if err != nil {
		t.Fatal(err)
	}
	err = db.AutoMigrate(new(model.Movie), new(model.Room), new(model.User), new(model.RoomUserRelation))
	if err != nil {
		t.Fatal(err)
	}
}

func TestCreateRoom(t *testing.T) {
	db, err := gorm.Open(sqlite.Open("./sqlite.db"), &gorm.Config{})
	if err != nil {
		t.Fatal(err)
	}
	room := model.Room{
		Name:           "test",
		HashedPassword: nil,
	}
	err = db.Create(&room).Error
	if err != nil {
		t.Fatal(err)
	}
}

func TestCreateUser(t *testing.T) {
	db, err := gorm.Open(sqlite.Open("./sqlite.db"), &gorm.Config{})
	if err != nil {
		t.Fatal(err)
	}
	user := model.User{
		Username:           "user1",
		GroupUserRelations: []model.RoomUserRelation{},
	}
	err = db.Create(&user).Error
	if err != nil {
		t.Fatal(err)
	}
}

func TestAddUserToRoom(t *testing.T) {
	db, err := gorm.Open(sqlite.Open("./sqlite.db"), &gorm.Config{})
	if err != nil {
		t.Fatal(err)
	}
	ur := model.RoomUserRelation{
		UserID:      1,
		RoomID:      1,
		Role:        model.RoomRoleUser,
		Permissions: model.DefaultPermissions,
	}
	err = db.Create(&ur).Error
	if err != nil {
		t.Fatal(err)
	}
}
