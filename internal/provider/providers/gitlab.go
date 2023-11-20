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

func (g *GitlabProvider) Init(c provider.Oauth2Option) {
	g.config.Scopes = []string{"read_user"}
	if c.Endpoint != nil {
		g.config.Endpoint = *c.Endpoint
	} else {
		g.config.Endpoint = gitlab.Endpoint
	}
	g.config.ClientID = c.ClientID
	g.config.ClientSecret = c.ClientSecret
	g.config.RedirectURL = c.RedirectURL
}

func (g *GitlabProvider) Provider() provider.OAuth2Provider {
	return "gitlab"
}

func (g *GitlabProvider) NewAuthURL(state string) string {
	return g.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (g *GitlabProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return g.config.Exchange(ctx, code)
}

func (g *GitlabProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return g.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (g *GitlabProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := g.config.Client(ctx, tk)
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
	RegisterProvider(new(GitlabProvider))
}
