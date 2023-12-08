package model

type VendorMeResp[T any] struct {
	IsLogin bool `json:"isLogin"`
	Info    T    `json:"info,omitempty"`
}
