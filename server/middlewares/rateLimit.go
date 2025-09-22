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

func NewLimiter(period time.Duration, limit int64, options ...limiter.Option) gin.HandlerFunc {
	limiter := limiter.New(memory.NewStore(), limiter.Rate{
		Period: period,
		Limit:  limit,
	}, options...)

	return mgin.NewMiddleware(limiter, mgin.WithLimitReachedHandler(func(c *gin.Context) {
		c.JSON(http.StatusTooManyRequests, model.NewAPIErrorStringResp("too many requests"))
	}))
}
