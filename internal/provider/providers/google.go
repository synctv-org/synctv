package providers

import (
	"context"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
	"golang.org/x/oauth2/google"
)

type GoogleProvider struct {
	config oauth2.Config
}

func newGoogleProvider() provider.Interface {
	return &GoogleProvider{
		config: oauth2.Config{
			Scopes:   []string{"profile"},
			Endpoint: google.Endpoint,
		},
	}
}

func (g *GoogleProvider) Init(c provider.Oauth2Option) {
	g.config.ClientID = c.ClientID
	g.config.ClientSecret = c.ClientSecret
	g.config.RedirectURL = c.RedirectURL
}

func (g *GoogleProvider) Provider() provider.OAuth2Provider {
	return "google"
}

func (g *GoogleProvider) NewAuthURL(_ context.Context, state string) (string, error) {
	return g.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (g *GoogleProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return g.config.Exchange(ctx, code)
}

func (g *GoogleProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return g.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (g *GoogleProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	tk, err := g.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}

	client := g.config.Client(ctx, tk)

	req, err := http.NewRequestWithContext(
		ctx,
		http.MethodGet,
		"https://www.googleapis.com/oauth2/v2/userinfo",
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

	ui := googleUserInfo{}

	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}

	return &provider.UserInfo{
		Username:       ui.Name,
		ProviderUserID: ui.ID,
	}, nil
}

func init() {
	RegisterProvider(newGoogleProvider())
}

type googleUserInfo struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}
