package auth

import (
	"embed"
	"html/template"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/zijiren233/gencontainer/synccache"
)

//go:embed templates/*.html
var temp embed.FS

var (
	redirectTemplate *template.Template
	tokenTemplate    *template.Template
	states           *synccache.SyncCache[string, stateHandler]
)

type stateHandler func(ctx *gin.Context, pi provider.ProviderInterface, code string)

func RenderRedirect(ctx *gin.Context, url string) error {
	ctx.Header("Content-Type", "text/html; charset=utf-8")
	return redirectTemplate.Execute(ctx.Writer, url)
}

func RenderToken(ctx *gin.Context, url, token string) error {
	ctx.Header("Content-Type", "text/html; charset=utf-8")
	return tokenTemplate.Execute(ctx.Writer, map[string]string{"Url": url, "Token": token})
}

func init() {
	redirectTemplate = template.Must(template.ParseFS(temp, "templates/redirect.html"))
	tokenTemplate = template.Must(template.ParseFS(temp, "templates/token.html"))
	states = synccache.NewSyncCache[string, stateHandler](time.Minute * 10)
}
