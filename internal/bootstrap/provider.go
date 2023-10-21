package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/provider"
)

func InitProvider(ctx context.Context) error {
	for op, v := range conf.Conf.OAuth2 {
		err := provider.InitProvider(op, v.ClientID, v.ClientSecret, provider.WithRedirectURL(v.RedirectURL))
		if err != nil {
			return err
		}
	}
	return nil
}
