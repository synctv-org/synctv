package vendorEmby

import (
	"errors"
	"net/http"
	"strconv"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/emby"
)

type ListReq struct {
	Path string `json:"path"`
}

func (r *ListReq) Validate() error {
	if r.Path == "" {
		return nil
	}
	i, err := strconv.Atoi(r.Path)
	if err != nil {
		return err
	}
	if i < 0 {
		return errors.New("path is invalid")
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

	aucd, err := user.EmbyCache().Get(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	page, size, err := utils.GetPageAndMax(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	cli := vendor.LoadEmbyClient(ctx.Query("backend"))
	data, err := cli.FsList(ctx, &emby.FsListReq{
		Host:       aucd.Host,
		Path:       req.Path,
		Token:      aucd.ApiKey,
		Limit:      uint64(size),
		StartIndex: uint64((page - 1) * size),
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	var resp model.VendorFSListResp
	for _, p := range data.Paths {
		var n = p.Name
		if p.Path == "1" {
			n = ""
		}
		resp.Paths = append(resp.Paths, &model.Path{
			Name: n,
			Path: p.Path,
		})
	}
	for _, i := range data.Items {
		resp.Items = append(resp.Items, &model.Item{
			Name:  i.Name,
			Path:  i.Id,
			IsDir: i.IsFolder,
		})
	}

	resp.Total = data.Total
	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}
