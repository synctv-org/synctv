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

func (g *GoogleProvider) Init(ClientID, ClientSecret string, options ...Oauth2Option) {
	g.config.ClientID = ClientID
	g.config.ClientSecret = ClientSecret
	g.config.Scopes = []string{"profile"}
	g.config.Endpoint = google.Endpoint
	for _, o := range options {
		o(&g.config)
	}
}

func (g *GoogleProvider) Provider() OAuth2Provider {
	return "google"
}

func (g *GoogleProvider) NewConfig(options ...Oauth2Option) *oauth2.Config {
	c := g.config
	for _, o := range options {
		o(&c)
	}
	if c.RedirectURL == "" {
		panic("google oauth2 redirect url is empty")
	}
	return &c
}

func (g *GoogleProvider) GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error) {
	oauth2Token, err := config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := config.Client(ctx, oauth2Token)
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
