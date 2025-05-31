package utils

import (
	"encoding/hex"
	"errors"
	"fmt"
	"math/rand/v2"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"unicode/utf8"

	"github.com/gin-gonic/gin"
	"github.com/google/uuid"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/zijiren233/go-colorable"
	"github.com/zijiren233/stream"
	yamlcomment "github.com/zijiren233/yaml-comment"
	"gopkg.in/yaml.v3"
)

func init() {
	uuid.EnableRandPool()
}

var (
	letters              = []rune("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")
	noRedirectHTTPClient = &http.Client{
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
)

func NoRedirectHTTPClient() *http.Client {
	return noRedirectHTTPClient
}

const (
	UA = `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36 Edg/118.0.2088.69`
)

func RandString(n int) string {
	b := make([]rune, n)
	for i := range b {
		b[i] = letters[rand.IntN(len(letters))]
	}
	return string(b)
}

func RandBytes(n int) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(rand.IntN(256))
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
	_, err := os.Stat(name)
	return !os.IsNotExist(err)
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

	// Split version strings into base version and pre-release parts
	v1Parts := strings.Split(v1, "-")
	v2Parts := strings.Split(v2, "-")

	// Compare base versions
	v1Base, err := SplitVersion(strings.TrimLeft(v1Parts[0], "v"))
	if err != nil {
		return VersionEqual, err
	}
	v2Base, err := SplitVersion(strings.TrimLeft(v2Parts[0], "v"))
	if err != nil {
		return VersionEqual, err
	}

	// Base version lengths must match
	if len(v1Base) != len(v2Base) {
		return VersionEqual, fmt.Errorf("invalid version: %s, %s", v1, v2)
	}

	// Compare base version numbers
	for i := range v1Base {
		if v1Base[i] > v2Base[i] {
			return VersionGreater, nil
		}
		if v1Base[i] < v2Base[i] {
			return VersionLess, nil
		}
	}

	// If base versions are equal, compare pre-release parts
	v1PreRelease := v1Parts[1:]
	v2PreRelease := v2Parts[1:]

	// No pre-release is greater than any pre-release version
	if len(v1PreRelease) == 0 && len(v2PreRelease) != 0 {
		return VersionGreater, nil
	}
	if len(v1PreRelease) != 0 && len(v2PreRelease) == 0 {
		return VersionLess, nil
	}
	if len(v1PreRelease) == 0 && len(v2PreRelease) == 0 {
		return VersionEqual, nil
	}

	// Pre-release version precedence
	preReleaseWeight := map[string]int{
		"alpha": 0,
		"beta":  1,
		"rc":    2,
	}

	v1Type := getPreReleaseType(v1PreRelease[0])
	v2Type := getPreReleaseType(v2PreRelease[0])

	if v1Type != v2Type {
		if preReleaseWeight[v1Type] > preReleaseWeight[v2Type] {
			return VersionGreater, nil
		}
		return VersionLess, nil
	}

	// Same pre-release type, compare their versions
	if len(v1PreRelease) == 2 && len(v2PreRelease) == 2 {
		return CompVersion(v1PreRelease[1], v2PreRelease[1])
	}

	return VersionEqual, fmt.Errorf("invalid version: %s, %s", v1, v2)
}

func getPreReleaseType(s string) string {
	switch {
	case strings.HasPrefix(s, "alpha"):
		return "alpha"
	case strings.HasPrefix(s, "beta"):
		return "beta"
	case strings.HasPrefix(s, "rc"):
		return "rc"
	default:
		return ""
	}
}

func SplitVersion(v string) ([]int, error) {
	split := strings.Split(v, ".")
	vs := make([]int, 0, len(split))
	for _, s := range split {
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
	switch done {
	case 1:
		return true
	case 2:
		return false
	}

	o.m.Lock()
	defer o.m.Unlock()
	switch o.done {
	case 0:
		doned = false
		atomic.StoreUint32(&o.done, 2)
	case 1:
		doned = true
	default:
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

func OptFilePath(filePath string) (string, error) {
	if filePath == "" {
		return "", nil
	}
	if !filepath.IsAbs(filePath) {
		return filepath.Abs(filepath.Join(flags.Global.DataDir, filePath))
	}
	return filePath, nil
}

func LIKE(s string) string {
	return fmt.Sprintf("%%%s%%", s)
}

func SortUUID() string {
	return SortUUIDWithUUID(uuid.New())
}

func SortUUIDWithUUID(src uuid.UUID) string {
	dst := make([]byte, 32)
	hex.Encode(dst, src[:])
	return stream.BytesToString(dst)
}

func HTTPCookieToMap(c []*http.Cookie) map[string]string {
	m := make(map[string]string, len(c))
	for _, v := range c {
		m[v.Name] = v.Value
	}
	return m
}

func MapToHTTPCookie(m map[string]string) []*http.Cookie {
	c := make([]*http.Cookie, 0, len(m))
	for k, v := range m {
		c = append(c, &http.Cookie{
			Name:  k,
			Value: v,
		})
	}
	return c
}

func GetFileExtension(f string) string {
	return strings.TrimLeft(filepath.Ext(f), ".")
}

func GetURLExtension(u string) string {
	if u == "" {
		return ""
	}
	p, err := url.Parse(u)
	if err != nil {
		return ""
	}
	ext := GetFileExtension(p.Path)
	if ext != "" {
		return ext
	}
	return GetFileExtension(p.RawQuery)
}

func IsM3u8Url(u string) bool {
	return strings.HasPrefix(GetURLExtension(u), "m3u")
}

var (
	needColor     bool
	needColorOnce sync.Once
)

func ForceColor() bool {
	needColorOnce.Do(func() {
		if flags.Server.DisableLogColor {
			needColor = false
			return
		}
		needColor = colorable.IsTerminal(os.Stdout.Fd())
	})
	return needColor
}

func GetPageAndMax(ctx *gin.Context) (page, _max int, err error) {
	_max, err = strconv.Atoi(ctx.DefaultQuery("max", "10"))
	if err != nil {
		return 0, 0, errors.New("max must be a number")
	}
	page, err = strconv.Atoi(ctx.DefaultQuery("page", "1"))
	if err != nil {
		return 0, 0, errors.New("page must be a number")
	}
	if page <= 0 {
		page = 1
	}
	if _max <= 0 {
		_max = 10
	} else if _max > 100 {
		_max = 100
	}
	return
}

func TruncateByRune(s string, length int) string {
	total := 0
	for _, r := range s {
		runeLen := utf8.RuneLen(r)
		if runeLen == -1 || total+runeLen > length {
			return s[:total]
		}
		total += runeLen
	}
	return s[:total]
}

func GetEnvFiles(root string) ([]string, error) {
	var envs []string

	files, err := os.ReadDir(root)
	if err != nil {
		return nil, err
	}

	for _, file := range files {
		if !file.IsDir() && strings.HasPrefix(file.Name(), ".env") {
			envs = append(envs, file.Name())
		}
	}

	return envs, nil
}
