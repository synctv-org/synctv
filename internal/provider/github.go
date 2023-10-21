package provider

import (
	"context"
	"net/http"

	json "github.com/json-iterator/go"
	"golang.org/x/oauth2"
	"golang.org/x/oauth2/github"
)

type GithubProvider struct {
	ClientID, ClientSecret string
}

func (p *GithubProvider) Init(ClientID, ClientSecret string) {
	p.ClientID = ClientID
	p.ClientSecret = ClientSecret
}

func (p *GithubProvider) Provider() OAuth2Provider {
	return "github"
}

func (p *GithubProvider) NewConfig() *oauth2.Config {
	return &oauth2.Config{
		ClientID:     p.ClientID,
		ClientSecret: p.ClientSecret,
		Scopes:       []string{"user"},
		Endpoint:     github.Endpoint,
	}
}

func (p *GithubProvider) GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error) {
	oauth2Token, err := config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := config.Client(ctx, oauth2Token)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://api.github.com/user", nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := githubUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &UserInfo{
		Username:       ui.Login,
		ProviderUserID: ui.ID,
	}, nil
}

type githubUserInfo struct {
	Login string `json:"login"`
	ID    uint   `json:"id"`
}

func init() {
	registerProvider(new(GithubProvider))
}
