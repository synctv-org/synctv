package bilibili

import (
	"fmt"
	"net/http"

	json "github.com/json-iterator/go"
)

type VideoPageInfo struct {
	Bvid       string       `json:"bvid"`
	Title      string       `json:"title"`
	CoverImage string       `json:"coverImage"`
	VideoInfos []*VideoInfo `json:"videoInfos"`
}

type VideoInfo struct {
	Cid int `json:"cid"`
	// 分P
	Name       string `json:"name"`
	FirstFrame string `json:"firstFrame"`
}

func (c *Client) ParseVideoPage(aid uint, bvid string) (*VideoPageInfo, error) {
	var url string
	if aid != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/x/web-interface/view?aid=%d", aid)
	} else if bvid != "" {
		url = fmt.Sprintf("https://api.bilibili.com/x/web-interface/view?bvid=%s", bvid)
	} else {
		return nil, fmt.Errorf("aid and bvid are both empty")
	}
	req, err := c.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	info := videoPageInfo{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}
	// TODO: error message
	r := &VideoPageInfo{
		Bvid:       info.Data.Bvid,
		Title:      info.Data.Title,
		CoverImage: info.Data.Pic,
	}
	r.VideoInfos = make([]*VideoInfo, 0, len(info.Data.Pages))
	for _, page := range info.Data.Pages {
		r.VideoInfos = append(r.VideoInfos, &VideoInfo{
			Cid:        page.Cid,
			Name:       page.Part,
			FirstFrame: page.FirstFrame,
		})
	}
	return r, nil
}

const (
	Q240P    uint = 6
	Q360P    uint = 16
	Q480P    uint = 32
	Q720P    uint = 64
	Q1080P   uint = 80
	Q1080PP  uint = 112
	Q1080P60 uint = 116
	Q4K      uint = 120
	QHDR     uint = 124
	QDOLBY   uint = 126
	Q8K      uint = 127
)

type VideoURL struct {
	AcceptDescription []string `json:"acceptDescription"`
	AcceptQuality     []uint   `json:"acceptQuality"`
	CurrentQuality    uint     `json:"currentQuality"`
	URL               string   `json:"url"`
}

type GetVideoURLConf struct {
	Quality uint
}

type GetVideoURLConfig func(*GetVideoURLConf)

// https://github.com/SocialSisterYi/bilibili-API-collect/blob/master/docs/video/videostream_url.md
func (c *Client) GetVideoURL(aid uint, bvid string, cid uint, conf ...GetVideoURLConfig) (*VideoURL, error) {
	config := &GetVideoURLConf{
		Quality: Q1080PP,
	}
	for _, v := range conf {
		v(config)
	}
	var url string
	if aid != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/x/player/wbi/playurl?aid=%d&cid=%d&qn=%d&platform=html5&high_quality=1", aid, cid, config.Quality)
	} else if bvid != "" {
		url = fmt.Sprintf("https://api.bilibili.com/x/player/wbi/playurl?bvid=%s&cid=%d&qn=%d&platform=html5&high_quality=1", bvid, cid, config.Quality)
	} else {
		return nil, fmt.Errorf("aid and bvid are both empty")
	}
	req, err := c.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	info := videoInfo{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}
	return &VideoURL{
		AcceptDescription: info.Data.AcceptDescription,
		AcceptQuality:     info.Data.AcceptQuality,
		CurrentQuality:    info.Data.Quality,
		URL:               info.Data.Durl[0].URL,
	}, nil
}

type Subtitle struct {
	Name string `json:"name"`
	URL  string `json:"url"`
}

func (c *Client) GetSubtitles(aid uint, bvid string, cid uint) ([]*Subtitle, error) {
	var url string
	if aid != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/x/player/v2?aid=%d&cid=%d", aid, cid)
	} else if bvid != "" {
		url = fmt.Sprintf("https://api.bilibili.com/x/player/v2?bvid=%s&cid=%d", bvid, cid)
	} else {
		return nil, fmt.Errorf("aid and bvid are both empty")
	}
	req, err := c.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	info := playerV2Info{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}
	r := make([]*Subtitle, len(info.Data.Subtitle.Subtitles))
	for i, s := range info.Data.Subtitle.Subtitles {
		r[i] = &Subtitle{
			Name: s.LanDoc,
			URL:  s.SubtitleURL,
		}
	}
	return r, nil
}

type PGCPageInfo struct {
	Actors     string     `json:"actors"`
	CoverImage string     `json:"coverImage"`
	PGCInfos   []*PGCInfo `json:"pgcInfos"`
}

type PGCInfo struct {
	EpId       uint   `json:"epId"`
	Cid        uint   `json:"cid"`
	Name       string `json:"name"`
	CoverImage string `json:"coverImage"`
}

func (c *Client) ParsePGCPage(epId, season_id uint) (*PGCPageInfo, error) {
	var url string
	if epId != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/pgc/view/web/season?ep_id=%d", epId)
	} else if season_id != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/pgc/view/web/season?season_id=%d", season_id)
	} else {
		return nil, fmt.Errorf("edId and season_id are both empty")
	}

	req, err := c.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	info := seasonInfo{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}

	r := &PGCPageInfo{
		Actors:     info.Result.Actors,
		CoverImage: info.Result.Cover,
		PGCInfos:   make([]*PGCInfo, len(info.Result.Episodes)),
	}

	for i, v := range info.Result.Episodes {
		r.PGCInfos[i] = &PGCInfo{
			EpId:       v.EpID,
			Cid:        v.Cid,
			Name:       v.ShareCopy,
			CoverImage: v.Cover,
		}
	}

	return r, nil
}

func (c *Client) GetPGCURL(ep_id, cid uint, conf ...GetVideoURLConfig) (*VideoURL, error) {
	config := &GetVideoURLConf{
		Quality: Q1080PP,
	}
	for _, v := range conf {
		v(config)
	}
	url := fmt.Sprintf("https://api.bilibili.com/pgc/player/web/playurl?ep_id=%d&cid=%d&qn=%d&fourk=1&fnval=0", ep_id, cid, config.Quality)
	req, err := c.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	info := pgcURLInfo{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}
	return &VideoURL{
		AcceptDescription: info.Result.AcceptDescription,
		AcceptQuality:     info.Result.AcceptQuality,
		CurrentQuality:    info.Result.Quality,
		URL:               info.Result.Durl[0].URL,
	}, nil
}
