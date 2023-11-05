package bilibili

import (
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"time"

	json "github.com/json-iterator/go"
	"github.com/zencoder/go-dash/v3/mpd"
)

type VideoPageInfo struct {
	Title      string       `json:"title"`
	Actors     string       `json:"actors"`
	VideoInfos []*VideoInfo `json:"videoInfos"`
}

type VideoInfo struct {
	Bvid       string `json:"bvid,omitempty"`
	Cid        uint   `json:"cid,omitempty"`
	Epid       uint   `json:"epid,omitempty"`
	Name       string `json:"name"`
	CoverImage string `json:"coverImage"`
}

type ParseVideoPageConf struct {
	GetSections bool
}

type ParseVideoPageConfig func(*ParseVideoPageConf)

func WithGetSections(GetSections bool) ParseVideoPageConfig {
	return func(c *ParseVideoPageConf) {
		c.GetSections = GetSections
	}
}

func (c *Client) ParseVideoPage(aid uint, bvid string, conf ...ParseVideoPageConfig) (*VideoPageInfo, error) {
	config := &ParseVideoPageConf{}
	for _, v := range conf {
		v(config)
	}
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
	if info.Code != 0 {
		return nil, errors.New(info.Message)
	}
	r := &VideoPageInfo{
		Title:  info.Data.Title,
		Actors: info.Data.Owner.Name,
	}

	if config.GetSections && len(info.Data.UgcSeason.Sections) != 0 {
		r.Title = info.Data.UgcSeason.Title
		for _, v := range info.Data.UgcSeason.Sections {
			for _, episode := range v.Episodes {
				r.VideoInfos = append(r.VideoInfos, &VideoInfo{
					Bvid:       episode.Bvid,
					Cid:        episode.Cid,
					Name:       episode.Title,
					CoverImage: episode.Arc.Pic,
				})
			}
		}
	} else {
		r.VideoInfos = make([]*VideoInfo, len(info.Data.Pages))
		if len(info.Data.Pages) == 1 {
			info.Data.Pages[0].Part = info.Data.Title
		}
		for i, page := range info.Data.Pages {
			r.VideoInfos[i] = &VideoInfo{
				Bvid:       info.Data.Bvid,
				Cid:        page.Cid,
				Name:       page.Part,
				CoverImage: info.Data.Pic,
			}
		}
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

func (c *GetVideoURLConf) fix() {
	if c.Quality == 0 {
		c.Quality = Q1080PP
	}
}

type GetVideoURLConfig func(*GetVideoURLConf)

func WithQuality(q uint) GetVideoURLConfig {
	return func(c *GetVideoURLConf) {
		c.Quality = q
	}
}

// https://github.com/SocialSisterYi/bilibili-API-collect/blob/master/docs/video/videostream_url.md
func (c *Client) GetVideoURL(aid uint, bvid string, cid uint, conf ...GetVideoURLConfig) (*VideoURL, error) {
	config := &GetVideoURLConf{
		Quality: Q1080PP,
	}
	for _, v := range conf {
		v(config)
	}
	config.fix()

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
	if info.Code != 0 {
		return nil, errors.New(info.Message)
	}
	return &VideoURL{
		AcceptDescription: info.Data.AcceptDescription,
		AcceptQuality:     info.Data.AcceptQuality,
		CurrentQuality:    info.Data.Quality,
		URL:               info.Data.Durl[0].URL,
	}, nil
}

type GetDashVideoURLConf struct {
	HDR            bool
	Need4K         bool
	NeedDOLBY      bool
	NeedDOLBYAudio bool
	Need8K         bool
	NeedAV1        bool
}

type GetDashVideoURLConfig func(*GetDashVideoURLConf)

func WithHDR(hdr bool) GetDashVideoURLConfig {
	return func(c *GetDashVideoURLConf) {
		c.HDR = hdr
	}
}

func WithNeed4K(need4k bool) GetDashVideoURLConfig {
	return func(c *GetDashVideoURLConf) {
		c.Need4K = need4k
	}
}

func WithNeedDOLBY(needDOLBY bool) GetDashVideoURLConfig {
	return func(c *GetDashVideoURLConf) {
		c.NeedDOLBY = needDOLBY
	}
}

func WithNeedDOLBYAudio(needDOLBYAudio bool) GetDashVideoURLConfig {
	return func(c *GetDashVideoURLConf) {
		c.NeedDOLBYAudio = needDOLBYAudio
	}
}

func WithNeed8K(need8k bool) GetDashVideoURLConfig {
	return func(c *GetDashVideoURLConf) {
		c.Need8K = need8k
	}
}

// https://github.com/SocialSisterYi/bilibili-API-collect/blob/master/docs/video/videostream_url.md
func (c *Client) GetDashVideoURL(aid uint, bvid string, cid uint, conf ...GetDashVideoURLConfig) (*mpd.MPD, error) {
	config := &GetDashVideoURLConf{}
	for _, v := range conf {
		v(config)
	}

	var (
		fnval    uint = 16
		extQuery string
	)
	if config.Need4K {
		fnval = 128
		extQuery = "&fourk=1"
	} else if config.Need8K {
		fnval = 1024
	}

	if config.HDR {
		fnval |= 64
	}
	if config.NeedDOLBY {
		fnval |= 512
	}
	if config.NeedDOLBYAudio {
		fnval |= 256
	}
	if config.NeedAV1 {
		fnval |= 2048
	}

	var url string
	if aid != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/x/player/wbi/playurl?aid=%d&cid=%d&fnver=0&platform=pc&fnval=%d%s", aid, cid, fnval, extQuery)
	} else if bvid != "" {
		url = fmt.Sprintf("https://api.bilibili.com/x/player/wbi/playurl?bvid=%s&cid=%d&fnver=0&platform=pc&fnval=%d%s", bvid, cid, fnval, extQuery)
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
	info := dashResp{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}
	if info.Code != 0 {
		return nil, errors.New(info.Message)
	}
	m := mpd.NewMPD(mpd.DASH_PROFILE_ONDEMAND, fmt.Sprintf("PT%.2fS", info.Data.Dash.Duration), fmt.Sprintf("PT%.2fS", info.Data.Dash.MinBufferTime))

	var as *mpd.AdaptationSet
	for _, v := range info.Data.Dash.Video {
		as, err = m.AddNewAdaptationSetVideo(v.MimeType, "progressive", true, v.StartWithSap)
		if err != nil {
			return nil, err
		}
		video, err := as.AddNewRepresentationVideo(v.Bandwidth, v.Codecs, fmt.Sprint(time.Now().UnixMicro()), v.FrameRate, v.Width, v.Height)
		if err != nil {
			return nil, err
		}
		video.Sar = &v.Sar
		err = video.AddNewBaseURL(v.BaseURL)
		if err != nil {
			return nil, err
		}
		_, err = video.AddNewSegmentBase(v.SegmentBase.IndexRange, v.SegmentBase.Initialization)
		if err != nil {
			return nil, err
		}
	}

	as = nil
	for _, a := range info.Data.Dash.Audio {
		as, err = m.AddNewAdaptationSetAudio(a.MimeType, true, a.StartWithSap, "und")
		if err != nil {
			return nil, err
		}
		audio, err := as.AddNewRepresentationAudio(44100, a.Bandwidth, a.Codecs, strconv.Itoa(a.ID))
		if err != nil {
			return nil, err
		}
		audio.Sar = &a.Sar
		err = audio.AddNewBaseURL(a.BaseURL)
		if err != nil {
			return nil, err
		}
		_, err = audio.AddNewSegmentBase(a.SegmentBase.IndexRange, a.SegmentBase.Initialization)
		if err != nil {
			return nil, err
		}
	}
	return m, nil
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
	if info.Code != 0 {
		return nil, errors.New(info.Message)
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

func (c *Client) ParsePGCPage(epId, season_id uint) (*VideoPageInfo, error) {
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
	if info.Code != 0 {
		return nil, errors.New(info.Message)
	}

	r := &VideoPageInfo{
		Title:      info.Result.Title,
		Actors:     info.Result.Actors,
		VideoInfos: make([]*VideoInfo, len(info.Result.Episodes)),
	}

	for i, v := range info.Result.Episodes {
		r.VideoInfos[i] = &VideoInfo{
			Epid:       v.EpID,
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
	config.fix()

	var url string
	if ep_id != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/pgc/player/web/playurl?ep_id=%d&qn=%d&fourk=1&fnval=0", ep_id, config.Quality)
	} else if cid != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/pgc/player/web/playurl?cid=%d&qn=%d&fourk=1&fnval=0", cid, config.Quality)
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
	info := pgcURLInfo{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}
	if info.Code != 0 {
		return nil, errors.New(info.Message)
	}

	return &VideoURL{
		AcceptDescription: info.Result.AcceptDescription,
		AcceptQuality:     info.Result.AcceptQuality,
		CurrentQuality:    info.Result.Quality,
		URL:               info.Result.Durl[0].URL,
	}, nil
}

func (c *Client) GetDashPGCURL(ep_id, cid uint, conf ...GetDashVideoURLConfig) (*mpd.MPD, error) {
	config := &GetDashVideoURLConf{}
	for _, v := range conf {
		v(config)
	}

	var (
		fnval    uint = 16
		extQuery string
	)
	if config.Need4K {
		fnval = 128
		extQuery = "&fourk=1"
	} else if config.Need8K {
		fnval = 1024
	}

	if config.HDR {
		fnval |= 64
	}
	if config.NeedDOLBY {
		fnval |= 512
	}
	if config.NeedDOLBYAudio {
		fnval |= 256
	}
	if config.NeedAV1 {
		fnval |= 2048
	}

	var url string
	if ep_id != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/pgc/player/web/playurl?ep_id=%d&fnval=%d%s", ep_id, fnval, extQuery)
	} else if cid != 0 {
		url = fmt.Sprintf("https://api.bilibili.com/pgc/player/web/playurl?cid=%d&fnval=%d%s", ep_id, fnval, extQuery)
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
	info := dashPGCResp{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return nil, err
	}
	if info.Code != 0 {
		return nil, errors.New(info.Message)
	}
	m := mpd.NewMPD(mpd.DASH_PROFILE_ONDEMAND, fmt.Sprintf("PT%.2fS", info.Result.Dash.Duration), fmt.Sprintf("PT%.2fS", info.Result.Dash.MinBufferTime))

	var as *mpd.AdaptationSet
	for _, v := range info.Result.Dash.Video {
		as, err = m.AddNewAdaptationSetVideo(v.MimeType, "progressive", true, v.StartWithSap)
		if err != nil {
			return nil, err
		}
		video, err := as.AddNewRepresentationVideo(v.Bandwidth, v.Codecs, fmt.Sprint(time.Now().UnixMicro()), v.FrameRate, v.Width, v.Height)
		if err != nil {
			return nil, err
		}
		video.Sar = &v.Sar
		err = video.AddNewBaseURL(v.BaseURL)
		if err != nil {
			return nil, err
		}
		_, err = video.AddNewSegmentBase(v.SegmentBase.IndexRange, v.SegmentBase.Initialization)
		if err != nil {
			return nil, err
		}
	}

	as = nil
	for _, a := range info.Result.Dash.Audio {
		as, err = m.AddNewAdaptationSetAudio(a.MimeType, true, a.StartWithSap, "und")
		if err != nil {
			return nil, err
		}
		audio, err := as.AddNewRepresentationAudio(44100, a.Bandwidth, a.Codecs, strconv.Itoa(a.ID))
		if err != nil {
			return nil, err
		}
		audio.Sar = &a.Sar
		err = audio.AddNewBaseURL(a.BaseURL)
		if err != nil {
			return nil, err
		}
		_, err = audio.AddNewSegmentBase(a.SegmentBase.IndexRange, a.SegmentBase.Initialization)
		if err != nil {
			return nil, err
		}
	}
	return m, nil
}
