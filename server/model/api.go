package model

import (
	"regexp"
	"time"
)

var (
	alnumReg         = regexp.MustCompile(`^[[:alnum:]]+$`)
	alnumPrintReg    = regexp.MustCompile(`^[[:print:][:alnum:]]+$`)
	alnumPrintHanReg = regexp.MustCompile(`^[[:print:][:alnum:]\p{Han}]+$`)
	emailReg         = regexp.MustCompile(`^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`)
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
