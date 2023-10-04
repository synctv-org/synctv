package public

import (
	"embed"
	"io/fs"
)

//go:embed dist/*
var dist embed.FS

var Public, _ = fs.Sub(dist, "dist")
