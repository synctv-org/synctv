package op

import (
	"github.com/bluele/gcache"
)

func Init(size int) error {
	userCache = gcache.New(size).
		LRU().
		Build()

	return nil
}
