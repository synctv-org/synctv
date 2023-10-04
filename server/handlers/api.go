package handlers

import (
	"time"
)

type ApiResp struct {
	Time  int64  `json:"time"`
	Error string `json:"error,omitempty"`
	Data  any    `json:"data,omitempty"`
}

func (ar *ApiResp) SetError(err error) {
	ar.Error = err.Error()
}

func (ar *ApiResp) SetDate(data any) {
	ar.Data = data
}

func NewApiErrorResp(err error) *ApiResp {
	return &ApiResp{
		Time:  time.Now().UnixMicro(),
		Error: err.Error(),
	}
}

func NewApiErrorStringResp(err string) *ApiResp {
	return &ApiResp{
		Time:  time.Now().UnixMicro(),
		Error: err,
	}
}

func NewApiDataResp(data any) *ApiResp {
	return &ApiResp{
		Time: time.Now().UnixMicro(),
		Data: data,
	}
}
