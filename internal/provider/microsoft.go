package provider

import (
	"context"
	"hash/crc32"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/zijiren233/stream"
	"golang.org/x/oauth2"
	"golang.org/x/oauth2/microsoft"
)

type MicrosoftProvider struct {
	config oauth2.Config
}

func (p *MicrosoftProvider) Init(ClientID, ClientSecret string, options ...Oauth2Option) {
	p.config.ClientID = ClientID
	p.config.ClientSecret = ClientSecret
	p.config.Scopes = []string{"user.read"}
	p.config.Endpoint = microsoft.LiveConnectEndpoint
	for _, o := range options {
		o(&p.config)
	}
}

func (p *MicrosoftProvider) Provider() OAuth2Provider {
	return "microsoft"
}

func (p *MicrosoftProvider) NewConfig(options ...Oauth2Option) *oauth2.Config {
	c := p.config
	for _, o := range options {
		o(&c)
	}
	return &c
}

func (p *MicrosoftProvider) GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error) {
	oauth2Token, err := config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := config.Client(ctx, oauth2Token)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://graph.microsoft.com/v1.0/me", nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := microsoftUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &UserInfo{
		Username:       ui.DisplayName,
		ProviderUserID: uint(crc32.ChecksumIEEE(stream.StringToBytes(ui.ID))),
	}, nil
}

type microsoftUserInfo struct {
	ID          string `json:"id"`
	DisplayName string `json:"displayName"`
}

func init() {
	registerProvider(new(MicrosoftProvider))
}
