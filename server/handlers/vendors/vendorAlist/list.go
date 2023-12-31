package vendorAlist

import (
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/alist"
	"gorm.io/gorm"
)

type ListReq struct {
	Path     string `json:"path"`
	Password string `json:"password"`
	Refresh  bool   `json:"refresh"`
}

func (r *ListReq) Validate() (err error) {
	return nil
}

func (r *ListReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

type AlistFileItem struct {
	*model.Item
	Size     uint64 `json:"size"`
	Modified uint64 `json:"modified"`
}

type AlistFSListResp = model.VendorFSListResp[*AlistFileItem]

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
		socpes := [](func(*gorm.DB) *gorm.DB){
			db.OrderByCreatedAtAsc,
		}

		total, err := db.GetAlistVendorsCount(user.ID, socpes...)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		if total == 0 {
			ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("alist server not found"))
			return
		}

		ev, err := db.GetAlistVendors(user.ID, append(socpes, db.Paginate(page, size))...)
		if err != nil {
			if errors.Is(err, db.ErrNotFound("vendor")) {
				ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("alist server not found"))
				return
			}
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}

		if total == 1 {
			req.Path = ev[0].ServerID + "/"
			goto AlistFSListResp
		}

		resp := AlistFSListResp{
			Paths: []*model.Path{
				{
					Name: "",
					Path: "",
				},
			},
			Total: uint64(total),
		}

		for _, evi := range ev {
			resp.Items = append(resp.Items, &AlistFileItem{
				Item: &model.Item{
					Name:  evi.Host,
					Path:  evi.ServerID + `/`,
					IsDir: true,
				},
			})
		}

		ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))

		return
	}

AlistFSListResp:

	var serverID string
	serverID, req.Path, err = dbModel.GetAlistServerIdFromPath(req.Path)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !strings.HasPrefix(req.Path, "/") {
		req.Path = "/" + req.Path
	}

	aucd, err := user.AlistCache().LoadOrStore(ctx, serverID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("alist server not found"))
			return
		}

		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
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

	req.Path = strings.Trim(req.Path, "/")
	resp := AlistFSListResp{
		Total: data.Total,
		Paths: model.GenDefaultPaths(req.Path, true,
			&model.Path{
				Name: "",
				Path: "",
			},
			&model.Path{
				Name: aucd.Host,
				Path: aucd.ServerID + "/",
			}),
	}
	for _, flr := range data.Content {
		resp.Items = append(resp.Items, &AlistFileItem{
			Item: &model.Item{
				Name:  flr.Name,
				Path:  fmt.Sprintf("%s/%s", aucd.ServerID, strings.Trim(fmt.Sprintf("%s/%s", req.Path, flr.Name), "/")),
				IsDir: flr.IsDir,
			},
			Size:     flr.Size,
			Modified: flr.Modified,
		})
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&resp))
}
