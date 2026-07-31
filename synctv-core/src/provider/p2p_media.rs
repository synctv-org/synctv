use sha2::{Digest, Sha256};

use crate::models::media::{
    AcFunPlaybackResourceKind, DouyinPlaybackResource, HuyaPlaybackResourceKind, PlaybackMedia,
    PlaybackMediaProvider, PlaybackResult, PlaybackTwitchMedia, TikTokPlaybackResource,
    TwitchPlaybackResourceKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P2pMediaDelivery {
    pub swarm_id: String,
}

/// Returns a room-scoped byte-sharing decision for one provider representation.
///
/// Providers define logical byte-equivalence. Transport URLs, proxy routes,
/// signatures, and viewer-specific headers do not define swarm membership.
#[must_use]
pub fn playback_media_p2p_delivery(
    result: &PlaybackResult,
    mode_name: &str,
    media_index: usize,
    media: &PlaybackMedia,
) -> Option<P2pMediaDelivery> {
    if result.playback_kind.is_live_edge() || is_inherently_live(&media.provider) {
        return None;
    }
    if matches!(
        result.metadata.as_ref(),
        Some(crate::models::PlaybackMetadata::DirectUrl(metadata)) if !metadata.p2p_eligible
    ) {
        return None;
    }
    let provider_resource_id = provider_resource_descriptor(result, mode_name, media_index, media)?;
    let canonical = format!(
        "synctv-p2p-media-v5\nroom:{}\nprovider:{}\ninstance:{}\nresource:{provider_resource_id}",
        result.room_id.as_i64(),
        result.provider,
        result.provider_instance_name.as_deref().unwrap_or_default(),
    );
    let digest = hex::encode(Sha256::digest(canonical.as_bytes()));
    Some(P2pMediaDelivery {
        swarm_id: format!("sm3_{digest}"),
    })
}

fn provider_resource_descriptor(
    result: &PlaybackResult,
    mode_name: &str,
    media_index: usize,
    media: &PlaybackMedia,
) -> Option<String> {
    use crate::models::media::{
        FnosProxyResource, PlaybackAcFunMedia, PlaybackAlistMedia, PlaybackBilibiliMedia,
        PlaybackCctvMedia, PlaybackDirectUrlMedia, PlaybackDouyinMedia, PlaybackEmbyMedia,
        PlaybackFnosMedia, PlaybackHuyaMedia, PlaybackNextcloudMedia, PlaybackQnapMedia,
        PlaybackSeafileMedia, PlaybackSynologyMedia, PlaybackTikTokMedia, PlaybackTrueNasMedia,
        PlaybackYoutubeMedia,
    };

    let source = playback_source_descriptor(result);
    let representation = || format!("{source}:mode:{mode_name}:media:{media_index}");
    let resolve_source = |source_mode: &str, source_index: usize| {
        result
            .playback_infos
            .get(source_mode)
            .and_then(|info| info.medias.get(source_index))
            .filter(|source_media| !std::ptr::eq(*source_media, media))
            .and_then(|source_media| {
                provider_resource_descriptor(result, source_mode, source_index, source_media)
            })
    };

    match &media.provider {
        PlaybackMediaProvider::External(_) => Some(format!("external:{}", representation())),
        PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::Direct {
                p2p_resource_id, ..
            }
            | PlaybackDirectUrlMedia::ProxyStream {
                p2p_resource_id, ..
            }
            | PlaybackDirectUrlMedia::ProxyHlsManifest {
                p2p_resource_id, ..
            }
            | PlaybackDirectUrlMedia::ProxyDashManifest {
                p2p_resource_id, ..
            },
        ) => Some(p2p_resource_id.clone()),
        PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { .. }) => {
            Some(format!("alist:{}", representation()))
        }
        PlaybackMediaProvider::Alist(
            PlaybackAlistMedia::ProxyFile {
                mode_name,
                url_index,
                ..
            }
            | PlaybackAlistMedia::ProxyTranscodedHlsManifest {
                mode_name,
                url_index,
                ..
            },
        )
        | PlaybackMediaProvider::Bilibili(
            PlaybackBilibiliMedia::ProxyMediaStream {
                mode_name,
                url_index,
                ..
            }
            | PlaybackBilibiliMedia::ProxyHlsManifest {
                mode_name,
                url_index,
                ..
            },
        )
        | PlaybackMediaProvider::Emby(
            PlaybackEmbyMedia::ProxyMediaStream {
                mode_name,
                url_index,
                ..
            }
            | PlaybackEmbyMedia::ProxyHlsManifest {
                mode_name,
                url_index,
                ..
            },
        ) => resolve_source(mode_name, *url_index),
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct { .. }) => {
            Some(format!("bilibili:{}", representation()))
        }
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DurlManifest { .. }) => {
            Some(format!("bilibili:{source}:durl"))
        }
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDashManifest {
            mode_name,
            ..
        }
        | PlaybackBilibiliMedia::ProxyDashManifest { mode_name, .. }) => {
            Some(format!("bilibili:{source}:mode:{mode_name}:dash"))
        }
        PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Refresh {
            resource,
            stream_name,
            stream_kind,
        }) => Some(format!(
            "cctv:{resource}:stream:{stream_name}:kind:{stream_kind:?}"
        )),
        PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Proxy {
            mode_name,
            media_index,
            ..
        }) => resolve_source(mode_name, *media_index)
            .or_else(|| Some(format!("proxy:{}", representation()))),
        PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { .. }) => {
            Some(format!("emby:{}", representation()))
        }
        PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Refresh {
            server_id, path, ..
        }
        | PlaybackTrueNasMedia::Proxy {
            server_id, path, ..
        }) => Some(format!("truenas:{server_id}:path:{path}")),
        PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Refresh {
            server_id,
            repository_id,
            object_id,
            ..
        }
        | PlaybackSeafileMedia::Proxy {
            server_id,
            repository_id,
            object_id,
            ..
        }) => Some(format!(
            "seafile:{server_id}:repository:{repository_id}:object:{object_id}"
        )),
        PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Refresh {
            server_id,
            file_id,
            ..
        }
        | PlaybackNextcloudMedia::Proxy {
            server_id,
            file_id,
            ..
        }) => Some(format!("nextcloud:{server_id}:file:{file_id}")),
        PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Refresh {
            server_id,
            resource,
            ..
        }
        | PlaybackSynologyMedia::Proxy {
            server_id,
            resource,
            ..
        }) => Some(format!("synology:{server_id}:{}", stable_json(resource))),
        PlaybackMediaProvider::Fnos(PlaybackFnosMedia::FileRefresh {
            server_id, path, ..
        }) => Some(format!("fnos:{server_id}:file:{path}")),
        PlaybackMediaProvider::Fnos(PlaybackFnosMedia::MediaRefresh {
            server_id,
            media_guid,
            quality_index,
            ..
        }) => Some(format!(
            "fnos:{server_id}:media:{media_guid}:quality:{quality_index:?}"
        )),
        PlaybackMediaProvider::Fnos(PlaybackFnosMedia::TranscodeRefresh {
            server_id,
            spec,
            ..
        }) => Some(format!("fnos:{server_id}:transcode:{}", stable_json(spec))),
        PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
            server_id,
            resource,
            ..
        }) => Some(match resource {
            FnosProxyResource::File { path } => format!("fnos:{server_id}:file:{path}"),
            FnosProxyResource::Media {
                media_guid,
                quality_index,
            } => format!(
                "fnos:{server_id}:media:{media_guid}:quality:{quality_index:?}"
            ),
            FnosProxyResource::Transcode { spec } => {
                format!("fnos:{server_id}:transcode:{}", stable_json(spec))
            }
        }),
        PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Refresh {
            server_id,
            resource,
            ..
        }
        | PlaybackQnapMedia::Proxy {
            server_id,
            resource,
            ..
        }) => Some(format!("qnap:{server_id}:{}", stable_json(resource))),
        PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
            resource_kind,
            resource_id,
            quality_type,
            format,
            bitrate,
            ..
        }) => Some(format!(
            "acfun:{resource_kind:?}:{resource_id}:quality:{quality_type:?}:format:{format:?}:bitrate:{bitrate:?}"
        )),
        PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Proxy {
            mode_name,
            media_index,
            ..
        })
        | PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Proxy {
            mode_name,
            media_index,
            ..
        })
        | PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Proxy {
            mode_name,
            media_index,
            ..
        })
        | PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Proxy {
            mode_name,
            media_index,
            ..
        })
        | PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Proxy {
            mode_name,
            media_index,
            ..
        })
        | PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Proxy {
            mode_name,
            media_index,
            ..
        }) => resolve_source(mode_name, *media_index)
            .or_else(|| Some(format!("proxy:{}", representation()))),
        PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh {
            resource_kind,
            resource_id,
            quality_name,
            cdn,
            format,
            bitrate,
        }) => Some(format!(
            "huya:{resource_kind:?}:{resource_id}:quality:{quality_name}:cdn:{cdn}:format:{format:?}:bitrate:{bitrate:?}"
        )),
        PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh {
            resource_kind,
            resource_id,
            quality_name,
            ..
        }) => Some(format!(
            "twitch:{resource_kind:?}:{resource_id}:quality:{quality_name}"
        )),
        PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
            video_id,
            resource,
            ..
        }) => Some(format!("youtube:{video_id}:{}", stable_json(resource))),
        PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh {
            resource,
            variant_key,
            ..
        }) => Some(format!("douyin:{}:variant:{variant_key}", stable_json(resource))),
        PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Refresh {
            resource,
            variant_key,
            ..
        }) => Some(format!("tiktok:{}:variant:{variant_key}", stable_json(resource))),
        PlaybackMediaProvider::Douyu(_)
        | PlaybackMediaProvider::Rtmp(_)
        | PlaybackMediaProvider::LiveProxy(_) => None,
    }
}

fn playback_source_descriptor(result: &PlaybackResult) -> String {
    result.id.map_or_else(
        || {
            result.target.as_ref().map_or_else(
                || format!("name:{}", result.name),
                |target| format!("target:{}", stable_json(target)),
            )
        },
        |id| format!("media:{}", id.as_i64()),
    )
}

fn stable_json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn is_inherently_live(provider: &PlaybackMediaProvider) -> bool {
    use crate::models::media::{
        PlaybackAcFunMedia, PlaybackDouyinMedia, PlaybackHuyaMedia, PlaybackTikTokMedia::Refresh,
    };

    matches!(
        provider,
        PlaybackMediaProvider::Rtmp(_)
            | PlaybackMediaProvider::LiveProxy(_)
            | PlaybackMediaProvider::Douyu(_)
            | PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
                resource_kind: AcFunPlaybackResourceKind::Live,
                ..
            })
            | PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh {
                resource_kind: HuyaPlaybackResourceKind::Live,
                ..
            })
            | PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh {
                resource: DouyinPlaybackResource::Live { .. },
                ..
            })
            | PlaybackMediaProvider::TikTok(Refresh {
                resource: TikTokPlaybackResource::Live { .. },
                ..
            })
            | PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh {
                resource_kind: TwitchPlaybackResourceKind::Channel,
                ..
            })
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::media::{
        CctvPlaybackStreamKind, DirectUrlPlaybackMetadata, FnosProxyResource, PlaybackAlistMedia,
        PlaybackBilibiliMedia, PlaybackCctvMedia, PlaybackDirectUrlMedia, PlaybackEmbyMedia,
        PlaybackExternalMedia, PlaybackFnosMedia, PlaybackInfo, PlaybackMetadata,
        PlaybackTrueNasMedia, PlaybackTwitchMedia, PlaybackYoutubeMedia, YoutubePlaybackResource,
    };
    use crate::models::{MediaId, RoomId, UserId};

    fn direct_media(resource_id: &str, url: &str, format: &str) -> PlaybackMedia {
        PlaybackMedia {
            name: "1080p".to_string(),
            format: format.to_string(),
            expire_at: None,
            metadata: None,
            provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                p2p_resource_id: resource_id.to_string(),
                url: url.to_string(),
                headers: std::collections::HashMap::new(),
            }),
        }
    }

    fn result(media: PlaybackMedia) -> PlaybackResult {
        PlaybackResult::builder(None, RoomId::new(), "movie".to_string(), 0.0)
            .id(MediaId::new())
            .provider("directUrl".to_string())
            .add_mode(
                "direct".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![media],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )
            .default_mode("direct".to_string())
            .build()
            .expect("valid playback result")
    }

    #[test]
    fn external_provider_uses_playback_representation_identity() {
        let media = PlaybackMedia {
            provider: PlaybackMediaProvider::External(PlaybackExternalMedia {
                url: "https://cdn.example/video.mp4".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            ..direct_media("direct_url:unused", "https://cdn.example/video.mp4", "mp4")
        };
        let playback = result(media.clone());

        assert!(playback_media_p2p_delivery(&playback, "direct", 0, &media).is_some());
    }

    #[test]
    fn direct_url_requires_provider_confirmed_static_bytes() {
        let media = direct_media(
            "direct_url:manifest",
            "https://cdn.example/manifest.m3u8",
            "m3u8",
        );
        let mut playback = result(media.clone());
        playback.metadata = Some(PlaybackMetadata::DirectUrl(DirectUrlPlaybackMetadata {
            format: Some("m3u8".to_string()),
            filename: Some("manifest.m3u8".to_string()),
            p2p_eligible: false,
        }));
        assert!(playback_media_p2p_delivery(&playback, "direct", 0, &media).is_none());

        playback.metadata = Some(PlaybackMetadata::DirectUrl(DirectUrlPlaybackMetadata {
            p2p_eligible: true,
            ..Default::default()
        }));
        assert!(playback_media_p2p_delivery(&playback, "direct", 0, &media).is_some());
    }

    #[test]
    fn static_provider_resources_receive_distinct_p2p_identities() {
        let resources = [
            PlaybackMedia {
                provider: PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Refresh {
                    credential_owner_id: "owner".to_string(),
                    server_id: "nas".to_string(),
                    path: "/movies/a.mp4".to_string(),
                }),
                ..direct_media("unused", "", "mp4")
            },
            PlaybackMedia {
                provider: PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
                    video_id: "video-a".to_string(),
                    resource: YoutubePlaybackResource::Format { itag: 137 },
                    credential_owner_id: UserId::new(),
                    provider_instance_name: Some("youtube-main".to_string()),
                }),
                ..direct_media("unused", "", "mp4")
            },
            PlaybackMedia {
                provider: PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh {
                    resource_kind: TwitchPlaybackResourceKind::Video,
                    resource_id: "1234".to_string(),
                    quality_name: "1080p60".to_string(),
                    credential_owner_id: UserId::new(),
                    provider_instance_name: Some("twitch-main".to_string()),
                }),
                ..direct_media("unused", "", "hls")
            },
        ];
        let playback = result(resources[0].clone());
        let identities = resources
            .iter()
            .enumerate()
            .map(|(index, media)| {
                playback_media_p2p_delivery(&playback, "direct", index, media)
                    .expect("static provider resource should be P2P eligible")
                    .swarm_id
            })
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(identities.len(), resources.len());
    }

    #[test]
    fn generic_proxy_mode_reuses_source_representation_identity() {
        let direct = PlaybackMedia {
            provider: PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct {
                url: "https://alist.example/d/movie.mp4?sign=direct".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            ..direct_media("unused", "", "mp4")
        };
        let proxy = PlaybackMedia {
            provider: PlaybackMediaProvider::Alist(PlaybackAlistMedia::ProxyFile {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "direct".to_string(),
                url_index: 0,
                url: "https://alist.example/d/movie.mp4?sign=proxy".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            ..direct.clone()
        };
        let mut playback = result(direct.clone());
        playback.playback_infos.insert(
            "proxy_direct".to_string(),
            PlaybackInfo::builder().add_media(proxy.clone()).build(),
        );

        let direct_delivery = playback_media_p2p_delivery(&playback, "direct", 0, &direct)
            .expect("direct representation should be eligible");
        let proxy_delivery = playback_media_p2p_delivery(&playback, "proxy_direct", 0, &proxy)
            .expect("proxy representation should be eligible");

        assert_eq!(direct_delivery.swarm_id, proxy_delivery.swarm_id);
    }

    #[test]
    fn cctv_vod_proxy_reuses_the_static_stream_identity() {
        let refresh = PlaybackMedia {
            provider: PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Refresh {
                resource: "5c846c0518444308ba32c4159df3b3e0".to_string(),
                stream_name: "HLS".to_string(),
                stream_kind: CctvPlaybackStreamKind::VideoHls,
            }),
            ..direct_media("unused", "", "m3u8")
        };
        let proxy = PlaybackMedia {
            provider: PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Proxy {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "direct".to_string(),
                media_index: 0,
            }),
            ..refresh.clone()
        };
        let mut playback = result(refresh.clone());
        playback.provider = "cctv".to_string();
        playback.playback_infos.insert(
            "proxy_hls_hls".to_string(),
            PlaybackInfo::builder().add_media(proxy.clone()).build(),
        );

        let refresh_delivery = playback_media_p2p_delivery(&playback, "direct", 0, &refresh)
            .expect("CCTV VOD should be P2P eligible");
        let proxy_delivery = playback_media_p2p_delivery(&playback, "proxy_hls_hls", 0, &proxy)
            .expect("proxied CCTV VOD should be P2P eligible");

        assert_eq!(refresh_delivery.swarm_id, proxy_delivery.swarm_id);
    }

    #[test]
    fn bilibili_dash_modes_share_resource_identity() {
        let direct = PlaybackMedia {
            provider: PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDashManifest {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "dash".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            ..direct_media("unused", "", "dash")
        };
        let proxy = PlaybackMedia {
            provider: PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::ProxyDashManifest {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "dash".to_string(),
            }),
            ..direct.clone()
        };
        let playback = result(direct.clone());

        let direct_delivery = playback_media_p2p_delivery(&playback, "direct_dash", 0, &direct)
            .expect("direct DASH representation should be eligible");
        let proxy_delivery = playback_media_p2p_delivery(&playback, "proxy_dash", 0, &proxy)
            .expect("proxy DASH representation should be eligible");

        assert_eq!(direct_delivery.swarm_id, proxy_delivery.swarm_id);
    }

    #[test]
    fn emby_proxy_reuses_source_representation_identity() {
        let direct = PlaybackMedia {
            provider: PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct {
                url: "https://emby.example/video.mp4".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            ..direct_media("unused", "", "mp4")
        };
        let proxy = PlaybackMedia {
            provider: PlaybackMediaProvider::Emby(PlaybackEmbyMedia::ProxyMediaStream {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "direct".to_string(),
                url_index: 0,
                url: "https://emby.example/video.mp4".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            ..direct.clone()
        };
        let mut playback = result(direct.clone());
        playback.playback_infos.insert(
            "proxy_direct".to_string(),
            PlaybackInfo::builder().add_media(proxy.clone()).build(),
        );

        let direct_delivery = playback_media_p2p_delivery(&playback, "direct", 0, &direct)
            .expect("direct representation should be eligible");
        let proxy_delivery = playback_media_p2p_delivery(&playback, "proxy_direct", 0, &proxy)
            .expect("proxy representation should be eligible");

        assert_eq!(direct_delivery.swarm_id, proxy_delivery.swarm_id);
    }

    #[test]
    fn provider_variants_and_instances_remain_isolated() {
        let format = |itag| PlaybackMedia {
            provider: PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
                video_id: "video-a".to_string(),
                resource: YoutubePlaybackResource::Format { itag },
                credential_owner_id: UserId::new(),
                provider_instance_name: Some("youtube-main".to_string()),
            }),
            ..direct_media("unused", "", "mp4")
        };
        let first = format(137);
        let second = format(399);
        let playback = result(first.clone());
        let mut other_instance = playback.clone();
        other_instance.provider_instance_name = Some("youtube-secondary".to_string());

        let first_delivery = playback_media_p2p_delivery(&playback, "direct", 0, &first)
            .expect("YouTube format should be eligible");
        let second_delivery = playback_media_p2p_delivery(&playback, "direct", 0, &second)
            .expect("YouTube format should be eligible");
        let other_instance_delivery =
            playback_media_p2p_delivery(&other_instance, "direct", 0, &first)
                .expect("YouTube provider instance should be eligible");

        assert_ne!(first_delivery.swarm_id, second_delivery.swarm_id);
        assert_ne!(first_delivery.swarm_id, other_instance_delivery.swarm_id);
    }

    #[test]
    fn proxy_only_static_provider_uses_stable_representation_identity() {
        let proxy = PlaybackMedia {
            provider: PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Proxy {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "360p".to_string(),
                media_index: 0,
            }),
            ..direct_media("unused", "", "mp4")
        };
        let mut playback = result(proxy.clone());
        playback.provider = "youtube".to_string();

        let first = playback_media_p2p_delivery(&playback, "360p", 0, &proxy)
            .expect("proxy-only static media should be eligible");
        let second = playback_media_p2p_delivery(&playback, "360p", 0, &proxy)
            .expect("proxy-only static media should remain eligible");

        assert_eq!(first.swarm_id, second.swarm_id);

        playback.id = Some(MediaId::new());
        let other_resource = playback_media_p2p_delivery(&playback, "360p", 0, &proxy)
            .expect("another static media resource should be eligible");
        assert_ne!(first.swarm_id, other_resource.swarm_id);
    }

    #[test]
    fn fnos_proxy_and_refresh_share_resource_identity() {
        let refresh = PlaybackMedia {
            provider: PlaybackMediaProvider::Fnos(PlaybackFnosMedia::FileRefresh {
                credential_owner_id: "owner".to_string(),
                server_id: "fnos".to_string(),
                path: "/movie.mp4".to_string(),
            }),
            ..direct_media("unused", "", "mp4")
        };
        let proxy = PlaybackMedia {
            provider: PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "direct".to_string(),
                media_index: 0,
                credential_owner_id: "owner".to_string(),
                server_id: "fnos".to_string(),
                resource: FnosProxyResource::File {
                    path: "/movie.mp4".to_string(),
                },
            }),
            ..refresh.clone()
        };
        let playback = result(refresh.clone());

        let refresh_delivery = playback_media_p2p_delivery(&playback, "direct", 0, &refresh)
            .expect("refresh representation should be eligible");
        let proxy_delivery = playback_media_p2p_delivery(&playback, "proxy_direct", 0, &proxy)
            .expect("proxy representation should be eligible");

        assert_eq!(refresh_delivery.swarm_id, proxy_delivery.swarm_id);
    }

    #[test]
    fn live_provider_resources_remain_excluded() {
        let twitch_live = PlaybackMedia {
            provider: PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh {
                resource_kind: TwitchPlaybackResourceKind::Channel,
                resource_id: "channel".to_string(),
                quality_name: "source".to_string(),
                credential_owner_id: UserId::new(),
                provider_instance_name: None,
            }),
            ..direct_media("unused", "", "hls")
        };
        let playback = result(twitch_live.clone());

        assert!(playback_media_p2p_delivery(&playback, "live", 0, &twitch_live).is_none());
    }

    #[test]
    fn transport_paths_share_a_resource_while_rooms_remain_isolated() {
        let first_media = direct_media(
            "direct_url:stable-resource",
            "https://a.example/video.mp4",
            "mp4",
        );
        let playback = result(first_media.clone());
        let mut other_room = playback.clone();
        other_room.room_id = RoomId::new();

        let base = playback_media_p2p_delivery(&playback, "direct", 0, &first_media)
            .expect("static HTTP media should be eligible");
        let changed_room = playback_media_p2p_delivery(&other_room, "direct", 0, &first_media)
            .expect("static HTTP media should be eligible");

        assert_ne!(base.swarm_id, changed_room.swarm_id);
    }

    #[test]
    fn direct_and_proxy_modes_use_provider_identity() {
        let direct = direct_media(
            "direct_url:stable-resource",
            "https://cdn.example/video.mp4?token=direct",
            "mp4",
        );
        let proxy = PlaybackMedia {
            provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream {
                p2p_resource_id: "direct_url:stable-resource".to_string(),
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "direct".to_string(),
                url_index: 0,
                url: "https://proxy.invalid/media".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            ..direct.clone()
        };
        let playback = result(direct.clone());

        let direct_delivery = playback_media_p2p_delivery(&playback, "direct", 0, &direct)
            .expect("direct media should be eligible");
        let proxy_delivery = playback_media_p2p_delivery(&playback, "proxy_direct", 0, &proxy)
            .expect("proxy media should be eligible");

        assert_eq!(direct_delivery.swarm_id, proxy_delivery.swarm_id);
    }

    #[test]
    fn live_playback_has_no_p2p_delivery() {
        let media = direct_media(
            "direct_url:live-resource",
            "https://live.example/index.m3u8",
            "hls",
        );
        let mut playback = result(media.clone());
        playback.playback_kind = crate::models::PlaybackKind::Live;

        assert!(playback_media_p2p_delivery(&playback, "hls", 0, &media).is_none());
    }

    #[test]
    fn manifest_formats_share_the_provider_identity_contract() {
        let hls = direct_media(
            "direct_url:hls-resource",
            "https://vod.example/master",
            "hls",
        );
        let dash = direct_media(
            "direct_url:dash-resource",
            "https://vod.example/manifest",
            "dash",
        );
        let playback = result(hls.clone());

        assert!(playback_media_p2p_delivery(&playback, "hls", 0, &hls).is_some());
        assert!(playback_media_p2p_delivery(&playback, "dash", 0, &dash).is_some());
    }
}
