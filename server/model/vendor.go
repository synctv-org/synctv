package model

import (
	"errors"
	"fmt"
	"strings"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
)

type VendorMeResp[T any] struct {
	Info    T    `json:"info,omitempty"`
	IsLogin bool `json:"isLogin"`
}

type VendorFSListResp[T any] struct {
	Paths []*Path `json:"paths"`
	Items []T     `json:"items"`
	Total uint64  `json:"total"`
}

func GenDefaultPaths(path string, skipEmpty bool, paths ...*Path) []*Path {
	path = strings.TrimRight(path, "/")
	for _, v := range strings.Split(path, `/`) {
		if v == "" && skipEmpty {
			continue
		}

		if l := len(paths); l != 0 {
			paths = append(paths, &Path{
				Name: v,
				Path: fmt.Sprintf("%s/%s", strings.TrimRight(paths[l-1].Path, "/"), v),
			})
		} else {
			paths = append(paths, &Path{
				Name: v,
				Path: v,
			})
		}
	}

	return paths
}

type Path struct {
	Name string `json:"name"`
	Path string `json:"path"`
}

type Item struct {
	Name  string `json:"name"`
	Path  string `json:"path"`
	IsDir bool   `json:"isDir"`
}

type ServerIDReq struct {
	ServerID string `json:"serverId"`
}

func (r *ServerIDReq) Validate() error {
	if r.ServerID == "" {
		return errors.New("serverId is required")
	}
	return nil
}

func (r *ServerIDReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}
