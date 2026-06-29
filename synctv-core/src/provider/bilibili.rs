//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    provider_client::{create_remote_bilibili_client, BilibiliClientArc, ProviderClientManager},
    MediaProvider, PlaybackInfo, PlaybackResult, PreparedSourceConfig, ProviderContext,
    ProviderCredentialDependency, ProviderError, SourceConfig,
};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use crate::models::media::{
    BilibiliDashManifestSlot, BilibiliPlaybackMetadata, PlaybackBilibiliDanmaku,
    PlaybackBilibiliMedia, PlaybackBilibiliSubtitle, PlaybackDanmaku, PlaybackDanmakuProvider,
    PlaybackExternalSubtitle, PlaybackMedia, PlaybackMediaProvider, PlaybackMetadata,
    PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::models::{BilibiliMediaSourceConfig as BilibiliSourceConfig, MediaSourceConfig, UserId};
use crate::service::RemoteProviderManager;

use synctv_media_providers::grpc::bilibili as bilibili_proto;

pub const DASH_MANIFEST_METADATA_KEY: &str = "bilibili_dash_manifests";
pub const LIVE_DANMAKU_FORMAT: &str = "synctv-bilibili-live";
pub const LIVE_DANMAKU_TRACK_NAME: &str = "Bilibili Live Danmaku";

/// Bilibili `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct BilibiliProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    client_manager: Arc<ProviderClientManager>,
}

/// Bilibili video info
#[derive(Debug, Clone, Serialize)]
pub struct BilibiliVideoInfo {
    pub bvid: String,
    pub cid: u64,
    pub epid: u64,
    pub name: String,
    pub cover_image: String,
    pub r#live: bool,
}

/// Bilibili page info response
#[derive(Debug, Clone, Serialize)]
pub struct BilibiliPageInfo {
    pub title: String,
    pub actors: Vec<String>,
    pub videos: Vec<BilibiliVideoInfo>,
}

impl BilibiliProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "bilibili";

    /// Create a new `BilibiliProvider` with `RemoteProviderManager`
    pub fn new(
        provider_instance_manager: Arc<RemoteProviderManager>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager,
            client_manager: Arc::new(ProviderClientManager::new()?),
        })
    }

    #[must_use]
    pub fn with_client_manager(
        provider_instance_manager: Arc<RemoteProviderManager>,
        client_manager: Arc<ProviderClientManager>,
    ) -> Self {
        Self {
            provider_instance_manager,
            client_manager,
        }
    }

    #[cfg(test)]
    pub fn new_local_only() -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager:
                crate::service::remote_provider_manager::empty_provider_instance_manager(),
            client_manager: Arc::new(ProviderClientManager::new()?),
        })
    }

    async fn get_client_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliClientArc, ProviderError> {
        match instance_name {
            None => Ok(self.client_manager.local_bilibili_client()),
            Some(_) => {
                self.provider_instance_manager
                    .resolve_client_required_with_context(
                        instance_name,
                        request_context,
                        create_remote_bilibili_client,
                        || self.client_manager.local_bilibili_client(),
                    )
                    .await
            }
        }
    }

    /// Match URL to determine type and ID
    pub async fn r#match_with_context(
        &self,
        url: String,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::MatchResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let req = synctv_media_providers::grpc::bilibili::MatchReq { url };
        client.r#match(req).await.map_err(std::convert::Into::into)
    }

    /// Parse video page
    pub async fn parse_video_page_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseVideoPageReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .parse_video_page(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Parse PGC page
    pub async fn parse_pgc_page_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParsePgcPageReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .parse_pgc_page(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Parse live page
    pub async fn parse_live_page_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseLivePageReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .parse_live_page(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Generate QR code for login
    pub async fn new_qr_code_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewQrCodeResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .new_qr_code(synctv_media_providers::grpc::bilibili::Empty {})
            .await
            .map_err(std::convert::Into::into)
    }

    /// Check QR code login status
    pub async fn login_with_qr_code_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithQrCodeResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .login_with_qr_code(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Get new captcha
    pub async fn new_captcha_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewCaptchaResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .new_captcha(synctv_media_providers::grpc::bilibili::Empty {})
            .await
            .map_err(std::convert::Into::into)
    }

    /// Send SMS verification code
    pub async fn new_sms_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::NewSmsReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewSmsResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.new_sms(req).await.map_err(std::convert::Into::into)
    }

    /// Login with SMS code
    pub async fn login_with_sms_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithSmsReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithSmsResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .login_with_sms(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Get user info
    pub async fn user_info_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::UserInfoReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::UserInfoResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .user_info(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Get live danmaku server info for the WebSocket connection
    pub async fn get_live_danmu_info_with_context(
        &self,
        room_id: u64,
        cookies: HashMap<String, String>,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let req = synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq { cookies, room_id };
        client
            .get_live_danmu_info(req)
            .await
            .map_err(std::convert::Into::into)
    }
}

impl BilibiliSourceConfig {
    const fn shared(&self) -> bool {
        match self {
            Self::Video(config) => config.shared,
            Self::Pgc(config) => config.shared,
            Self::Live(config) => config.shared,
        }
    }

    fn from_media_config(config: &MediaSourceConfig) -> Result<&Self, ProviderError> {
        match config {
            MediaSourceConfig::Bilibili(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Bilibili requires Bilibili media source_config".to_string(),
            )),
        }
    }

    fn from_source_config(source_config: SourceConfig<'_>) -> Result<&Self, ProviderError> {
        match source_config {
            SourceConfig::Media(config) => Self::from_media_config(config),
            SourceConfig::DynamicPlaylist(_) => Err(ProviderError::InvalidConfig(
                "Bilibili supports media source_config".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BilibiliVideoIdentifier {
    bvid: Option<String>,
    aid: u64,
}

impl BilibiliVideoIdentifier {
    fn parse(bvid: Option<&str>, aid: Option<u64>) -> Result<Self, ProviderError> {
        let bvid = bvid
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let aid = match aid {
            Some(value) if value > 0 => value,
            _ => 0,
        };
        if bvid.is_none() && aid == 0 {
            return Err(ProviderError::InvalidConfig(
                "Bilibili video requires either bvid or aid".to_string(),
            ));
        }
        if let Some(bvid) = bvid.as_deref() {
            if !bvid.starts_with("BV") {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili bvid must start with 'BV'".to_string(),
                ));
            }
            if bvid.len() != 12 {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili bvid must be exactly 12 characters long".to_string(),
                ));
            }
            if !bvid.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili bvid must contain only alphanumeric characters".to_string(),
                ));
            }
        }
        Ok(Self { bvid, aid })
    }

    fn cache_key_part(&self) -> String {
        match (self.bvid.as_deref(), self.aid) {
            (Some(bvid), 0) => format!("bvid:{bvid}"),
            (None, aid) => format!("aid:{aid}"),
            (Some(bvid), aid) => format!("bvid:{bvid}:aid:{aid}"),
        }
    }
}

fn resolve_bilibili_video_identifier(
    bvid: Option<&str>,
    aid: Option<u64>,
) -> Result<(Option<String>, u64), ProviderError> {
    let identifier = BilibiliVideoIdentifier::parse(bvid, aid)?;
    Ok((identifier.bvid, identifier.aid))
}

fn non_empty_playback_urls<I>(urls: I, context: &str) -> Result<Vec<String>, ProviderError>
where
    I: IntoIterator<Item = String>,
{
    let urls = urls
        .into_iter()
        .filter(|url| !url.trim().is_empty())
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Err(ProviderError::ApiError(format!(
            "Bilibili {context} playback response did not include playable URLs"
        )));
    }
    Ok(urls)
}

fn dash_playback_urls(
    dash: &bilibili_proto::DashInfo,
    context: &str,
) -> Result<Vec<String>, ProviderError> {
    non_empty_playback_urls(
        dash.video_streams
            .iter()
            .map(|stream| stream.base_url.clone())
            .chain(
                dash.audio_streams
                    .iter()
                    .map(|stream| stream.base_url.clone()),
            ),
        context,
    )
}

fn insert_dash_manifest_metadata(
    metadata: &mut PlaybackMetadata,
    mode: BilibiliDashManifestSlot,
    dash: &bilibili_proto::DashInfo,
) {
    metadata
        .bilibili
        .get_or_insert_with(BilibiliPlaybackMetadata::default)
        .dash_manifests
        .set(mode, dash.clone());
}

fn dash_manifest_from_metadata(
    result: &PlaybackResult,
    mode_name: &str,
) -> Result<bilibili_proto::DashInfo, ProviderError> {
    let mode = BilibiliDashManifestSlot::parse(mode_name).ok_or(ProviderError::NotFound)?;
    result
        .metadata
        .bilibili
        .as_ref()
        .and_then(|metadata| metadata.dash_manifests.get(mode))
        .cloned()
        .ok_or(ProviderError::NotFound)
}

fn has_dash_manifest_metadata(result: &PlaybackResult, mode_name: &str) -> bool {
    dash_manifest_from_metadata(result, mode_name).is_ok()
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn dash_duration(value: f64) -> String {
    if value.is_finite() && value > 0.0 {
        format!("PT{value:.3}S")
    } else {
        "PT0S".to_string()
    }
}

fn frame_rate_attr(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        String::new()
    } else {
        format!(" frameRate=\"{}\"", xml_escape(value))
    }
}

fn segment_base_xml(segment_base: Option<&bilibili_proto::SegmentBase>) -> String {
    let Some(segment_base) = segment_base else {
        return String::new();
    };
    if segment_base.index_range.trim().is_empty()
        && segment_base.initialization_range.trim().is_empty()
    {
        return String::new();
    }
    let mut xml = String::new();
    let _ = write!(
        xml,
        "<SegmentBase indexRange=\"{}\">",
        xml_escape(&segment_base.index_range)
    );
    if !segment_base.initialization_range.trim().is_empty() {
        let _ = write!(
            xml,
            "<Initialization range=\"{}\"/>",
            xml_escape(&segment_base.initialization_range)
        );
    }
    xml.push_str("</SegmentBase>");
    xml
}

fn build_bilibili_mpd_manifest<F>(
    dash: &bilibili_proto::DashInfo,
    mut url_for: F,
) -> Result<String, ProviderError>
where
    F: FnMut(usize, &str) -> String,
{
    let mut url_index = 0usize;
    let mut xml = String::new();
    let media_presentation_duration = dash_duration(dash.duration);
    let min_buffer_time = dash_duration(dash.min_buffer_time);
    let _ = write!(
        xml,
        r#"<?xml version="1.0" encoding="UTF-8"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011" mediaPresentationDuration="{media_presentation_duration}" minBufferTime="{min_buffer_time}"><Period id="0" duration="{media_presentation_duration}">"#
    );

    if !dash.video_streams.is_empty() {
        xml.push_str(r#"<AdaptationSet id="1" contentType="video" segmentAlignment="true" startWithSAP="1">"#);
        for stream in &dash.video_streams {
            if stream.base_url.trim().is_empty() {
                continue;
            }
            let base_url = url_for(url_index, &stream.base_url);
            url_index += 1;
            let segment_base = segment_base_xml(stream.segment_base.as_ref());
            let _ = write!(
                xml,
                r#"<Representation id="video-{}" mimeType="{}" codecs="{}" width="{}" height="{}" bandwidth="{}" startWithSAP="{}"{}><BaseURL>{}</BaseURL>{}</Representation>"#,
                stream.id,
                xml_escape(&stream.mime_type),
                xml_escape(&stream.codecs),
                stream.width,
                stream.height,
                stream.bandwidth,
                stream.start_with_sap,
                frame_rate_attr(&stream.frame_rate),
                xml_escape(&base_url),
                segment_base,
            );
        }
        xml.push_str("</AdaptationSet>");
    }

    if !dash.audio_streams.is_empty() {
        xml.push_str(r#"<AdaptationSet id="2" contentType="audio" segmentAlignment="true" startWithSAP="1">"#);
        for stream in &dash.audio_streams {
            if stream.base_url.trim().is_empty() {
                continue;
            }
            let base_url = url_for(url_index, &stream.base_url);
            url_index += 1;
            let segment_base = segment_base_xml(stream.segment_base.as_ref());
            let sampling_rate_attr = if stream.audio_sampling_rate == 0 {
                String::new()
            } else {
                format!(r#" audioSamplingRate="{}""#, stream.audio_sampling_rate)
            };
            let _ = write!(
                xml,
                r#"<Representation id="audio-{}" mimeType="{}" codecs="{}" bandwidth="{}" startWithSAP="{}"{}><BaseURL>{}</BaseURL>{}</Representation>"#,
                stream.id,
                xml_escape(&stream.mime_type),
                xml_escape(&stream.codecs),
                stream.bandwidth,
                stream.start_with_sap,
                sampling_rate_attr,
                xml_escape(&base_url),
                segment_base,
            );
        }
        xml.push_str("</AdaptationSet>");
    }

    xml.push_str("</Period></MPD>");

    if url_index == 0 {
        return Err(ProviderError::ApiError(
            "Bilibili DASH manifest did not include playable URLs".to_string(),
        ));
    }

    Ok(xml)
}

fn mark_bilibili_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // DASH/MPD modes keep both direct and proxy manifests: app clients can
    // apply the returned Bilibili headers to the manifest and segment requests,
    // while proxy siblings remain as a server-mediated fallback.
    let original_default_mode = result.default_mode.clone();
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();

    for (mode_name, original_info) in original_modes {
        if original_info.medias.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }

        let use_mpd_manifest = original_info
            .medias
            .first()
            .is_some_and(|media| media.format == "mpd")
            && has_dash_manifest_metadata(result, &mode_name);

        if let Some(info) = result.playback_infos.get_mut(&mode_name) {
            if use_mpd_manifest {
                let source_media = original_info.medias.first();
                info.medias = vec![playback_media(
                    source_media.map_or_else(|| mode_name.clone(), |media| media.name.clone()),
                    source_media.map_or_else(|| "mpd".to_string(), |media| media.format.clone()),
                    source_media.and_then(|media| media.expire_at.map(|dt| dt.timestamp())),
                    PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDashManifest {
                        version: version.to_string(),
                        expires_at,
                        mode_name: mode_name.clone(),
                        headers: source_media
                            .map_or_else(bilibili_headers, PlaybackMedia::upstream_headers),
                    }),
                )];
            }
        }

        let proxy_mode_name = format!("proxy_{mode_name}");
        if result.playback_infos.contains_key(&proxy_mode_name) {
            continue;
        }

        let mut proxy_info = original_info.clone();
        if use_mpd_manifest {
            let source_media = original_info.medias.first();
            proxy_info.medias = vec![playback_media(
                source_media.map_or_else(|| mode_name.clone(), |media| media.name.clone()),
                source_media.map_or_else(|| "mpd".to_string(), |media| media.format.clone()),
                source_media.and_then(|media| media.expire_at.map(|dt| dt.timestamp())),
                PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::ProxyDashManifest {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                }),
            )];
        } else {
            let proxy_is_hls = super::playback_info_is_hls(&mode_name, &original_info);
            proxy_info.medias = original_info
                .medias
                .iter()
                .enumerate()
                .filter_map(|(url_index, media)| {
                    let url = media.upstream_url()?.to_string();
                    Some(playback_media(
                        media.name.clone(),
                        media.format.clone(),
                        media.expire_at.map(|dt| dt.timestamp()),
                        PlaybackMediaProvider::Bilibili(if proxy_is_hls {
                            PlaybackBilibiliMedia::ProxyHlsManifest {
                                version: version.to_string(),
                                expires_at,
                                mode_name: mode_name.clone(),
                                url_index,
                                url,
                                headers: media.upstream_headers(),
                            }
                        } else {
                            PlaybackBilibiliMedia::ProxyMediaStream {
                                version: version.to_string(),
                                expires_at,
                                mode_name: mode_name.clone(),
                                url_index,
                                url,
                                headers: media.upstream_headers(),
                            }
                        }),
                    ))
                })
                .collect();
        }
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name().to_string(),
                language: subtitle.language().to_string(),
                format: subtitle.format().to_string(),
                provider: PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    subtitle_index,
                    url: subtitle.upstream_url().to_string(),
                    headers: subtitle.upstream_headers(),
                }),
            })
            .collect();
        proxy_info.danmakus = original_info
            .danmakus
            .iter()
            .enumerate()
            .map(|(danmaku_index, danmaku)| {
                let provider = match &danmaku.provider {
                    PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
                        room_id,
                        media_id,
                    }) => PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
                        room_id: *room_id,
                        media_id: *media_id,
                    }),
                    _ => PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::File {
                        version: version.to_string(),
                        expires_at,
                        danmaku_index,
                        url: danmaku.upstream_url().unwrap_or_default().to_string(),
                        headers: danmaku.upstream_headers(),
                    }),
                };
                PlaybackDanmaku {
                    name: danmaku.name().to_string(),
                    format: danmaku.format().map(ToString::to_string),
                    provider,
                }
            })
            .collect();

        result.playback_infos.insert(proxy_mode_name, proxy_info);
    }

    let proxy_default_mode = format!("proxy_{original_default_mode}");
    result.default_mode = if result.playback_infos.contains_key(&original_default_mode) {
        original_default_mode
    } else if result.playback_infos.contains_key(&proxy_default_mode) {
        proxy_default_mode
    } else {
        original_default_mode
    };
}

fn bilibili_credential_server_id() -> String {
    crate::models::UserProviderCredential::bilibili_server_id()
}

fn is_bilibili_pgc_dash_unavailable(error: &synctv_media_providers::ProviderClientError) -> bool {
    is_bilibili_dash_unavailable_error(error, "get_dash_pgcurl")
}

fn is_bilibili_video_dash_unavailable(error: &synctv_media_providers::ProviderClientError) -> bool {
    is_bilibili_dash_unavailable_error(error, "get_dash_video_url")
}

fn is_bilibili_dash_unavailable_error(
    error: &synctv_media_providers::ProviderClientError,
    rpc_context: &str,
) -> bool {
    matches!(
        error,
        synctv_media_providers::ProviderClientError::Api { message, .. }
            if message.contains("DASH")
                || (message.contains(rpc_context) && message.contains("API error (code 0)"))
    )
}

fn playback_cache_entry(
    config: &BilibiliSourceConfig,
    credential_cache_partition: &str,
) -> Result<(String, Duration), ProviderError> {
    match config {
        BilibiliSourceConfig::Video(config) => {
            let video_key = BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?
                .cache_key_part();
            Ok((
                format!(
                    "playback:video:{video_key}:{}:{credential_cache_partition}",
                    config.cid
                ),
                Duration::from_hours(2),
            ))
        }
        BilibiliSourceConfig::Pgc(config) => Ok((
            format!(
                "playback:pgc:{}:{}:{credential_cache_partition}",
                config.epid, config.cid
            ),
            Duration::from_hours(2),
        )),
        BilibiliSourceConfig::Live(config) => Ok((
            format!(
                "playback:live:{}:{credential_cache_partition}",
                config.room_id
            ),
            Duration::from_mins(2),
        )),
    }
}

async fn resolve_optional_bilibili_cookies(
    ctx: &ProviderContext<'_>,
    credential_owner_id: UserId,
) -> Result<(HashMap<String, String>, String), ProviderError> {
    let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
        ProviderError::Internal(
            "provider_access_service not available in ProviderContext".to_string(),
        )
    })?;
    let access = access_service
        .bilibili_access(credential_owner_id, ctx.request_context())
        .await?;
    Ok((access.cookies, access.credential_cache_partition))
}

#[async_trait]
impl MediaProvider for BilibiliProvider {
    #[cfg(test)]
    fn test_client_manager_marker(&self) -> Option<usize> {
        Some(self.client_manager.marker())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = BilibiliSourceConfig::from_media_config(source_config)?;

        // Resolve cookies from DB. Shared Bilibili media uses the creator's
        // login; non-shared media uses the requesting user's own login. Missing
        // credentials are valid and fall back to anonymous playback.
        let credential_owner_id = if config.shared() {
            _ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?
        } else {
            _ctx.user_id().ok_or_else(|| {
                ProviderError::Internal("user_id not available in ProviderContext".to_string())
            })?
        };
        let (cookies, credential_cache_partition) =
            resolve_optional_bilibili_cookies(_ctx, *credential_owner_id).await?;

        let (cache_key, cache_ttl) = playback_cache_entry(config, &credential_cache_partition)?;

        Box::pin(super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            mark_bilibili_playback_resources,
            || async {
                self.resolve_from_api_with_cookies(
                    _ctx,
                    config,
                    &cookies,
                    super::bound_provider_instance_name(_ctx),
                    _ctx.request_context(),
                )
                .await
            },
        ))
        .await
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let config = BilibiliSourceConfig::from_source_config(source_config)?;

        match &config {
            BilibiliSourceConfig::Video(config) => {
                BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?;
                if config.cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili video cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Pgc(config) => {
                if config.epid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC epid must be non-zero".to_string(),
                    ));
                }
                if config.cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Live(config) => {
                if config.room_id == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili live room_id must be non-zero".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let config = BilibiliSourceConfig::from_source_config(source_config)?;
        if config.shared() {
            let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?;
            return Ok(vec![ProviderCredentialDependency::new(
                Self::NAME,
                credential_owner_id.to_string(),
                bilibili_credential_server_id(),
            )]);
        }

        let viewer_id = ctx.user_id().ok_or_else(|| {
            ProviderError::Internal("user_id not available in ProviderContext".to_string())
        })?;
        Ok(vec![ProviderCredentialDependency::optional(
            Self::NAME,
            viewer_id.to_string(),
            bilibili_credential_server_id(),
        )])
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<PreparedSourceConfig, ProviderError> {
        let _config = BilibiliSourceConfig::from_source_config(source_config)?;
        Ok(source_config.into())
    }

    fn as_bilibili_live_danmaku_provider(&self) -> Option<&dyn super::BilibiliLiveDanmakuProvider> {
        Some(self)
    }
}

fn map_bilibili_live_danmaku_event(
    event: synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEvent,
) -> super::BilibiliLiveDanmakuEvent {
    let kind = match synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::try_from(
        event.r#type,
    )
    .unwrap_or(synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::Unspecified)
    {
        synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::Unspecified => {
            super::BilibiliLiveDanmakuEventKind::Unspecified
        }
        synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::Chat => {
            super::BilibiliLiveDanmakuEventKind::Chat
        }
        synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::UserEnter => {
            super::BilibiliLiveDanmakuEventKind::UserEnter
        }
        synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::Gift => {
            super::BilibiliLiveDanmakuEventKind::Gift
        }
        synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::Heartbeat => {
            super::BilibiliLiveDanmakuEventKind::Heartbeat
        }
        synctv_media_providers::grpc::bilibili::BilibiliLiveDanmakuEventType::Unknown => {
            super::BilibiliLiveDanmakuEventKind::Unknown
        }
    };
    super::BilibiliLiveDanmakuEvent {
        format: event.format,
        event_type: event.event_type,
        kind,
        user: event.user,
        message: event.message,
        timestamp: event.timestamp,
        gift_name: event.gift_name,
        gift_count: event.gift_count,
        online_count: event.online_count,
    }
}

#[async_trait]
impl super::BilibiliLiveDanmakuProvider for BilibiliProvider {
    async fn watch_bilibili_live_danmaku(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<super::BilibiliLiveDanmakuStream, ProviderError> {
        let config = BilibiliSourceConfig::from_media_config(source_config)?;
        let BilibiliSourceConfig::Live(config) = config else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili live danmaku requires a live source config".to_string(),
            ));
        };

        let credential_owner_id = if config.shared {
            *ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?
        } else {
            *ctx.user_id().ok_or_else(|| {
                ProviderError::Internal("user_id not available in ProviderContext".to_string())
            })?
        };
        let (cookies, _) = resolve_optional_bilibili_cookies(ctx, credential_owner_id).await?;
        let client = self
            .get_client_with_context(ctx.provider_instance_name(), ctx.request_context())
            .await?;
        let stream = client
            .watch_bilibili_live_danmaku(
                synctv_media_providers::grpc::bilibili::WatchBilibiliLiveDanmakuReq {
                    cookies,
                    room_id: config.room_id,
                },
            )
            .await?;
        let stream = stream.map(|event| {
            event
                .map(map_bilibili_live_danmaku_event)
                .map_err(ProviderError::from)
        });
        Ok(Box::pin(stream))
    }
}

impl BilibiliProvider {
    pub async fn get_media_stream(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        let headers = media.upstream_headers();
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
                range_header: range_header.map(ToString::to_string),
            },
        )
    }

    pub async fn get_hls_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        let headers = media.upstream_headers();
        Ok(
            super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                url: url.to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
            },
        )
    }

    pub async fn get_hls_segment(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        target_url: String,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let headers = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .map_or_else(bilibili_headers, |info| {
                let headers = info
                    .medias
                    .first()
                    .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
                if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                }
            });
        super::playback_transport::transport_action_for_target_url(
            target_url,
            headers,
            range_header,
        )
    }

    pub async fn get_dash_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        manifest_mode: BilibiliDashManifestMode,
        request_context: Option<&super::ExecutionControl>,
        proxy_url_for: Option<&mut BilibiliDashProxyUrlMapper<'_>>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let dash = dash_manifest_from_metadata(&versioned.result, mode_name)?;
        let body = match manifest_mode {
            BilibiliDashManifestMode::Direct => {
                build_bilibili_mpd_manifest(&dash, |_index, url| url.to_string())?
            }
            BilibiliDashManifestMode::Proxy => {
                let proxy_url_for = proxy_url_for.ok_or_else(|| {
                    ProviderError::InvalidConfig(
                        "Proxy URL mapping is required for proxied DASH manifests".to_string(),
                    )
                })?;
                build_bilibili_mpd_manifest(&dash, proxy_url_for)?
            }
        };

        Ok(
            super::playback_transport::PlaybackTransportAction::DirectBody {
                body: body.into_bytes(),
                content_type: "application/dash+xml".to_string(),
                status: 200,
            },
        )
    }

    pub async fn get_dash_segment(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let dash = dash_manifest_from_metadata(&versioned.result, mode_name)?;
        let urls = dash_playback_urls(&dash, "DASH segment")?;
        let url = urls.get(url_index).ok_or(ProviderError::NotFound)?;
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.clone(),
                headers: bilibili_headers(),
                range_header: range_header.map(ToString::to_string),
            },
        )
    }

    pub async fn get_subtitle(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let subtitle = playback_info
            .subtitles
            .get(subtitle_index)
            .ok_or(ProviderError::NotFound)?;
        let media_headers = playback_info
            .medias
            .first()
            .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
        let headers = super::subtitle_headers_for_proxy(&media_headers, subtitle);
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: subtitle.upstream_url().to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
                range_header: None,
            },
        )
    }

    pub async fn get_danmaku_file(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        danmaku_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .ok_or(ProviderError::NotFound)?;
        let danmaku = playback_info
            .danmakus
            .get(danmaku_index)
            .ok_or(ProviderError::NotFound)?;
        let url = danmaku.upstream_url().ok_or(ProviderError::NotFound)?;
        let headers = danmaku.upstream_headers();
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
                range_header: None,
            },
        )
    }
}

use super::bilibili_headers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BilibiliDashManifestMode {
    Direct,
    Proxy,
}

pub type BilibiliDashProxyUrlMapper<'a> = dyn FnMut(usize, &str) -> String + Send + 'a;

fn playback_media(
    name: String,
    format: String,
    expires_at: Option<i64>,
    provider: PlaybackMediaProvider,
) -> PlaybackMedia {
    PlaybackMedia {
        name,
        format,
        expire_at: expires_at.and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        metadata: None,
        provider,
    }
}

fn bilibili_subtitle_track(name: String, url: String) -> PlaybackSubtitle {
    PlaybackSubtitle {
        language: name.clone(),
        name,
        format: "json".to_string(),
        provider: PlaybackSubtitleProvider::External(PlaybackExternalSubtitle {
            url,
            headers: bilibili_headers(),
        }),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn bilibili_direct_medias(
    name_prefix: &str,
    urls: Vec<String>,
    format: &str,
    expires_at: Option<i64>,
    headers: HashMap<String, String>,
) -> Vec<PlaybackMedia> {
    urls.into_iter()
        .enumerate()
        .map(|(index, url)| {
            let name = if index == 0 {
                name_prefix.to_string()
            } else {
                format!("{name_prefix} {}", index + 1)
            };
            playback_media(
                name,
                format.to_string(),
                expires_at,
                PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct {
                    url,
                    headers: headers.clone(),
                }),
            )
        })
        .collect()
}

fn bilibili_live_headers() -> HashMap<String, String> {
    let mut headers = bilibili_headers();
    headers.insert(
        "Referer".to_string(),
        "https://live.bilibili.com".to_string(),
    );
    headers
}

fn bilibili_live_danmaku_track(ctx: &ProviderContext<'_>) -> Option<PlaybackDanmaku> {
    let room_id = ctx.room_id()?;
    let media_id = ctx.media_id()?;
    Some(PlaybackDanmaku {
        name: LIVE_DANMAKU_TRACK_NAME.to_string(),
        format: Some(LIVE_DANMAKU_FORMAT.to_string()),
        provider: PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
            room_id: *room_id,
            media_id: *media_id,
        }),
    })
}

impl BilibiliProvider {
    /// Resolve playback result from Bilibili API (no caching).
    /// Cookies are resolved from the credential store, not from source_config.
    async fn resolve_from_api_with_cookies(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliSourceConfig,
        cookies: &HashMap<String, String>,
        provider_instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackResult, ProviderError> {
        let sanitized_cookies = cookies.clone();
        let client = self
            .get_client_with_context(provider_instance_name, request_context)
            .await?;

        let mode_info = |medias: Vec<PlaybackMedia>,
                         subtitles: Vec<PlaybackSubtitle>,
                         danmakus: Vec<PlaybackDanmaku>| PlaybackInfo {
            medias,
            default_media_index: None,
            subtitles,
            default_subtitle_index: None,
            danmakus,
            default_danmaku_index: None,
        };

        match config {
            BilibiliSourceConfig::Video(config) => {
                let (bvid, aid) =
                    resolve_bilibili_video_identifier(config.bvid.as_deref(), config.aid)?;
                let request_bvid = bvid.clone().unwrap_or_default();
                let cid = config.cid;

                let request = synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq {
                    aid,
                    bvid: request_bvid.clone(),
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                let dash_resp = match client.get_dash_video_url(request).await {
                    Ok(dash_resp) if dash_resp.dash.is_some() => Some(dash_resp),
                    Ok(_) => None,
                    Err(error) if is_bilibili_video_dash_unavailable(&error) => None,
                    Err(error) => return Err(error.into()),
                };

                let mut metadata = PlaybackMetadata {
                    content_type: Some("video".to_string()),
                    bilibili: Some(BilibiliPlaybackMetadata {
                        bvid,
                        aid: Some(aid),
                        cid: Some(cid),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let mut subtitles = Vec::new();

                let subtitle_request = synctv_media_providers::grpc::bilibili::GetSubtitlesReq {
                    aid,
                    bvid: request_bvid.clone(),
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                match client.get_subtitles(subtitle_request).await {
                    Ok(subtitle_resp) => {
                        subtitles = subtitle_resp
                            .subtitles
                            .into_iter()
                            .map(|(name, url)| bilibili_subtitle_track(name, url))
                            .collect();
                    }
                    Err(e) => {
                        tracing::warn!(
                            bvid = %request_bvid, aid = %aid, cid = %cid, error = %e,
                            "Failed to fetch Bilibili subtitles for video, continuing without subtitles"
                        );
                    }
                }

                let duration_seconds = dash_resp
                    .as_ref()
                    .and_then(|resp| resp.dash.as_ref())
                    .map(|dash| dash.duration);
                if let Some(d) = dash_resp.as_ref().and_then(|resp| resp.dash.as_ref()) {
                    metadata.duration = Some(d.duration);
                    metadata
                        .bilibili
                        .get_or_insert_with(BilibiliPlaybackMetadata::default)
                        .min_buffer_time = Some(d.min_buffer_time);
                }

                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);
                let mut playback_infos = HashMap::new();

                let default_mode = if let Some(dash_resp) = dash_resp {
                    let dash = dash_resp.dash.as_ref().ok_or_else(|| {
                        ProviderError::ApiError(
                            "Bilibili video playback response did not include DASH streams"
                                .to_string(),
                        )
                    })?;
                    insert_dash_manifest_metadata(
                        &mut metadata,
                        BilibiliDashManifestSlot::Dash,
                        dash,
                    );
                    let dash_urls = dash_playback_urls(dash, "video DASH")?;
                    let hevc_urls = dash_resp
                        .hevc_dash
                        .as_ref()
                        .filter(|dash| !dash.video_streams.is_empty())
                        .map(|dash| {
                            insert_dash_manifest_metadata(
                                &mut metadata,
                                BilibiliDashManifestSlot::Hevc,
                                dash,
                            );
                            dash_playback_urls(dash, "video HEVC DASH")
                        })
                        .transpose()?;

                    playback_infos.insert(
                        "dash".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "DASH",
                                dash_urls,
                                "mpd",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles.clone(),
                            Vec::new(),
                        ),
                    );
                    if let Some(hevc_urls) = hevc_urls {
                        playback_infos.insert(
                            "hevc".to_string(),
                            mode_info(
                                bilibili_direct_medias(
                                    "HEVC",
                                    hevc_urls,
                                    "mpd",
                                    expires_at,
                                    bilibili_headers(),
                                ),
                                subtitles,
                                Vec::new(),
                            ),
                        );
                    }
                    "dash".to_string()
                } else {
                    let request = synctv_media_providers::grpc::bilibili::GetVideoUrlReq {
                        aid,
                        bvid: request_bvid,
                        cid,
                        quality: 80,
                        cookies: sanitized_cookies.clone(),
                    };
                    let video_resp = client.get_video_url(request).await?;
                    let video_urls = non_empty_playback_urls(
                        video_resp
                            .segments
                            .iter()
                            .map(|segment| segment.url.clone()),
                        "video durl",
                    )?;

                    let bilibili = metadata
                        .bilibili
                        .get_or_insert_with(BilibiliPlaybackMetadata::default);
                    bilibili.fallback_format = Some("durl".to_string());
                    bilibili.quality = Some(video_resp.current_quality);
                    playback_infos.insert(
                        "mp4".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "MP4",
                                video_urls,
                                "mp4",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles,
                            Vec::new(),
                        ),
                    );
                    "mp4".to_string()
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: Self::NAME.to_string(),
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds,
                    is_live: Some(false),
                    metadata,
                })
            }

            BilibiliSourceConfig::Pgc(config) => {
                let epid = config.epid;
                let cid = config.cid;

                let request = synctv_media_providers::grpc::bilibili::GetDashPgcurlReq {
                    epid,
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                let dash_resp = match client.get_dash_pgcurl(request).await {
                    Ok(dash_resp) if dash_resp.dash.is_some() => Some(dash_resp),
                    Ok(_) => None,
                    Err(error) if is_bilibili_pgc_dash_unavailable(&error) => None,
                    Err(error) => return Err(error.into()),
                };

                let mut metadata = PlaybackMetadata {
                    content_type: Some("pgc".to_string()),
                    bilibili: Some(BilibiliPlaybackMetadata {
                        epid: Some(epid),
                        cid: Some(cid),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let mut subtitles = Vec::new();

                let subtitle_request = synctv_media_providers::grpc::bilibili::GetSubtitlesReq {
                    aid: 0,
                    bvid: String::new(),
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                match client.get_subtitles(subtitle_request).await {
                    Ok(subtitle_resp) => {
                        subtitles = subtitle_resp
                            .subtitles
                            .into_iter()
                            .map(|(name, url)| bilibili_subtitle_track(name, url))
                            .collect();
                    }
                    Err(e) => {
                        tracing::warn!(
                            epid = %epid, cid = %cid, error = %e,
                            "Failed to fetch Bilibili subtitles for PGC content, continuing without subtitles"
                        );
                    }
                }

                let duration_seconds = dash_resp
                    .as_ref()
                    .and_then(|resp| resp.dash.as_ref())
                    .map(|dash| dash.duration);
                if let Some(d) = dash_resp.as_ref().and_then(|resp| resp.dash.as_ref()) {
                    metadata.duration = Some(d.duration);
                }

                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);
                let mut playback_infos = HashMap::new();

                let default_mode = if let Some(dash_resp) = dash_resp {
                    let dash = dash_resp.dash.as_ref().ok_or_else(|| {
                        ProviderError::ApiError(
                            "Bilibili PGC playback response did not include DASH streams"
                                .to_string(),
                        )
                    })?;
                    insert_dash_manifest_metadata(
                        &mut metadata,
                        BilibiliDashManifestSlot::Dash,
                        dash,
                    );
                    let pgc_urls = dash_playback_urls(dash, "PGC DASH")?;
                    let hevc_urls = dash_resp
                        .hevc_dash
                        .as_ref()
                        .filter(|dash| !dash.video_streams.is_empty())
                        .map(|dash| {
                            insert_dash_manifest_metadata(
                                &mut metadata,
                                BilibiliDashManifestSlot::Hevc,
                                dash,
                            );
                            dash_playback_urls(dash, "PGC HEVC DASH")
                        })
                        .transpose()?;

                    playback_infos.insert(
                        "dash".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "DASH",
                                pgc_urls,
                                "mpd",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles.clone(),
                            Vec::new(),
                        ),
                    );
                    if let Some(hevc_urls) = hevc_urls {
                        playback_infos.insert(
                            "hevc".to_string(),
                            mode_info(
                                bilibili_direct_medias(
                                    "HEVC",
                                    hevc_urls,
                                    "mpd",
                                    expires_at,
                                    bilibili_headers(),
                                ),
                                subtitles,
                                Vec::new(),
                            ),
                        );
                    }
                    "dash".to_string()
                } else {
                    let request = synctv_media_providers::grpc::bilibili::GetPgcurlReq {
                        epid,
                        cid,
                        quality: 80,
                        cookies: sanitized_cookies.clone(),
                    };
                    let pgc_resp = client.get_pgcurl(request).await?;
                    let pgc_urls = non_empty_playback_urls(
                        pgc_resp.segments.iter().map(|segment| segment.url.clone()),
                        "PGC durl",
                    )?;

                    let bilibili = metadata
                        .bilibili
                        .get_or_insert_with(BilibiliPlaybackMetadata::default);
                    bilibili.fallback_format = Some("durl".to_string());
                    bilibili.quality = Some(pgc_resp.current_quality);
                    playback_infos.insert(
                        "mp4".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "MP4",
                                pgc_urls,
                                "mp4",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles,
                            Vec::new(),
                        ),
                    );
                    "mp4".to_string()
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: Self::NAME.to_string(),
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds,
                    is_live: Some(false),
                    metadata,
                })
            }

            BilibiliSourceConfig::Live(config) => {
                let room_id = config.room_id;

                let request = synctv_media_providers::grpc::bilibili::GetLiveStreamsReq {
                    cid: room_id,
                    hls: true,
                    cookies: sanitized_cookies,
                };
                let live_resp = client.get_live_streams(request).await?;

                let mut playback_infos = HashMap::new();
                let metadata = PlaybackMetadata {
                    content_type: Some("live".to_string()),
                    is_live: Some(true),
                    bilibili: Some(BilibiliPlaybackMetadata {
                        room_id: Some(room_id),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let live_expires_at = Some(Utc::now().timestamp() + 120);

                for stream in live_resp.live_streams {
                    let quality_name = if stream.desc.is_empty() {
                        format!("quality_{}", stream.quality)
                    } else {
                        format!("{}_{}", stream.desc, stream.quality)
                    };
                    playback_infos.insert(
                        quality_name,
                        mode_info(
                            bilibili_direct_medias(
                                "Live HLS",
                                stream.urls,
                                "hls",
                                live_expires_at,
                                bilibili_live_headers(),
                            ),
                            Vec::new(),
                            bilibili_live_danmaku_track(ctx).into_iter().collect(),
                        ),
                    );
                }

                let default_mode = {
                    let mut keys: Vec<&String> = playback_infos.keys().collect();
                    keys.sort();
                    keys.into_iter()
                        .next()
                        .cloned()
                        .unwrap_or_else(|| "direct".to_string())
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: Self::NAME.to_string(),
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds: None,
                    is_live: Some(true),
                    metadata,
                })
            }
        }
    }
}
