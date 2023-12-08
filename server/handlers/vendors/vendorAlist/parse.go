package vendorAlist

import (
	"fmt"
	"net/http"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/alist"
)

type ListReq struct {
	Path     string `json:"path"`
	Password string `json:"password"`
	Refresh  bool   `json:"refresh"`
}

func (r *ListReq) Validate() error {
	if r.Path == "" {
		r.Password = "/"
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

	v, err := db.GetVendorByUserIDAndVendor(user.ID, dbModel.StreamingVendorAlist)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	fmt.Printf("v.Authorization: %v\n", v.Authorization)

	var cli = vendor.AlistClient(ctx.Query("backend"))

	resp, err := cli.FsList(ctx, &alist.FsListReq{
		Token:    v.Authorization,
		Password: req.Password,
		Path:     req.Path,
		Host:     v.Host,
		Refresh:  req.Refresh,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}
