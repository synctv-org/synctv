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
	ErrURLTooLong  = errors.New("url too long")
	ErrEmptyName   = errors.New("empty name")
	ErrTypeTooLong = errors.New("type too long")

	ErrID = errors.New("id length must be 32")

	ErrEmptyIDs = errors.New("empty ids")
)

type PushMovieReq model.MovieBase

func (p *PushMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(p)
}

func (p *PushMovieReq) Validate() error {
	if len(p.URL) > 8192 {
		return ErrURLTooLong
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

type IDReq struct {
	ID string `json:"id"`
}

func (i *IDReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IDReq) Validate() error {
	if len(i.ID) != 32 {
		return ErrID
	}
	return nil
}

type IDCanEmptyReq struct {
	ID string `json:"id"`
}

func (i *IDCanEmptyReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IDCanEmptyReq) Validate() error {
	if len(i.ID) != 32 && i.ID != "" {
		return ErrID
	}
	return nil
}

type SetRoomCurrentMovieReq struct {
	IDCanEmptyReq
	SubPath string `json:"subPath"`
}

func (s *SetRoomCurrentMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

type EditMovieReq struct {
	IDReq
	PushMovieReq
}

func (e *EditMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(e)
}

func (e *EditMovieReq) Validate() error {
	if err := e.IDReq.Validate(); err != nil {
		return err
	}
	if err := e.PushMovieReq.Validate(); err != nil {
		return err
	}
	return nil
}

type IDsReq struct {
	IDs []string `json:"ids"`
}

func (i *IDsReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(i)
}

func (i *IDsReq) Validate() error {
	if len(i.IDs) == 0 {
		return ErrEmptyIDs
	}
	for _, v := range i.IDs {
		if len(v) != 32 {
			return ErrID
		}
	}
	return nil
}

type SwapMovieReq struct {
	ID1 string `json:"id1"`
	ID2 string `json:"id2"`
}

func (s *SwapMovieReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SwapMovieReq) Validate() error {
	if len(s.ID1) != 32 || len(s.ID2) != 32 {
		return ErrID
	}
	return nil
}

func GenDefaultSubPaths(id, path string, skipEmpty bool, paths ...*MoviePath) []*MoviePath {
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

type MovieList struct {
	Paths  []*MoviePath `json:"paths"`
	Movies []*Movie     `json:"movies"`
	Total  int64        `json:"total"`
}

type MoviesResp struct {
	*MovieList
	Dynamic bool `json:"dynamic"`
}

type Movie struct {
	ID        string          `json:"id"`
	Creator   string          `json:"creator"`
	CreatorID string          `json:"creatorId"`
	SubPath   string          `json:"subPath"`
	Base      model.MovieBase `json:"base"`
	CreatedAt int64           `json:"createAt"`
}

type CurrentMovieResp struct {
	Movie    *Movie    `json:"movie"`
	Status   op.Status `json:"status"`
	ExpireID uint64    `json:"expireId"`
}

type ClearMoviesReq struct {
	ParentID string `json:"parentId"`
}

func (c *ClearMoviesReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(c)
}

func (c *ClearMoviesReq) Validate() error {
	if c.ParentID != "" && len(c.ParentID) != 32 {
		return errors.New("parent id length must be empty or 32")
	}
	return nil
}
