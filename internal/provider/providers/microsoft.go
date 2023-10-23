package providers

import (
	"context"
	"hash/crc32"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/zijiren233/stream"
	"golang.org/x/oauth2"
	"golang.org/x/oauth2/microsoft"
)

type MicrosoftProvider struct {
	config oauth2.Config
}

func (p *MicrosoftProvider) Init(c provider.Oauth2Option) {
	p.config.Scopes = []string{"user.read"}
	p.config.Endpoint = microsoft.LiveConnectEndpoint
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *MicrosoftProvider) Provider() provider.OAuth2Provider {
	return "microsoft"
}

func (p *MicrosoftProvider) NewAuthURL(state string) string {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (p *MicrosoftProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *MicrosoftProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := p.config.Client(ctx, tk)
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
	return &provider.UserInfo{
		Username:       ui.DisplayName,
		ProviderUserID: uint(crc32.ChecksumIEEE(stream.StringToBytes(ui.ID))),
	}, nil
}

type microsoftUserInfo struct {
	ID          string `json:"id"`
	DisplayName string `json:"displayName"`
}

func init() {
	provider.RegisterProvider(new(MicrosoftProvider))
}
