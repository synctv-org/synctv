package vendorEmby

import (
	"errors"
	"fmt"
	"net/http"
	"strconv"

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
	ServerID string `json:"-"`
	Path     string `json:"path"`
	Keywords string `json:"keywords"`
}

func (r *ListReq) Validate() (err error) {
	if r.Path == "" {
		return nil
	}
	r.ServerID, r.Path, err = dbModel.GetEmbyServerIdFromPath(r.Path)
	if err != nil {
		return err
	}
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

type EmbyFileItem struct {
	*model.Item
	Type string `json:"type"`
}

type EmbyFSListResp = model.VendorFSListResp[*EmbyFileItem]

func List(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

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

	if req.ServerID == "" {
		if req.Keywords != "" {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("keywords is not supported when not choose server (server id is empty)"))
			return
		}
		socpes := [](func(*gorm.DB) *gorm.DB){
			db.OrderByCreatedAtAsc,
		}
		ev, err := db.GetEmbyVendors(user.ID, append(socpes, db.Paginate(page, size))...)
		if err != nil {
			if errors.Is(err, db.ErrNotFound("vendor")) {
				ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("emby server id not found"))
				return
			}
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}

		total, err := db.GetEmbyVendorsCount(user.ID, socpes...)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
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

	aucd, err := user.EmbyCache().LoadOrStore(ctx, req.ServerID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("emby server id not found"))
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
		Limit:      uint64(size),
		StartIndex: uint64((page - 1) * size),
		SearchTerm: req.Keywords,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
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
