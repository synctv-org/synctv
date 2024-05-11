package model

import (
	"errors"
	"fmt"
	"strings"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/utils"
)

var (
	ErrUrlTooLong  = errors.New("url too long")
	ErrEmptyName   = errors.New("empty name")
	ErrTypeTooLong = errors.New("type too long")

	ErrId = errors.New("id length must be 32")

	ErrEmptyIds = errors.New("empty ids")
)

type PushMovieReq model.MovieBase

func (p *PushMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(p)
}

func (p *PushMovieReq) Validate() error {
	if len(p.Url) > 8192 {
		return ErrUrlTooLong
	}

	if p.Name == "" {
		return ErrEmptyName
	} else if len(p.Name) > 256 {
		// 从最后一个完整rune截断而不是返回错误
		p.Name = utils.TruncateByRune(p.Name, 253) + "..."
	}

	if len(p.Type) > 32 {
		return ErrTypeTooLong
	}

	return nil
}

type PushMoviesReq []*PushMovieReq

func (p *PushMoviesReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(p)
}

func (p *PushMoviesReq) Validate() error {
	for _, v := range *p {
		if err := v.Validate(); err != nil {
			return err
		}
	}
	return nil
}

type IdReq struct {
	Id string `json:"id"`
}

func (i *IdReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IdReq) Validate() error {
	if len(i.Id) != 32 {
		return ErrId
	}
	return nil
}

type IdCanEmptyReq struct {
	Id string `json:"id"`
}

func (i *IdCanEmptyReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IdCanEmptyReq) Validate() error {
	if len(i.Id) != 32 && i.Id != "" {
		return ErrId
	}
	return nil
}

type SetRoomCurrentMovieReq struct {
	IdCanEmptyReq
	SubPath string `json:"subPath"`
}

func (s *SetRoomCurrentMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
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
	Ids []string `json:"ids"`
}

func (i *IdsReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IdsReq) Validate() error {
	if len(i.Ids) == 0 {
		return ErrEmptyIds
	}
	for _, v := range i.Ids {
		if len(v) != 32 {
			return ErrId
		}
	}
	return nil
}

type SwapMovieReq struct {
	Id1 string `json:"id1"`
	Id2 string `json:"id2"`
}

func (s *SwapMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SwapMovieReq) Validate() error {
	if len(s.Id1) != 32 || len(s.Id2) != 32 {
		return ErrId
	}
	return nil
}

func GenDefaultSubPaths(path string, skipEmpty bool, paths ...*MoviePath) []*MoviePath {
	if len(paths) == 0 {
		return nil
	}
	id := paths[len(paths)-1].ID
	path = strings.TrimRight(path, "/")
	for _, v := range strings.Split(path, `/`) {
		if v == "" && skipEmpty {
			continue
		}
		if l := len(paths); l != 0 {
			paths = append(paths, &MoviePath{
				Name:    v,
				ID:      id,
				SubPath: fmt.Sprintf("%s/%s", strings.TrimRight(paths[l-1].SubPath, "/"), v),
			})
		} else {
			paths = append(paths, &MoviePath{
				Name:    v,
				ID:      id,
				SubPath: v,
			})
		}
	}
	return paths
}

type MoviePath struct {
	Name    string `json:"name"`
	ID      string `json:"id"`
	SubPath string `json:"subPath"`
}

type MoviesResp struct {
	Paths   []*MoviePath `json:"paths"`
	Movies  []*Movie     `json:"movies"`
	Total   int64        `json:"total"`
	Dynamic bool         `json:"dynamic"`
}

type Movie struct {
	Id        string          `json:"id"`
	CreatedAt int64           `json:"createAt"`
	Base      model.MovieBase `json:"base"`
	Creator   string          `json:"creator"`
	CreatorId string          `json:"creatorId"`
	SubPath   string          `json:"subPath"`
}

type CurrentMovieResp struct {
	Status   op.Status `json:"status"`
	Movie    *Movie    `json:"movie"`
	ExpireId uint64    `json:"expireId"`
}
