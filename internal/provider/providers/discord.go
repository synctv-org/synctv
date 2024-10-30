package providers

import (
	"context"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
)

type DiscordProvider struct {
	config oauth2.Config
}

func newDiscordProvider() provider.ProviderInterface {
	return &DiscordProvider{
		config: oauth2.Config{
			Scopes: []string{"identify"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://discord.com/oauth2/authorize",
				TokenURL: "https://discord.com/api/oauth2/token",
			},
		},
	}
}

func (p *DiscordProvider) Init(c provider.Oauth2Option) {
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *DiscordProvider) Provider() provider.OAuth2Provider {
	return "discord"
}

func (p *DiscordProvider) NewAuthURL(ctx context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *DiscordProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *DiscordProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *DiscordProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	tk, err := p.config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://discord.com/api/v10/oauth2/@me", nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := discordUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Data.Name,
		ProviderUserID: ui.Data.Id,
	}, nil
}

type discordUserInfo struct {
	Data struct {
		Id   string `json:"id"`
		Name string `json:"username"`
	} `json:"user"`
}

func init() {
	RegisterProvider(newDiscordProvider())
}
