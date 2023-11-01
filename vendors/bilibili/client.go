package bilibili

import (
	"context"
	"io"
	"net/http"

	"github.com/synctv-org/synctv/utils"
)

type Client struct {
	httpClient *http.Client
	cookies    []*http.Cookie
	buvid      []*http.Cookie
	ctx        context.Context
}

type ClientConfig func(*Client)

func WithHttpClient(httpClient *http.Client) ClientConfig {
	return func(c *Client) {
		c.httpClient = httpClient
	}
}

func WithContext(ctx context.Context) ClientConfig {
	return func(c *Client) {
		c.ctx = ctx
	}
}

func NewClient(cookies []*http.Cookie, conf ...ClientConfig) (*Client, error) {
	b, err := getBuvidCookies()
	if err != nil {
		return nil, err
	}
	cli := &Client{
		httpClient: http.DefaultClient,
		cookies:    cookies,
		ctx:        context.Background(),
		buvid:      b,
	}
	for _, v := range conf {
		v(cli)
	}
	return cli, nil
}

func (c *Client) SetCookies(cookies []*http.Cookie) {
	c.cookies = cookies
}

type RequestConfig struct {
	wbi bool
}

func defaultRequestConfig() *RequestConfig {
	return &RequestConfig{
		wbi: true,
	}
}

type RequestOption func(*RequestConfig)

func WithoutWbi() RequestOption {
	return func(c *RequestConfig) {
		c.wbi = false
	}
}

func (c *Client) NewRequest(method, url string, body io.Reader, conf ...RequestOption) (req *http.Request, err error) {
	config := defaultRequestConfig()
	for _, v := range conf {
		v(config)
	}
	if config.wbi {
		url, err = signAndGenerateURL(url)
		if err != nil {
			return nil, err
		}
	}
	req, err = http.NewRequestWithContext(c.ctx, method, url, body)
	if err != nil {
		return nil, err
	}
	for _, cookie := range c.buvid {
		req.AddCookie(cookie)
	}
	for _, cookie := range c.cookies {
		req.AddCookie(cookie)
	}
	req.Header.Set("User-Agent", utils.UA)
	req.Header.Set("Referer", "https://www.bilibili.com")
	return req, nil
}
