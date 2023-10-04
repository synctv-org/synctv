package utils

import (
	"math/rand"
	"os"
	"path/filepath"

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

func GetPageItems[T any](items []T, max, page int) []T {
	start := (page - 1) * max
	if start < 0 {
		start = 0
	} else if start > len(items) {
		start = len(items)
	}
	end := int(page * max)
	if end > len(items) {
		end = len(items)
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
