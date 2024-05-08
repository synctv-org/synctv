package vendorEmby

import (
	"errors"
	"fmt"
	"net/http"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/emby"
	"gorm.io/gorm"
)

type ListReq struct {
	Path     string `json:"path"`
	Keywords string `json:"keywords"`
}

func (r *ListReq) Validate() (err error) {
	return nil
}

func (r *ListReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

type EmbyFileItem struct {
	*model.Item
	Type string `json:"type"`
}

type EmbyFSListResp = model.VendorFSListResp[*EmbyFileItem]

func List(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := ListReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	page, size, err := utils.GetPageAndMax(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Path == "" {
		if req.Keywords != "" {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("keywords is not supported when not choose server (server id is empty)"))
			return
		}
		socpes := [](func(*gorm.DB) *gorm.DB){
			db.OrderByCreatedAtAsc,
		}

		total, err := db.GetEmbyVendorsCount(user.ID, socpes...)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		if total == 0 {
			ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("emby server not found"))
			return
		}

		ev, err := db.GetEmbyVendors(user.ID, append(socpes, db.Paginate(page, size))...)
		if err != nil {
			if errors.Is(err, db.ErrNotFound("vendor")) {
				ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("emby server not found"))
				return
			}
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}

		if total == 1 {
			req.Path = ev[0].ServerID + "/"
			goto EmbyFSListResp
		}

		resp := EmbyFSListResp{
			Paths: []*model.Path{
				{
					Name: "",
					Path: "",
				},
			},
			Total: uint64(total),
		}

		for _, evi := range ev {
			resp.Items = append(resp.Items, &EmbyFileItem{
				Item: &model.Item{
					Name:  evi.Host,
					Path:  evi.ServerID + `/`,
					IsDir: true,
				},
				Type: "server",
			})
		}

		ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))

		return
	}

EmbyFSListResp:

	var serverID string
	serverID, req.Path, err = dbModel.GetEmbyServerIdFromPath(req.Path)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	aucd, err := user.EmbyCache().LoadOrStore(ctx, serverID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("emby server not found"))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	cli := vendor.LoadEmbyClient(ctx.Query("backend"))
	data, err := cli.FsList(ctx, &emby.FsListReq{
		Host:       aucd.Host,
		Path:       req.Path,
		Token:      aucd.ApiKey,
		UserId:     aucd.UserID,
		Limit:      uint64(size),
		StartIndex: uint64((page - 1) * size),
		SearchTerm: req.Keywords,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(fmt.Errorf("emby fs list error: %w", err)))
		return
	}

	var resp EmbyFSListResp = EmbyFSListResp{
		Paths: []*model.Path{
			{},
		},
	}
	for _, p := range data.Paths {
		var n = p.Name
		if p.Path == "1" {
			n = aucd.Host
		}
		resp.Paths = append(resp.Paths, &model.Path{
			Name: n,
			Path: fmt.Sprintf("%s/%s", aucd.ServerID, p.Path),
		})
	}
	for _, i := range data.Items {
		resp.Items = append(resp.Items, &EmbyFileItem{
			Item: &model.Item{
				Name:  i.Name,
				Path:  fmt.Sprintf("%s/%s", aucd.ServerID, i.Id),
				IsDir: i.IsFolder,
			},
			Type: i.Type,
		})
	}

	resp.Total = data.Total
	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}
