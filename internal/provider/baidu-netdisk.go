package provider

import (
	"context"
	"fmt"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/sirupsen/logrus"
	"golang.org/x/oauth2"
)

// https://pan.baidu.com/union/apply
type BaiduNetDiskProvider struct {
	config oauth2.Config
}

func (p *BaiduNetDiskProvider) Init(ClientID, ClientSecret string, options ...Oauth2Option) {
	p.config.ClientID = ClientID
	p.config.ClientSecret = ClientSecret
	p.config.Scopes = []string{"basic", "netdisk"}
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  "https://openapi.baidu.com/oauth/2.0/authorize",
		TokenURL: "https://openapi.baidu.com/oauth/2.0/token",
	}
	for _, o := range options {
		o(&p.config)
	}
}

func (p *BaiduNetDiskProvider) Provider() OAuth2Provider {
	return "baidu-netdisk"
}

func (p *BaiduNetDiskProvider) NewConfig(options ...Oauth2Option) *oauth2.Config {
	c := p.config
	for _, o := range options {
		o(&c)
	}
	return &c
}

func (p *BaiduNetDiskProvider) GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error) {
	oauth2Token, err := config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	logrus.Info(oauth2Token)
	client := config.Client(ctx, oauth2Token)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://pan.baidu.com/rest/2.0/xpan/nas?method=uinfo&access_token=%s", oauth2Token.AccessToken), nil)
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
	return &UserInfo{
		Username:       ui.BaiduName,
		ProviderUserID: ui.Uk,
	}, nil
}

func init() {
	registerProvider(new(BaiduNetDiskProvider))
}

type baiduNetDiskProviderUserInfo struct {
	BaiduName string `json:"baidu_name"`
	Errmsg    string `json:"errmsg"`
	Errno     int    `json:"errno"`
	Uk        uint   `json:"uk"`
}
