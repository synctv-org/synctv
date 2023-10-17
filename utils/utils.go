package utils

import (
	"math/rand"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"

	yamlcomment "github.com/zijiren233/yaml-comment"
	"gopkg.in/yaml.v3"
)

var letters = []rune("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")

func RandString(n int) string {
	b := make([]rune, n)
	for i := range b {
		b[i] = letters[rand.Intn(len(letters))]
	}
	return string(b)
}

func RandBytes(n int) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(rand.Intn(256))
	}
	return b
}

func GetPageItems[T any](items []T, max, page int64) []T {
	if max <= 0 || page <= 0 {
		return nil
	}
	start := (page - 1) * max
	l := int64(len(items))
	if start > l {
		start = l
	}
	end := page * max
	if end > l {
		end = l
	}
	return items[start:end]
}

func Index[T comparable](items []T, item T) int {
	for i, v := range items {
		if v == item {
			return i
		}
	}
	return -1
}

func In[T comparable](items []T, item T) bool {
	return Index(items, item) != -1
}

func Exists(name string) bool {
	if _, err := os.Stat(name); err != nil {
		if os.IsNotExist(err) {
			return false
		}
	}
	return true
}

func WriteYaml(file string, module any) error {
	err := os.MkdirAll(filepath.Dir(file), os.ModePerm)
	if err != nil {
		return err
	}
	f, err := os.Create(file)
	if err != nil {
		return err
	}
	defer f.Close()
	return yamlcomment.NewEncoder(yaml.NewEncoder(f)).Encode(module)
}

func ReadYaml(file string, module any) error {
	f, err := os.Open(file)
	if err != nil {
		return err
	}
	defer f.Close()
	return yaml.NewDecoder(f).Decode(module)
}

const (
	VersionEqual = iota
	VersionGreater
	VersionLess
)

func CompVersion(v1, v2 string) (int, error) {
	if v1 == v2 {
		return VersionEqual, nil
	}
	v1s, err := SplitVersion(strings.TrimLeft(v1, "v"))
	if err != nil {
		return VersionEqual, err
	}
	v2s, err := SplitVersion(strings.TrimLeft(v2, "v"))
	if err != nil {
		return VersionEqual, err
	}
	for i := 0; i < len(v1s) && i < len(v2s); i++ {
		if v1s[i] > v2s[i] {
			return VersionGreater, nil
		} else if v1s[i] < v2s[i] {
			return VersionLess, nil
		}
	}
	if len(v1s) > len(v2s) {
		return VersionGreater, nil
	} else if len(v1s) < len(v2s) {
		return VersionLess, nil
	}
	return VersionGreater, nil
}

func SplitVersion(v string) ([]int, error) {
	var vs []int
	for _, s := range strings.Split(v, ".") {
		i, err := strconv.Atoi(s)
		if err != nil {
			return nil, err
		}
		vs = append(vs, i)
	}
	return vs, nil
}

type Once struct {
	done uint32
	m    sync.Mutex
}

func (o *Once) Done() (doned bool) {
	o.m.Lock()
	defer o.m.Unlock()
	if o.done == 0 {
		o.done = 1
		return false
	}
	return true
}

func (o *Once) Do(f func()) {
	if atomic.LoadUint32(&o.done) == 0 {
		o.doSlow(f)
	}
}

func (o *Once) doSlow(f func()) {
	o.m.Lock()
	defer o.m.Unlock()
	if o.done == 0 {
		defer atomic.StoreUint32(&o.done, 1)
		f()
	}
}
