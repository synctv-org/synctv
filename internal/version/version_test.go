package version_test

import (
	"testing"

	"github.com/synctv-org/synctv/internal/version"
)

func TestCheckLatest(t *testing.T) {
	v, err := version.NewVersionInfo()
	if err != nil {
		t.Fatal(err)
	}
	s, err := v.CheckLatest(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	t.Log(s)
}

func TestLatestBinaryURL(t *testing.T) {
	v, err := version.NewVersionInfo()
	if err != nil {
		t.Fatal(err)
	}
	s, err := v.LatestBinaryURL(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	t.Log(s)
}
