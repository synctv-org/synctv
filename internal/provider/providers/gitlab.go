package providers

import (
	"context"
	"net/http"

	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
	"golang.org/x/oauth2/gitlab"
)

type GitlabProvider struct {
	config oauth2.Config
}

func newGitlabProvider() provider.Interface {
	return &GitlabProvider{
		config: oauth2.Config{
			Scopes:   []string{"read_user"},
			Endpoint: gitlab.Endpoint,
		},
	}
}

func (g *GitlabProvider) Init(c provider.Oauth2Option) {
	g.config.ClientID = c.ClientID
	g.config.ClientSecret = c.ClientSecret
	g.config.RedirectURL = c.RedirectURL
}

func (g *GitlabProvider) Provider() provider.OAuth2Provider {
	return "gitlab"
}

func (g *GitlabProvider) NewAuthURL(_ context.Context, state string) (string, error) {
	return g.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (g *GitlabProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return g.config.Exchange(ctx, code)
}

func (g *GitlabProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return g.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (g *GitlabProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	tk, err := g.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}
	client := g.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(
		ctx,
		http.MethodGet,
		"https://gitlab.com/api/v4/user",
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
	return nil, FormatNotImplementedError("gitlab")
}

func init() {
	RegisterProvider(newGitlabProvider())
}
