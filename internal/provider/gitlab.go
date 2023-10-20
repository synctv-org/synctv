package provider

import (
	"context"
	"net/http"

	"golang.org/x/oauth2"
	"golang.org/x/oauth2/gitlab"
)

type GitlabProvider struct {
	ClientID, ClientSecret string
}

func (g *GitlabProvider) Init(ClientID, ClientSecret string) {
	g.ClientID = ClientID
	g.ClientSecret = ClientSecret
}

func (g *GitlabProvider) Provider() OAuth2Provider {
	return "gitlab"
}

func (g *GitlabProvider) NewConfig() *oauth2.Config {
	return &oauth2.Config{
		ClientID:     g.ClientID,
		ClientSecret: g.ClientSecret,
		Scopes:       []string{"read_user"},
		Endpoint:     gitlab.Endpoint,
	}
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
