package model

import (
	"fmt"
	"strings"
)

type VendorMeResp[T any] struct {
	IsLogin bool `json:"isLogin"`
	Info    T    `json:"info,omitempty"`
}

type VendorFSListResp[T any] struct {
	Paths []*Path `json:"paths"`
	Items []T     `json:"items"`
	Total uint64  `json:"total"`
}

func GenDefaultPaths(path string) []*Path {
	paths := []*Path{}
	path = strings.TrimRight(path, "/")
	for i, v := range strings.Split(path, `/`) {
		if i != 0 {
			paths = append(paths, &Path{
				Name: v,
				Path: fmt.Sprintf("%s/%s", paths[i-1].Path, v),
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
