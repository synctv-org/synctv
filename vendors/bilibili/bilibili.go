package bilibili

type qrcodeResp struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	TTL     int    `json:"ttl"`
	Data    struct {
		URL       string `json:"url"`
		QrcodeKey string `json:"qrcode_key"`
	} `json:"data"`
}

type videoPageInfo struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	TTL     int    `json:"ttl"`
	Data    struct {
		Bvid      string `json:"bvid"`
		Aid       uint   `json:"aid"`
		Videos    int    `json:"videos"`
		Tid       int    `json:"tid"`
		Tname     string `json:"tname"`
		Copyright int    `json:"copyright"`
		Pic       string `json:"pic"`
		Title     string `json:"title"`
		Pubdate   int    `json:"pubdate"`
		Ctime     int    `json:"ctime"`
		Desc      string `json:"desc"`
		DescV2    []struct {
			RawText string `json:"raw_text"`
			Type    int    `json:"type"`
			BizID   int    `json:"biz_id"`
		} `json:"desc_v2"`
		State    int `json:"state"`
		Duration int `json:"duration"`
		Rights   struct {
			Bp            int `json:"bp"`
			Elec          int `json:"elec"`
			Download      int `json:"download"`
			Movie         int `json:"movie"`
			Pay           int `json:"pay"`
			Hd5           int `json:"hd5"`
			NoReprint     int `json:"no_reprint"`
			Autoplay      int `json:"autoplay"`
			UgcPay        int `json:"ugc_pay"`
			IsCooperation int `json:"is_cooperation"`
			UgcPayPreview int `json:"ugc_pay_preview"`
			NoBackground  int `json:"no_background"`
			CleanMode     int `json:"clean_mode"`
			IsSteinGate   int `json:"is_stein_gate"`
			Is360         int `json:"is_360"`
			NoShare       int `json:"no_share"`
			ArcPay        int `json:"arc_pay"`
			FreeWatch     int `json:"free_watch"`
		} `json:"rights"`
		Owner struct {
			Mid  int    `json:"mid"`
			Name string `json:"name"`
			Face string `json:"face"`
		} `json:"owner"`
		Stat struct {
			Aid        int    `json:"aid"`
			View       int    `json:"view"`
			Danmaku    int    `json:"danmaku"`
			Reply      int    `json:"reply"`
			Favorite   int    `json:"favorite"`
			Coin       int    `json:"coin"`
			Share      int    `json:"share"`
			NowRank    int    `json:"now_rank"`
			HisRank    int    `json:"his_rank"`
			Like       int    `json:"like"`
			Dislike    int    `json:"dislike"`
			Evaluation string `json:"evaluation"`
			ArgueMsg   string `json:"argue_msg"`
			Vt         int    `json:"vt"`
		} `json:"stat"`
		Dynamic   string `json:"dynamic"`
		Cid       int    `json:"cid"`
		Dimension struct {
			Width  int `json:"width"`
			Height int `json:"height"`
			Rotate int `json:"rotate"`
		} `json:"dimension"`
		SeasonID           int         `json:"season_id"`
		Premiere           interface{} `json:"premiere"`
		TeenageMode        int         `json:"teenage_mode"`
		IsChargeableSeason bool        `json:"is_chargeable_season"`
		IsStory            bool        `json:"is_story"`
		IsUpowerExclusive  bool        `json:"is_upower_exclusive"`
		IsUpowerPlay       bool        `json:"is_upower_play"`
		EnableVt           int         `json:"enable_vt"`
		VtDisplay          string      `json:"vt_display"`
		NoCache            bool        `json:"no_cache"`
		Pages              []struct {
			Cid       uint   `json:"cid"`
			Page      int    `json:"page"`
			From      string `json:"from"`
			Part      string `json:"part"`
			Duration  int    `json:"duration"`
			Vid       string `json:"vid"`
			Weblink   string `json:"weblink"`
			Dimension struct {
				Width  int `json:"width"`
				Height int `json:"height"`
				Rotate int `json:"rotate"`
			} `json:"dimension"`
			FirstFrame string `json:"first_frame"`
		} `json:"pages"`
		Subtitle struct {
			AllowSubmit bool          `json:"allow_submit"`
			List        []interface{} `json:"list"`
		} `json:"subtitle"`
		UgcSeason struct {
			ID        int    `json:"id"`
			Title     string `json:"title"`
			Cover     string `json:"cover"`
			Mid       int    `json:"mid"`
			Intro     string `json:"intro"`
			SignState int    `json:"sign_state"`
			Attribute int    `json:"attribute"`
			Sections  []struct {
				SeasonID int    `json:"season_id"`
				ID       int    `json:"id"`
				Title    string `json:"title"`
				Type     int    `json:"type"`
				Episodes []struct {
					SeasonID  int    `json:"season_id"`
					SectionID int    `json:"section_id"`
					ID        int    `json:"id"`
					Aid       int    `json:"aid"`
					Cid       uint   `json:"cid"`
					Title     string `json:"title"`
					Attribute int    `json:"attribute"`
					Arc       struct {
						Aid       int    `json:"aid"`
						Videos    int    `json:"videos"`
						TypeID    int    `json:"type_id"`
						TypeName  string `json:"type_name"`
						Copyright int    `json:"copyright"`
						Pic       string `json:"pic"`
						Title     string `json:"title"`
						Pubdate   int    `json:"pubdate"`
						Ctime     int    `json:"ctime"`
						Desc      string `json:"desc"`
						State     int    `json:"state"`
						Duration  int    `json:"duration"`
						Rights    struct {
							Bp            int `json:"bp"`
							Elec          int `json:"elec"`
							Download      int `json:"download"`
							Movie         int `json:"movie"`
							Pay           int `json:"pay"`
							Hd5           int `json:"hd5"`
							NoReprint     int `json:"no_reprint"`
							Autoplay      int `json:"autoplay"`
							UgcPay        int `json:"ugc_pay"`
							IsCooperation int `json:"is_cooperation"`
							UgcPayPreview int `json:"ugc_pay_preview"`
							ArcPay        int `json:"arc_pay"`
							FreeWatch     int `json:"free_watch"`
						} `json:"rights"`
						Author struct {
							Mid  int    `json:"mid"`
							Name string `json:"name"`
							Face string `json:"face"`
						} `json:"author"`
						Stat struct {
							Aid        int    `json:"aid"`
							View       int    `json:"view"`
							Danmaku    int    `json:"danmaku"`
							Reply      int    `json:"reply"`
							Fav        int    `json:"fav"`
							Coin       int    `json:"coin"`
							Share      int    `json:"share"`
							NowRank    int    `json:"now_rank"`
							HisRank    int    `json:"his_rank"`
							Like       int    `json:"like"`
							Dislike    int    `json:"dislike"`
							Evaluation string `json:"evaluation"`
							ArgueMsg   string `json:"argue_msg"`
							Vt         int    `json:"vt"`
							Vv         int    `json:"vv"`
						} `json:"stat"`
						Dynamic   string `json:"dynamic"`
						Dimension struct {
							Width  int `json:"width"`
							Height int `json:"height"`
							Rotate int `json:"rotate"`
						} `json:"dimension"`
						DescV2             interface{} `json:"desc_v2"`
						IsChargeableSeason bool        `json:"is_chargeable_season"`
						IsBlooper          bool        `json:"is_blooper"`
						EnableVt           int         `json:"enable_vt"`
						VtDisplay          string      `json:"vt_display"`
					} `json:"arc"`
					Page struct {
						Cid       int    `json:"cid"`
						Page      int    `json:"page"`
						From      string `json:"from"`
						Part      string `json:"part"`
						Duration  int    `json:"duration"`
						Vid       string `json:"vid"`
						Weblink   string `json:"weblink"`
						Dimension struct {
							Width  int `json:"width"`
							Height int `json:"height"`
							Rotate int `json:"rotate"`
						} `json:"dimension"`
					} `json:"page"`
					Bvid string `json:"bvid"`
				} `json:"episodes"`
			} `json:"sections"`
			Stat struct {
				SeasonID int `json:"season_id"`
				View     int `json:"view"`
				Danmaku  int `json:"danmaku"`
				Reply    int `json:"reply"`
				Fav      int `json:"fav"`
				Coin     int `json:"coin"`
				Share    int `json:"share"`
				NowRank  int `json:"now_rank"`
				HisRank  int `json:"his_rank"`
				Like     int `json:"like"`
				Vt       int `json:"vt"`
				Vv       int `json:"vv"`
			} `json:"stat"`
			EpCount     int  `json:"ep_count"`
			SeasonType  int  `json:"season_type"`
			IsPaySeason bool `json:"is_pay_season"`
			EnableVt    int  `json:"enable_vt"`
		} `json:"ugc_season"`
		IsSeasonDisplay bool `json:"is_season_display"`
		UserGarb        struct {
			URLImageAniCut string `json:"url_image_ani_cut"`
		} `json:"user_garb"`
		HonorReply struct {
			Honor []struct {
				Aid                int    `json:"aid"`
				Type               int    `json:"type"`
				Desc               string `json:"desc"`
				WeeklyRecommendNum int    `json:"weekly_recommend_num"`
			} `json:"honor"`
		} `json:"honor_reply"`
		LikeIcon          string `json:"like_icon"`
		NeedJumpBv        bool   `json:"need_jump_bv"`
		DisableShowUpInfo bool   `json:"disable_show_up_info"`
	} `json:"data"`
}

type videoInfo struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	TTL     int    `json:"ttl"`
	Data    struct {
		From              string   `json:"from"`
		Result            string   `json:"result"`
		Message           string   `json:"message"`
		Quality           uint     `json:"quality"`
		Format            string   `json:"format"`
		Timelength        int      `json:"timelength"`
		AcceptFormat      string   `json:"accept_format"`
		AcceptDescription []string `json:"accept_description"`
		AcceptQuality     []uint   `json:"accept_quality"`
		VideoCodecid      int      `json:"video_codecid"`
		SeekParam         string   `json:"seek_param"`
		SeekType          string   `json:"seek_type"`
		Durl              []struct {
			Order     int         `json:"order"`
			Length    int         `json:"length"`
			Size      int         `json:"size"`
			Ahead     string      `json:"ahead"`
			Vhead     string      `json:"vhead"`
			URL       string      `json:"url"`
			BackupURL interface{} `json:"backup_url"`
		} `json:"durl"`
		SupportFormats []struct {
			Quality        int         `json:"quality"`
			Format         string      `json:"format"`
			NewDescription string      `json:"new_description"`
			DisplayDesc    string      `json:"display_desc"`
			Superscript    string      `json:"superscript"`
			Codecs         interface{} `json:"codecs"`
		} `json:"support_formats"`
		HighFormat   interface{} `json:"high_format"`
		LastPlayTime int         `json:"last_play_time"`
		LastPlayCid  int         `json:"last_play_cid"`
	} `json:"data"`
}

type playerV2Info struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	TTL     int    `json:"ttl"`
	Data    struct {
		Aid      int    `json:"aid"`
		Bvid     string `json:"bvid"`
		AllowBp  bool   `json:"allow_bp"`
		NoShare  bool   `json:"no_share"`
		Cid      int    `json:"cid"`
		MaxLimit int    `json:"max_limit"`
		PageNo   int    `json:"page_no"`
		HasNext  bool   `json:"has_next"`
		IPInfo   struct {
			IP       string `json:"ip"`
			ZoneIP   string `json:"zone_ip"`
			ZoneID   int    `json:"zone_id"`
			Country  string `json:"country"`
			Province string `json:"province"`
			City     string `json:"city"`
		} `json:"ip_info"`
		LoginMid     int    `json:"login_mid"`
		LoginMidHash string `json:"login_mid_hash"`
		IsOwner      bool   `json:"is_owner"`
		Name         string `json:"name"`
		Permission   string `json:"permission"`
		LevelInfo    struct {
			CurrentLevel int   `json:"current_level"`
			CurrentMin   int   `json:"current_min"`
			CurrentExp   int   `json:"current_exp"`
			NextExp      int   `json:"next_exp"`
			LevelUp      int64 `json:"level_up"`
		} `json:"level_info"`
		Vip struct {
			Type       int   `json:"type"`
			Status     int   `json:"status"`
			DueDate    int64 `json:"due_date"`
			VipPayType int   `json:"vip_pay_type"`
			ThemeType  int   `json:"theme_type"`
			Label      struct {
				Path                  string `json:"path"`
				Text                  string `json:"text"`
				LabelTheme            string `json:"label_theme"`
				TextColor             string `json:"text_color"`
				BgStyle               int    `json:"bg_style"`
				BgColor               string `json:"bg_color"`
				BorderColor           string `json:"border_color"`
				UseImgLabel           bool   `json:"use_img_label"`
				ImgLabelURIHans       string `json:"img_label_uri_hans"`
				ImgLabelURIHant       string `json:"img_label_uri_hant"`
				ImgLabelURIHansStatic string `json:"img_label_uri_hans_static"`
				ImgLabelURIHantStatic string `json:"img_label_uri_hant_static"`
			} `json:"label"`
			AvatarSubscript    int    `json:"avatar_subscript"`
			NicknameColor      string `json:"nickname_color"`
			Role               int    `json:"role"`
			AvatarSubscriptURL string `json:"avatar_subscript_url"`
			TvVipStatus        int    `json:"tv_vip_status"`
			TvVipPayType       int    `json:"tv_vip_pay_type"`
			TvDueDate          int    `json:"tv_due_date"`
		} `json:"vip"`
		AnswerStatus      int    `json:"answer_status"`
		BlockTime         int    `json:"block_time"`
		Role              string `json:"role"`
		LastPlayTime      int    `json:"last_play_time"`
		LastPlayCid       int    `json:"last_play_cid"`
		NowTime           int    `json:"now_time"`
		OnlineCount       int    `json:"online_count"`
		NeedLoginSubtitle bool   `json:"need_login_subtitle"`
		Subtitle          struct {
			AllowSubmit bool   `json:"allow_submit"`
			Lan         string `json:"lan"`
			LanDoc      string `json:"lan_doc"`
			Subtitles   []struct {
				ID          int64  `json:"id"`
				Lan         string `json:"lan"`
				LanDoc      string `json:"lan_doc"`
				IsLock      bool   `json:"is_lock"`
				SubtitleURL string `json:"subtitle_url"`
				Type        int    `json:"type"`
				IDStr       string `json:"id_str"`
				AiType      int    `json:"ai_type"`
				AiStatus    int    `json:"ai_status"`
			} `json:"subtitles"`
		} `json:"subtitle"`
		PlayerIcon struct {
			URL1  string `json:"url1"`
			Hash1 string `json:"hash1"`
			URL2  string `json:"url2"`
			Hash2 string `json:"hash2"`
			Ctime int    `json:"ctime"`
		} `json:"player_icon"`
		ViewPoints      []interface{} `json:"view_points"`
		IsUgcPayPreview bool          `json:"is_ugc_pay_preview"`
		PreviewToast    string        `json:"preview_toast"`
		Options         struct {
			Is360      bool `json:"is_360"`
			WithoutVip bool `json:"without_vip"`
		} `json:"options"`
		GuideAttention []interface{} `json:"guide_attention"`
		JumpCard       []interface{} `json:"jump_card"`
		OperationCard  []interface{} `json:"operation_card"`
		OnlineSwitch   struct {
			EnableGrayDashPlayback string `json:"enable_gray_dash_playback"`
			NewBroadcast           string `json:"new_broadcast"`
			RealtimeDm             string `json:"realtime_dm"`
			SubtitleSubmitSwitch   string `json:"subtitle_submit_switch"`
		} `json:"online_switch"`
		Fawkes struct {
			ConfigVersion int `json:"config_version"`
			FfVersion     int `json:"ff_version"`
		} `json:"fawkes"`
		ShowSwitch struct {
			LongProgress bool `json:"long_progress"`
		} `json:"show_switch"`
		BgmInfo           interface{} `json:"bgm_info"`
		ToastBlock        bool        `json:"toast_block"`
		IsUpowerExclusive bool        `json:"is_upower_exclusive"`
		IsUpowerPlay      bool        `json:"is_upower_play"`
		ElecHighLevel     struct {
			PrivilegeType int    `json:"privilege_type"`
			LevelStr      string `json:"level_str"`
			Title         string `json:"title"`
			Intro         string `json:"intro"`
		} `json:"elec_high_level"`
		DisableShowUpInfo bool `json:"disable_show_up_info"`
	} `json:"data"`
}

type seasonInfo struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Result  struct {
		Activity struct {
			HeadBgURL string `json:"head_bg_url"`
			ID        int    `json:"id"`
			Title     string `json:"title"`
		} `json:"activity"`
		Actors string `json:"actors"`
		Alias  string `json:"alias"`
		Areas  []struct {
			ID   int    `json:"id"`
			Name string `json:"name"`
		} `json:"areas"`
		BkgCover string `json:"bkg_cover"`
		Cover    string `json:"cover"`
		EnableVt bool   `json:"enable_vt"`
		Episodes []struct {
			Aid       int    `json:"aid"`
			Badge     string `json:"badge"`
			BadgeInfo struct {
				BgColor      string `json:"bg_color"`
				BgColorNight string `json:"bg_color_night"`
				Text         string `json:"text"`
			} `json:"badge_info"`
			BadgeType int    `json:"badge_type"`
			Bvid      string `json:"bvid"`
			Cid       uint   `json:"cid"`
			Cover     string `json:"cover"`
			Dimension struct {
				Height int `json:"height"`
				Rotate int `json:"rotate"`
				Width  int `json:"width"`
			} `json:"dimension"`
			Duration    int    `json:"duration"`
			EnableVt    bool   `json:"enable_vt"`
			EpID        uint   `json:"ep_id"`
			From        string `json:"from"`
			ID          int    `json:"id"`
			IsViewHide  bool   `json:"is_view_hide"`
			Link        string `json:"link"`
			LongTitle   string `json:"long_title"`
			PubTime     int    `json:"pub_time"`
			Pv          int    `json:"pv"`
			ReleaseDate string `json:"release_date"`
			Rights      struct {
				AllowDemand   int `json:"allow_demand"`
				AllowDm       int `json:"allow_dm"`
				AllowDownload int `json:"allow_download"`
				AreaLimit     int `json:"area_limit"`
			} `json:"rights"`
			ShareCopy          string `json:"share_copy"`
			ShareURL           string `json:"share_url"`
			ShortLink          string `json:"short_link"`
			ShowDrmLoginDialog bool   `json:"showDrmLoginDialog"`
			Skip               struct {
				Ed struct {
					End   int `json:"end"`
					Start int `json:"start"`
				} `json:"ed"`
				Op struct {
					End   int `json:"end"`
					Start int `json:"start"`
				} `json:"op"`
			} `json:"skip"`
			Status   int    `json:"status"`
			Subtitle string `json:"subtitle"`
			Title    string `json:"title"`
			Vid      string `json:"vid"`
		} `json:"episodes"`
		Evaluate string `json:"evaluate"`
		Freya    struct {
			BubbleDesc    string `json:"bubble_desc"`
			BubbleShowCnt int    `json:"bubble_show_cnt"`
			IconShow      int    `json:"icon_show"`
		} `json:"freya"`
		IconFont struct {
			Name string `json:"name"`
			Text string `json:"text"`
		} `json:"icon_font"`
		JpTitle string `json:"jp_title"`
		Link    string `json:"link"`
		MediaID int    `json:"media_id"`
		Mode    int    `json:"mode"`
		NewEp   struct {
			Desc  string `json:"desc"`
			ID    int    `json:"id"`
			IsNew int    `json:"is_new"`
			Title string `json:"title"`
		} `json:"new_ep"`
		Payment struct {
			Discount int `json:"discount"`
			PayType  struct {
				AllowDiscount    int `json:"allow_discount"`
				AllowPack        int `json:"allow_pack"`
				AllowTicket      int `json:"allow_ticket"`
				AllowTimeLimit   int `json:"allow_time_limit"`
				AllowVipDiscount int `json:"allow_vip_discount"`
				ForbidBb         int `json:"forbid_bb"`
			} `json:"pay_type"`
			Price             string `json:"price"`
			Promotion         string `json:"promotion"`
			Tip               string `json:"tip"`
			ViewStartTime     int    `json:"view_start_time"`
			VipDiscount       int    `json:"vip_discount"`
			VipFirstPromotion string `json:"vip_first_promotion"`
			VipPrice          string `json:"vip_price"`
			VipPromotion      string `json:"vip_promotion"`
		} `json:"payment"`
		PlayStrategy struct {
			Strategies []string `json:"strategies"`
		} `json:"play_strategy"`
		Positive struct {
			ID    int    `json:"id"`
			Title string `json:"title"`
		} `json:"positive"`
		Publish struct {
			IsFinish      int    `json:"is_finish"`
			IsStarted     int    `json:"is_started"`
			PubTime       string `json:"pub_time"`
			PubTimeShow   string `json:"pub_time_show"`
			UnknowPubDate int    `json:"unknow_pub_date"`
			Weekday       int    `json:"weekday"`
		} `json:"publish"`
		Rating struct {
			Count int     `json:"count"`
			Score float64 `json:"score"`
		} `json:"rating"`
		Record string `json:"record"`
		Rights struct {
			AllowBp         int    `json:"allow_bp"`
			AllowBpRank     int    `json:"allow_bp_rank"`
			AllowDownload   int    `json:"allow_download"`
			AllowReview     int    `json:"allow_review"`
			AreaLimit       int    `json:"area_limit"`
			BanAreaShow     int    `json:"ban_area_show"`
			CanWatch        int    `json:"can_watch"`
			Copyright       string `json:"copyright"`
			ForbidPre       int    `json:"forbid_pre"`
			FreyaWhite      int    `json:"freya_white"`
			IsCoverShow     int    `json:"is_cover_show"`
			IsPreview       int    `json:"is_preview"`
			OnlyVipDownload int    `json:"only_vip_download"`
			Resource        string `json:"resource"`
			WatchPlatform   int    `json:"watch_platform"`
		} `json:"rights"`
		SeasonID    int    `json:"season_id"`
		SeasonTitle string `json:"season_title"`
		Seasons     []struct {
			Badge     string `json:"badge"`
			BadgeInfo struct {
				BgColor      string `json:"bg_color"`
				BgColorNight string `json:"bg_color_night"`
				Text         string `json:"text"`
			} `json:"badge_info"`
			BadgeType           int    `json:"badge_type"`
			Cover               string `json:"cover"`
			EnableVt            bool   `json:"enable_vt"`
			HorizontalCover1610 string `json:"horizontal_cover_1610"`
			HorizontalCover169  string `json:"horizontal_cover_169"`
			IconFont            struct {
				Name string `json:"name"`
				Text string `json:"text"`
			} `json:"icon_font"`
			MediaID int `json:"media_id"`
			NewEp   struct {
				Cover     string `json:"cover"`
				ID        int    `json:"id"`
				IndexShow string `json:"index_show"`
			} `json:"new_ep"`
			SeasonID    int    `json:"season_id"`
			SeasonTitle string `json:"season_title"`
			SeasonType  int    `json:"season_type"`
			Stat        struct {
				Favorites    int `json:"favorites"`
				SeriesFollow int `json:"series_follow"`
				Views        int `json:"views"`
				Vt           int `json:"vt"`
			} `json:"stat"`
		} `json:"seasons"`
		Section []struct {
			Attr       int           `json:"attr"`
			EpisodeID  int           `json:"episode_id"`
			EpisodeIds []interface{} `json:"episode_ids"`
			Episodes   []struct {
				Aid       int    `json:"aid"`
				Badge     string `json:"badge"`
				BadgeInfo struct {
					BgColor      string `json:"bg_color"`
					BgColorNight string `json:"bg_color_night"`
					Text         string `json:"text"`
				} `json:"badge_info"`
				BadgeType int    `json:"badge_type"`
				Bvid      string `json:"bvid"`
				Cid       int    `json:"cid"`
				Cover     string `json:"cover"`
				Dimension struct {
					Height int `json:"height"`
					Rotate int `json:"rotate"`
					Width  int `json:"width"`
				} `json:"dimension"`
				Duration int    `json:"duration"`
				EnableVt bool   `json:"enable_vt"`
				EpID     int    `json:"ep_id"`
				From     string `json:"from"`
				IconFont struct {
					Name string `json:"name"`
					Text string `json:"text"`
				} `json:"icon_font"`
				ID          int    `json:"id"`
				IsViewHide  bool   `json:"is_view_hide"`
				Link        string `json:"link"`
				LongTitle   string `json:"long_title"`
				PubTime     int    `json:"pub_time"`
				Pv          int    `json:"pv"`
				ReleaseDate string `json:"release_date"`
				Rights      struct {
					AllowDemand   int `json:"allow_demand"`
					AllowDm       int `json:"allow_dm"`
					AllowDownload int `json:"allow_download"`
					AreaLimit     int `json:"area_limit"`
				} `json:"rights"`
				ShareCopy          string `json:"share_copy"`
				ShareURL           string `json:"share_url"`
				ShortLink          string `json:"short_link"`
				ShowDrmLoginDialog bool   `json:"showDrmLoginDialog"`
				Skip               struct {
					Ed struct {
						End   int `json:"end"`
						Start int `json:"start"`
					} `json:"ed"`
					Op struct {
						End   int `json:"end"`
						Start int `json:"start"`
					} `json:"op"`
				} `json:"skip"`
				Stat struct {
					Coin     int `json:"coin"`
					Danmakus int `json:"danmakus"`
					Likes    int `json:"likes"`
					Play     int `json:"play"`
					Reply    int `json:"reply"`
					Vt       int `json:"vt"`
				} `json:"stat"`
				StatForUnity struct {
					Coin    int `json:"coin"`
					Danmaku struct {
						Icon     string `json:"icon"`
						PureText string `json:"pure_text"`
						Text     string `json:"text"`
						Value    int    `json:"value"`
					} `json:"danmaku"`
					Likes int `json:"likes"`
					Reply int `json:"reply"`
					Vt    struct {
						Icon     string `json:"icon"`
						PureText string `json:"pure_text"`
						Text     string `json:"text"`
						Value    int    `json:"value"`
					} `json:"vt"`
				} `json:"stat_for_unity"`
				Status   int    `json:"status"`
				Subtitle string `json:"subtitle"`
				Title    string `json:"title"`
				Vid      string `json:"vid"`
			} `json:"episodes"`
			ID    int    `json:"id"`
			Title string `json:"title"`
			Type  int    `json:"type"`
			Type2 int    `json:"type2"`
		} `json:"section"`
		Series struct {
			DisplayType int    `json:"display_type"`
			SeriesID    int    `json:"series_id"`
			SeriesTitle string `json:"series_title"`
		} `json:"series"`
		ShareCopy     string `json:"share_copy"`
		ShareSubTitle string `json:"share_sub_title"`
		ShareURL      string `json:"share_url"`
		Show          struct {
			WideScreen int `json:"wide_screen"`
		} `json:"show"`
		ShowSeasonType int    `json:"show_season_type"`
		SquareCover    string `json:"square_cover"`
		Staff          string `json:"staff"`
		Stat           struct {
			Coins      int    `json:"coins"`
			Danmakus   int    `json:"danmakus"`
			Favorite   int    `json:"favorite"`
			Favorites  int    `json:"favorites"`
			FollowText string `json:"follow_text"`
			Likes      int    `json:"likes"`
			Reply      int    `json:"reply"`
			Share      int    `json:"share"`
			Views      int    `json:"views"`
			Vt         int    `json:"vt"`
		} `json:"stat"`
		Status   int      `json:"status"`
		Styles   []string `json:"styles"`
		Subtitle string   `json:"subtitle"`
		Title    string   `json:"title"`
		Total    int      `json:"total"`
		Type     int      `json:"type"`
		UpInfo   struct {
			Avatar             string `json:"avatar"`
			AvatarSubscriptURL string `json:"avatar_subscript_url"`
			Follower           int    `json:"follower"`
			IsFollow           int    `json:"is_follow"`
			Mid                int    `json:"mid"`
			NicknameColor      string `json:"nickname_color"`
			Pendant            struct {
				Image string `json:"image"`
				Name  string `json:"name"`
				Pid   int    `json:"pid"`
			} `json:"pendant"`
			ThemeType  int    `json:"theme_type"`
			Uname      string `json:"uname"`
			VerifyType int    `json:"verify_type"`
			VipLabel   struct {
				BgColor     string `json:"bg_color"`
				BgStyle     int    `json:"bg_style"`
				BorderColor string `json:"border_color"`
				Text        string `json:"text"`
				TextColor   string `json:"text_color"`
			} `json:"vip_label"`
			VipStatus int `json:"vip_status"`
			VipType   int `json:"vip_type"`
		} `json:"up_info"`
		UserStatus struct {
			AreaLimit    int `json:"area_limit"`
			BanAreaShow  int `json:"ban_area_show"`
			Follow       int `json:"follow"`
			FollowStatus int `json:"follow_status"`
			Login        int `json:"login"`
			Pay          int `json:"pay"`
			PayPackPaid  int `json:"pay_pack_paid"`
			Sponsor      int `json:"sponsor"`
		} `json:"user_status"`
	} `json:"result"`
}

type pgcURLInfo struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Result  struct {
		AcceptFormat string `json:"accept_format"`
		Code         int    `json:"code"`
		SeekParam    string `json:"seek_param"`
		IsPreview    int    `json:"is_preview"`
		Fnval        int    `json:"fnval"`
		VideoProject bool   `json:"video_project"`
		Fnver        int    `json:"fnver"`
		Type         string `json:"type"`
		Bp           int    `json:"bp"`
		Result       string `json:"result"`
		SeekType     string `json:"seek_type"`
		From         string `json:"from"`
		VideoCodecid int    `json:"video_codecid"`
		RecordInfo   struct {
			RecordIcon string `json:"record_icon"`
			Record     string `json:"record"`
		} `json:"record_info"`
		Durl []struct {
			Size      int      `json:"size"`
			Ahead     string   `json:"ahead"`
			Length    int      `json:"length"`
			Vhead     string   `json:"vhead"`
			BackupURL []string `json:"backup_url"`
			URL       string   `json:"url"`
			Order     int      `json:"order"`
			Md5       string   `json:"md5"`
		} `json:"durl"`
		IsDrm          bool   `json:"is_drm"`
		NoRexcode      int    `json:"no_rexcode"`
		Format         string `json:"format"`
		SupportFormats []struct {
			DisplayDesc    string        `json:"display_desc"`
			Superscript    string        `json:"superscript"`
			NeedLogin      bool          `json:"need_login,omitempty"`
			Codecs         []interface{} `json:"codecs"`
			Format         string        `json:"format"`
			Description    string        `json:"description"`
			Quality        int           `json:"quality"`
			NewDescription string        `json:"new_description"`
		} `json:"support_formats"`
		Message           string        `json:"message"`
		AcceptQuality     []uint        `json:"accept_quality"`
		Quality           uint          `json:"quality"`
		Timelength        int           `json:"timelength"`
		HasPaid           bool          `json:"has_paid"`
		ClipInfoList      []interface{} `json:"clip_info_list"`
		AcceptDescription []string      `json:"accept_description"`
		Status            int           `json:"status"`
	} `json:"result"`
}

type Nav struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	TTL     int    `json:"ttl"`
	Data    struct {
		IsLogin       bool   `json:"isLogin"`
		EmailVerified int    `json:"email_verified"`
		Face          string `json:"face"`
		FaceNft       int    `json:"face_nft"`
		FaceNftType   int    `json:"face_nft_type"`
		LevelInfo     struct {
			CurrentLevel int    `json:"current_level"`
			CurrentMin   int    `json:"current_min"`
			CurrentExp   int    `json:"current_exp"`
			NextExp      string `json:"next_exp"`
		} `json:"level_info"`
		Mid            int `json:"mid"`
		MobileVerified int `json:"mobile_verified"`
		Money          int `json:"money"`
		Moral          int `json:"moral"`
		Official       struct {
			Role  int    `json:"role"`
			Title string `json:"title"`
			Desc  string `json:"desc"`
			Type  int    `json:"type"`
		} `json:"official"`
		OfficialVerify struct {
			Type int    `json:"type"`
			Desc string `json:"desc"`
		} `json:"officialVerify"`
		Pendant struct {
			Pid               int    `json:"pid"`
			Name              string `json:"name"`
			Image             string `json:"image"`
			Expire            int    `json:"expire"`
			ImageEnhance      string `json:"image_enhance"`
			ImageEnhanceFrame string `json:"image_enhance_frame"`
			NPid              int    `json:"n_pid"`
		} `json:"pendant"`
		Scores       int    `json:"scores"`
		Uname        string `json:"uname"`
		VipDueDate   int64  `json:"vipDueDate"`
		VipStatus    int    `json:"vipStatus"`
		VipType      int    `json:"vipType"`
		VipPayType   int    `json:"vip_pay_type"`
		VipThemeType int    `json:"vip_theme_type"`
		VipLabel     struct {
			Path                  string `json:"path"`
			Text                  string `json:"text"`
			LabelTheme            string `json:"label_theme"`
			TextColor             string `json:"text_color"`
			BgStyle               int    `json:"bg_style"`
			BgColor               string `json:"bg_color"`
			BorderColor           string `json:"border_color"`
			UseImgLabel           bool   `json:"use_img_label"`
			ImgLabelURIHans       string `json:"img_label_uri_hans"`
			ImgLabelURIHant       string `json:"img_label_uri_hant"`
			ImgLabelURIHansStatic string `json:"img_label_uri_hans_static"`
			ImgLabelURIHantStatic string `json:"img_label_uri_hant_static"`
		} `json:"vip_label"`
		VipAvatarSubscript int    `json:"vip_avatar_subscript"`
		VipNicknameColor   string `json:"vip_nickname_color"`
		Vip                struct {
			Type       int   `json:"type"`
			Status     int   `json:"status"`
			DueDate    int64 `json:"due_date"`
			VipPayType int   `json:"vip_pay_type"`
			ThemeType  int   `json:"theme_type"`
			Label      struct {
				Path                  string `json:"path"`
				Text                  string `json:"text"`
				LabelTheme            string `json:"label_theme"`
				TextColor             string `json:"text_color"`
				BgStyle               int    `json:"bg_style"`
				BgColor               string `json:"bg_color"`
				BorderColor           string `json:"border_color"`
				UseImgLabel           bool   `json:"use_img_label"`
				ImgLabelURIHans       string `json:"img_label_uri_hans"`
				ImgLabelURIHant       string `json:"img_label_uri_hant"`
				ImgLabelURIHansStatic string `json:"img_label_uri_hans_static"`
				ImgLabelURIHantStatic string `json:"img_label_uri_hant_static"`
			} `json:"label"`
			AvatarSubscript    int    `json:"avatar_subscript"`
			NicknameColor      string `json:"nickname_color"`
			Role               int    `json:"role"`
			AvatarSubscriptURL string `json:"avatar_subscript_url"`
			TvVipStatus        int    `json:"tv_vip_status"`
			TvVipPayType       int    `json:"tv_vip_pay_type"`
			TvDueDate          int    `json:"tv_due_date"`
		} `json:"vip"`
		Wallet struct {
			Mid           int `json:"mid"`
			BcoinBalance  int `json:"bcoin_balance"`
			CouponBalance int `json:"coupon_balance"`
			CouponDueTime int `json:"coupon_due_time"`
		} `json:"wallet"`
		HasShop        bool   `json:"has_shop"`
		ShopURL        string `json:"shop_url"`
		AllowanceCount int    `json:"allowance_count"`
		AnswerStatus   int    `json:"answer_status"`
		IsSeniorMember int    `json:"is_senior_member"`
		WbiImg         struct {
			ImgURL string `json:"img_url"`
			SubURL string `json:"sub_url"`
		} `json:"wbi_img"`
		IsJury bool `json:"is_jury"`
	} `json:"data"`
}
