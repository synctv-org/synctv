package middlewares

import (
	"time"

	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	limiter "github.com/ulule/limiter/v3"
)

func Init(e *gin.Engine) {
	w := log.StandardLogger().Writer()
	e.
		Use(NewLog(log.StandardLogger())).
		Use(gin.RecoveryWithWriter(w)).
		Use(NewCors())

	if conf.Conf.RateLimit.Enable {
		d, err := time.ParseDuration(conf.Conf.RateLimit.Period)
		if err != nil {
			log.Fatal(err)
		}

		options := []limiter.Option{
			limiter.WithTrustForwardHeader(conf.Conf.RateLimit.TrustForwardHeader),
		}
		if conf.Conf.RateLimit.TrustedClientIPHeader != "" {
			options = append(
				options,
				limiter.WithClientIPHeader(conf.Conf.RateLimit.TrustedClientIPHeader),
			)
		}

		e.Use(NewLimiter(d, conf.Conf.RateLimit.Limit, options...))
	}
}
