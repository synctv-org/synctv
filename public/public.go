package public

import (
	"embed"
	"io/fs"
)

//go:embed all:dist
var dist embed.FS

var Public, _ = fs.Sub(dist, "dist")
