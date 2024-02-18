package static

import (
	"io/fs"
	"net/http"
	"net/url"
	"os"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/public"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/zijiren233/gencontainer/rwmap"
)

func Init(e *gin.Engine) {
	e.GET("/", func(ctx *gin.Context) {
		ctx.Redirect(http.StatusMovedPermanently, "/web/")
	})

	web := e.Group("/web")

	if flags.WebPath == "" {
		web.Use(middlewares.NewDistCacheControl("/web/"))

		SiglePageAppFS(web, public.Public, true)

		// err := initFSRouter(web, public.Public.(fs.ReadDirFS), ".")
		// if err != nil {
		// 	panic(err)
		// }

		// e.NoRoute(func(ctx *gin.Context) {
		// 	if strings.HasPrefix(ctx.Request.URL.Path, "/web/") {
		// 		ctx.FileFromFS("", http.FS(public.Public))
		// 		return
		// 	}
		// })
	} else {
		SiglePageAppFS(web, os.DirFS(flags.WebPath), false)

		// web.Static("/", flags.WebPath)

		// e.NoRoute(func(ctx *gin.Context) {
		// 	if strings.HasPrefix(ctx.Request.URL.Path, "/web/") {
		// 		ctx.FileFromFS("", http.Dir(flags.WebPath))
		// 		return
		// 	}
		// })
	}

}

func SiglePageAppFS(r *gin.RouterGroup, fileSys fs.FS, cacheStat bool) {
	const param = "filepath"
	var h func(ctx *gin.Context)
	if cacheStat {
		var cache = rwmap.RWMap[string, bool]{}
		h = func(ctx *gin.Context) {
			fp := strings.Trim(ctx.Param(param), "/")
			if stat, ok := cache.Load(fp); ok {
				if !stat {
					fp = ""
				}
			} else {
				f, err := fileSys.Open(fp)
				cache.LoadOrStore(fp, err == nil)
				if err != nil {
					fp = ""
				} else {
					f.Close()
				}
			}
			ctx.FileFromFS(fp, http.FS(fileSys))
		}
	} else {
		h = func(ctx *gin.Context) {
			fp := strings.Trim(ctx.Param(param), "/")
			f, err := fileSys.Open(fp)
			if err != nil {
				fp = ""
			} else {
				f.Close()
			}
			ctx.FileFromFS(fp, http.FS(fileSys))
		}
	}
	r.GET("/*"+param, h)
	r.HEAD("/*"+param, h)
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
