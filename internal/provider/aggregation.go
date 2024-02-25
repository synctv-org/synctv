package provider

type AggregationProviderInterface interface {
	ExtractProvider(OAuth2Provider) (ProviderInterface, error)
	Provider() OAuth2Provider
	Providers() []OAuth2Provider
}

func ExtractProviders(p AggregationProviderInterface, providers ...OAuth2Provider) ([]ProviderInterface, error) {
	if len(providers) == 0 {
		providers = p.Providers()
	}
	var pi []ProviderInterface = make([]ProviderInterface, len(providers))
	for i, provider := range providers {
		pi2, err := p.ExtractProvider(provider)
		if err != nil {
			return nil, err
		}
		pi[i] = pi2
	}
	return pi, nil
}
