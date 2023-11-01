package bilibili

import (
	"errors"
	"net/http"

	json "github.com/json-iterator/go"
)

func (c *Client) UserInfo() (*Nav, error) {
	req, err := c.NewRequest(http.MethodGet, "https://api.bilibili.com/x/web-interface/nav", nil, WithoutWbi())
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	var nav Nav
	err = json.NewDecoder(resp.Body).Decode(&nav)
	if err != nil {
		return nil, err
	}
	if nav.Code != 0 {
		return nil, errors.New(nav.Message)
	}
	return &nav, nil
}
