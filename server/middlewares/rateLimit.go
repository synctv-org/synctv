package middlewares

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/server/model"
	limiter "github.com/ulule/limiter/v3"
	mgin "github.com/ulule/limiter/v3/drivers/middleware/gin"
	"github.com/ulule/limiter/v3/drivers/store/memory"
)

func NewLimiter(Period time.Duration, Limit int64, options ...limiter.Option) gin.HandlerFunc {
	limit := limiter.New(memory.NewStore(), limiter.Rate{
		Period: Period,
		Limit:  Limit,
	}, options...)
	return mgin.NewMiddleware(limit, mgin.WithLimitReachedHandler(func(c *gin.Context) {
		c.JSON(http.StatusTooManyRequests, model.NewApiErrorStringResp("too many requests"))
	}))
}
