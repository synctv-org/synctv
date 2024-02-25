package aggregations

import (
	"github.com/synctv-org/synctv/internal/provider"
)

var (
	allAggregation []provider.AggregationProviderInterface
)

func addAggregation(ps ...provider.AggregationProviderInterface) {
	allAggregation = append(allAggregation, ps...)
}

func AllAggregation() []provider.AggregationProviderInterface {
	return allAggregation
}
