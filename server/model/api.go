package model

import (
	"regexp"
	"time"
)

var (
	// alnumReg         = regexp.MustCompile(`^[[:alnum:]]+$`)
	alnumPrintReg    = regexp.MustCompile(`^[[:print:][:alnum:]]+$`)
	alnumPrintHanReg = regexp.MustCompile(`^[[:print:][:alnum:]\p{Han}]+$`)
	emailReg         = regexp.MustCompile(`^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`)
)

type APIResp struct {
	Data  any    `json:"data,omitempty"`
	Error string `json:"error,omitempty"`
	Time  int64  `json:"time"`
}

func (ar *APIResp) SetError(err error) {
	ar.Error = err.Error()
}

func (ar *APIResp) SetDate(data any) {
	ar.Data = data
}

func NewAPIErrorResp(err error) *APIResp {
	return &APIResp{
		Time:  time.Now().UnixMicro(),
		Error: err.Error(),
	}
}

func NewAPIErrorStringResp(err string) *APIResp {
	return &APIResp{
		Time:  time.Now().UnixMicro(),
		Error: err,
	}
}

func NewAPIDataResp(data any) *APIResp {
	return &APIResp{
		Time: time.Now().UnixMicro(),
		Data: data,
	}
}
