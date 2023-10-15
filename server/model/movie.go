package model

import (
	"errors"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/room"
)

var (
	ErrUrlTooLong  = errors.New("url too long")
	ErrEmptyName   = errors.New("empty name")
	ErrNameTooLong = errors.New("name too long")
	ErrTypeTooLong = errors.New("type too long")

	ErrId = errors.New("id must be greater than 0")

	ErrEmptyIds = errors.New("empty ids")
)

type PushMovieReq room.BaseMovieInfo

func (p *PushMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(p)
}

func (p *PushMovieReq) Validate() error {
	if len(p.Url) > 1024 {
		return ErrUrlTooLong
	}

	if p.Name == "" {
		return ErrEmptyName
	} else if len(p.Name) > 1024 {
		return ErrNameTooLong
	}

	if len(p.Type) > 32 {
		return ErrTypeTooLong
	}

	return nil
}

type IdReq struct {
	Id uint64 `json:"id"`
}

func (i *IdReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IdReq) Validate() error {
	if i.Id <= 0 {
		return ErrId
	}
	return nil
}

type EditMovieReq struct {
	IdReq
	PushMovieReq
}

func (e *EditMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(e)
}

func (e *EditMovieReq) Validate() error {
	if err := e.IdReq.Validate(); err != nil {
		return err
	}
	if err := e.PushMovieReq.Validate(); err != nil {
		return err
	}
	return nil
}

type IdsReq struct {
	Ids []uint64 `json:"ids"`
}

func (i *IdsReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IdsReq) Validate() error {
	if len(i.Ids) == 0 {
		return ErrEmptyIds
	}
	return nil
}

type SwapMovieReq struct {
	Id1 uint64 `json:"id1"`
	Id2 uint64 `json:"id2"`
}

func (s *SwapMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SwapMovieReq) Validate() error {
	if s.Id1 <= 0 || s.Id2 <= 0 {
		return ErrId
	}
	return nil
}
