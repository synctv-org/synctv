package utils_test

import (
	"testing"

	"github.com/synctv-org/synctv/utils"
)

func TestCrypto(t *testing.T) {
	m := []byte("hello world")
	key := []byte(utils.RandString(32))
	m, err := utils.Crypto(m, key)
	if err != nil {
		t.Fatal(err)
	}
	t.Log(string(m))
	m, err = utils.Decrypto(m, key)
	if err != nil {
		t.Fatal(err)
	}
	t.Log(string(m))
}
