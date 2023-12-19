package model

type VendorMeResp[T any] struct {
	IsLogin bool `json:"isLogin"`
	Info    T    `json:"info,omitempty"`
}

type VendorFSListResp struct {
	Paths []*Path `json:"paths"`
	Items []*Item `json:"items"`
	Total uint64  `json:"total"`
}

type Item struct {
	Name  string `json:"name"`
	Path  string `json:"path"`
	IsDir bool   `json:"isDir"`
}

type Path struct {
	Name string `json:"name"`
	Path string `json:"path"`
}
