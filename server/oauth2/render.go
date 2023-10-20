package auth

import (
	"embed"
	"html/template"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/utils"
	synccache "github.com/synctv-org/synctv/utils/syncCache"
	"golang.org/x/oauth2"
)

//go:embed templates/redirect.html
var temp embed.FS

var (
	redirectTemplate *template.Template
	states           *synccache.SyncCache[string, struct{}]
)

func Render(ctx *gin.Context, c *oauth2.Config, option ...oauth2.AuthCodeOption) error {
	state := utils.RandString(16)
	states.Store(state, struct{}{}, time.Minute*5)
	return redirectTemplate.Execute(ctx.Writer, c.AuthCodeURL(state, option...))
}

func init() {
	redirectTemplate = template.Must(template.ParseFS(temp, "templates/redirect.html"))
	states = synccache.NewSyncCache[string, struct{}](time.Minute * 10)
}
