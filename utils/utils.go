package utils

import (
	"encoding/hex"
	"fmt"
	"math/rand"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/google/uuid"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/zijiren233/stream"
	yamlcomment "github.com/zijiren233/yaml-comment"
	"gopkg.in/yaml.v3"
)

func init() {
	uuid.EnableRandPool()
}

var (
	letters              = []rune("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")
	noRedirectHttpClient = &http.Client{
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
)

func NoRedirectHttpClient() *http.Client {
	return noRedirectHttpClient
}

const (
	UA = `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36 Edg/118.0.2088.69`
)

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

func GetPageItems[T any](items []T, page, pageSize int) []T {
	start, end := GetPageItemsRange(len(items), page, pageSize)
	return items[start:end]
}

func GetPageItemsRange(total, page, pageSize int) (start, end int) {
	if pageSize <= 0 || page <= 0 {
		return 0, 0
	}
	start = (page - 1) * pageSize
	if start > total {
		start = total
	}
	end = page * pageSize
	if end > total {
		end = total
	}
	return
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
	done := atomic.LoadUint32(&o.done)
	if done == 1 {
		return true
	} else if done == 2 {
		return false
	}

	o.m.Lock()
	defer o.m.Unlock()
	if o.done == 0 {
		doned = false
		atomic.StoreUint32(&o.done, 2)
	} else if o.done == 1 {
		doned = true
	} else {
		doned = false
	}
	return
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

func (o *Once) Reset() {
	atomic.StoreUint32(&o.done, 0)
}

func ParseURLIsLocalIP(u string) (bool, error) {
	url, err := url.Parse(u)
	if err != nil {
		return false, err
	}
	return IsLocalIP(url.Host), nil
}

func IsLocalIP(address string) bool {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		host = address
	}

	ipAddr, err := net.ResolveIPAddr("ip", host)
	if err != nil {
		return false
	}

	localIPs := getLocalIPs()

	for _, localIP := range localIPs {
		if ipAddr.IP.Equal(localIP) {
			return true
		}
	}

	return false
}

func getLocalIPs() []net.IP {
	var localIPs []net.IP

	addrs, err := net.InterfaceAddrs()
	if err != nil {
		return localIPs

	}

	for _, addr := range addrs {
		if ipNet, ok := addr.(*net.IPNet); ok && ipNet.IP.To4() != nil {
			localIPs = append(localIPs, ipNet.IP)
		}
	}

	return localIPs
}

func OptFilePath(filePath *string) {
	if filePath == nil || *filePath == "" {
		return
	}
	if !filepath.IsAbs(*filePath) {
		*filePath = filepath.Join(flags.DataDir, *filePath)
	}
}

func LIKE(s string) string {
	return fmt.Sprintf("%%%s%%", s)
}

func SortUUID() string {
	src := uuid.New()
	dst := make([]byte, 32)
	hex.Encode(dst, src[:])
	return stream.BytesToString(dst)
}

func HttpCookieToMap(c []*http.Cookie) map[string]string {
	m := make(map[string]string)
	for _, v := range c {
		m[v.Name] = v.Value
	}
	return m
}

func MapToHttpCookie(m map[string]string) []*http.Cookie {
	var c []*http.Cookie
	for k, v := range m {
		c = append(c, &http.Cookie{
			Name:  k,
			Value: v,
		})
	}
	return c
}

func GetUrlExtension(u string) string {
	if u == "" {
		return ""
	}
	p, err := url.Parse(u)
	if err != nil {
		return ""
	}
	return filepath.Ext(p.Path)
}
