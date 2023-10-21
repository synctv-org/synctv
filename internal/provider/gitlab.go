package provider

import (
	"context"
	"net/http"

	"golang.org/x/oauth2"
	"golang.org/x/oauth2/gitlab"
)

type GitlabProvider struct {
	config oauth2.Config
}

func (g *GitlabProvider) Init(ClientID, ClientSecret string, options ...Oauth2Option) {
	g.config.ClientID = ClientID
	g.config.ClientSecret = ClientSecret
	g.config.Scopes = []string{"read_user"}
	g.config.Endpoint = gitlab.Endpoint
	for _, o := range options {
		o(&g.config)
	}
}

func (g *GitlabProvider) Provider() OAuth2Provider {
	return "gitlab"
}

func (g *GitlabProvider) NewConfig(options ...Oauth2Option) *oauth2.Config {
	c := g.config
	for _, o := range options {
		o(&c)
	}
	return &c
}

func (g *GitlabProvider) GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error) {
	oauth2Token, err := config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := config.Client(ctx, oauth2Token)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://gitlab.com/api/v4/user", nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	return nil, FormatErrNotImplemented("gitlab")
}

func init() {
	registerProvider(new(GitlabProvider))
}
