package version_test

import (
	"context"
	"testing"

	"github.com/synctv-org/synctv/internal/version"
)

func TestCheckLatest(t *testing.T) {
	v, err := version.NewVersionInfo()
	if err != nil {
		t.Fatal(err)
	}
	s, err := v.CheckLatest(context.Background())
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
	s, err := v.LatestBinaryURL(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	t.Log(s)
}
