package bootstrap

import (
	"context"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
)

func InitRoom(ctx context.Context) error {
	r, err := db.GetAllRooms()
	if err != nil {
		return err
	}
	for _, room := range r {
		r, err := op.LoadRoom(&room)
		if err != nil {
			log.Errorf("load room error: %v", err)
			return err
		}
		m, err := r.GetAllMoviesByRoomID()
		if err != nil {
			log.Errorf("get all movies by room id error: %v", err)
			return err
		}
		for _, movie := range m {
			err = r.InitMovie(&movie)
			if err != nil {
				log.Errorf("init movie error: %v", err)
				return err
			}
		}
	}
	return nil
}
