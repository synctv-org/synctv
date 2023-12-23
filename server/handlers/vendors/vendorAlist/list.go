package vendorAlist

import (
	"errors"
	"fmt"
	"net/http"
	"path/filepath"
	"strings"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/alist"
)

type ListReq struct {
	Path     string `json:"path"`
	Password string `json:"password"`
	Refresh  bool   `json:"refresh"`
}

func (r *ListReq) Validate() error {
	if r.Path == "" {
		r.Path = "/"
	}
	return nil
}

func (r *ListReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func List(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := ListReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	req.Path = strings.TrimRight(req.Path, "/")
	if !strings.HasPrefix(req.Path, "/") {
		req.Path = "/" + req.Path
	}

	aucd, err := user.AlistCache().Get(ctx)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("alist not login"))
			return
		}

		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	page, size, err := utils.GetPageAndMax(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var cli = vendor.LoadAlistClient(ctx.Query("backend"))
	data, err := cli.FsList(ctx, &alist.FsListReq{
		Token:    aucd.Token,
		Password: req.Password,
		Path:     req.Path,
		Host:     aucd.Host,
		Refresh:  req.Refresh,
		Page:     uint64(page),
		PerPage:  uint64(size),
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	var resp model.VendorFSListResp
	resp.Total = data.Total
	if req.Path == "/" {
		req.Path = ""
	}
	for i, v := range strings.Split(req.Path, string(filepath.Separator)) {
		var p = v
		if i != 0 {
			p = fmt.Sprintf("%s/%s", resp.Paths[i-1].Path, v)
		}
		resp.Paths = append(resp.Paths, &model.Path{
			Name: v,
			Path: p,
		})
	}
	for _, flr := range data.Content {
		resp.Items = append(resp.Items, &model.Item{
			Name:  flr.Name,
			Path:  fmt.Sprintf("%s/%s", req.Path, flr.Name),
			IsDir: flr.IsDir,
		})
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&resp))
}
