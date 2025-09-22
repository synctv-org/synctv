package providers

import (
	"context"
	"fmt"
	"net/http"
	"strconv"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
)

// https://pan.baidu.com/union/apply
type BaiduNetDiskProvider struct {
	config oauth2.Config
}

func newBaiduNetDiskProvider() provider.Interface {
	return &BaiduNetDiskProvider{
		config: oauth2.Config{
			Scopes: []string{"basic", "netdisk"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://openapi.baidu.com/oauth/2.0/authorize",
				TokenURL: "https://openapi.baidu.com/oauth/2.0/token",
			},
		},
	}
}

func (p *BaiduNetDiskProvider) Init(c provider.Oauth2Option) {
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *BaiduNetDiskProvider) Provider() provider.OAuth2Provider {
	return "baidu-netdisk"
}

func (p *BaiduNetDiskProvider) NewAuthURL(_ context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *BaiduNetDiskProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *BaiduNetDiskProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *BaiduNetDiskProvider) GetUserInfo(
	ctx context.Context,
	code string,
) (*provider.UserInfo, error) {
	tk, err := p.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}

	client := p.config.Client(ctx, tk)

	req, err := http.NewRequestWithContext(
		ctx,
		http.MethodGet,
		"https://pan.baidu.com/rest/2.0/xpan/nas?method=uinfo&access_token="+tk.AccessToken,
		nil,
	)
	if err != nil {
		return nil, err
	}

	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	ui := baiduNetDiskProviderUserInfo{}

	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}

	if ui.Errno != 0 {
		return nil, fmt.Errorf("baidu oauth2 get user info error: %s", ui.Errmsg)
	}

	return &provider.UserInfo{
		Username:       ui.BaiduName,
		ProviderUserID: strconv.FormatUint(ui.Uk, 10),
	}, nil
}

//nolint:tagliatelle
type baiduNetDiskProviderUserInfo struct {
	BaiduName string `json:"baidu_name"`
	Errmsg    string `json:"errmsg"`
	Errno     int    `json:"errno"`
	Uk        uint64 `json:"uk"`
}

func init() {
	RegisterProvider(newBaiduNetDiskProvider())
}
