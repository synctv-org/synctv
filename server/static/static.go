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

func newFSHandler(fileSys fs.FS) func(ctx *gin.Context) {
	return func(ctx *gin.Context) {
		fp := strings.Trim(ctx.Param("filepath"), "/")
		f, err := fileSys.Open(fp)
		if err != nil {
			fp = ""
		} else {
			f.Close()
		}
		ctx.FileFromFS(fp, http.FS(fileSys))
	}
}

func newStatCachedFSHandler(fileSys fs.FS) (func(ctx *gin.Context), error) {
	cache := make(map[string]struct{})
	err := fs.WalkDir(fileSys, ".", func(path string, d fs.DirEntry, err error) error {
		cache[`/`+path] = struct{}{}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return func(ctx *gin.Context) {
		fp := ctx.Param("filepath")
		if _, ok := cache[fp]; !ok {
			fp = ""
		}
		ctx.FileFromFS(fp, http.FS(fileSys))
	}, nil
}

func SiglePageAppFS(r *gin.RouterGroup, fileSys fs.FS, cacheStat bool) error {
	var h func(ctx *gin.Context)
	if cacheStat {
		var err error
		h, err = newStatCachedFSHandler(fileSys)
		if err != nil {
			return err
		}
	} else {
		h = newFSHandler(fileSys)
	}
	r.GET("/*filepath", h)
	r.HEAD("/*filepath", h)
	return nil
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
