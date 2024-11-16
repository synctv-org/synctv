package provider

type AggregationProviderInterface interface {
	ExtractProvider(OAuth2Provider) (Interface, error)
	Provider() OAuth2Provider
	Providers() []OAuth2Provider
}

func ExtractProviders(p AggregationProviderInterface, providers ...OAuth2Provider) ([]Interface, error) {
	if len(providers) == 0 {
		providers = p.Providers()
	}
	pi := make([]Interface, len(providers))
	for i, provider := range providers {
		pi2, err := p.ExtractProvider(provider)
		if err != nil {
			return nil, err
		}
		pi[i] = pi2
	}
	return pi, nil
}
