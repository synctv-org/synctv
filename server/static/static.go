package static

import (
	"io/fs"
	"net/http"
	"path/filepath"

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
			ctx.FileFromFS("", http.FS(public.Public))
		})

	}
}

func initFSRouter(e *gin.RouterGroup, f fs.ReadDirFS, path string) error {
	dirs, err := f.ReadDir(path)
	if err != nil {
		return err
	}
	for _, dir := range dirs {
		if dir.IsDir() {
			return initFSRouter(e, f, filepath.Join(path, dir.Name()))
		} else {
			e.StaticFileFS(filepath.Join(path, dir.Name()), filepath.Join(path, dir.Name()), http.FS(f))
		}
	}
	return nil
}
