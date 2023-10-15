package model

import "github.com/gin-gonic/gin"

type Decoder interface {
	Decode(ctx *gin.Context) error
	Validate() error
}

func Decode(ctx *gin.Context, decoder Decoder) error {
	if err := decoder.Decode(ctx); err != nil {
		return err
	}
	if err := decoder.Validate(); err != nil {
		return err
	}
	return nil
}
