package static

import (
	"io/fs"
	"net/http"
	"net/url"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/public"
	"github.com/synctv-org/synctv/server/middlewares"
)

func Init(e *gin.Engine) {
	{
		e.GET("/", func(ctx *gin.Context) {
			ctx.Redirect(http.StatusMovedPermanently, "/web/")
		})

		web := e.Group("/web")

		web.Use(middlewares.NewDistCacheControl("/web/"))

		err := initFSRouter(web, public.Public.(fs.ReadDirFS), ".")
		if err != nil {
			panic(err)
		}

		e.NoRoute(func(ctx *gin.Context) {
			if strings.HasPrefix(ctx.Request.URL.Path, "/web/") {
				ctx.FileFromFS("", http.FS(public.Public))
				return
			}
		})
	}
}

func initFSRouter(e *gin.RouterGroup, f fs.ReadDirFS, path string) error {
	dirs, err := f.ReadDir(path)
	if err != nil {
		return err
	}
	for _, dir := range dirs {
		u, err := url.JoinPath(path, dir.Name())
		if err != nil {
			return err
		}
		if dir.IsDir() {
			err = initFSRouter(e, f, u)
			if err != nil {
				return err
			}
		} else {
			e.StaticFileFS(u, u, http.FS(f))
		}
	}
	return nil
}
