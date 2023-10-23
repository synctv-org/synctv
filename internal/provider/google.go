package provider

import (
	"context"
	"net/http"

	json "github.com/json-iterator/go"
	"golang.org/x/oauth2"
	"golang.org/x/oauth2/google"
)

type GoogleProvider struct {
	config oauth2.Config
}

func (g *GoogleProvider) Init(c Oauth2Option) {
	g.config.Scopes = []string{"profile"}
	g.config.Endpoint = google.Endpoint
	g.config.ClientID = c.ClientID
	g.config.ClientSecret = c.ClientSecret
	g.config.RedirectURL = c.RedirectURL
}

func (g *GoogleProvider) Provider() OAuth2Provider {
	return "google"
}

func (g *GoogleProvider) NewAuthURL(state string) string {
	return g.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (g *GoogleProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return g.config.Exchange(ctx, code)
}

func (g *GoogleProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*UserInfo, error) {
	client := g.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://www.googleapis.com/oauth2/v2/userinfo", nil)
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
	return &UserInfo{
		Username:       ui.Name,
		ProviderUserID: ui.ID,
	}, nil
}

func init() {
	registerProvider(new(GoogleProvider))
}

type googleUserInfo struct {
	ID   uint   `json:"id,string"`
	Name string `json:"name"`
}
