use sha2::{Digest, Sha256};

use crate::models::media::{PlaybackDanmaku, PlaybackMedia, PlaybackSubtitle};

const SWARM_ID_DOMAIN: &str = "synctv-provider-resource-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P2pResourceDelivery {
    pub swarm_id: String,
}

/// Build an opaque P2P swarm identity from a provider-owned resource descriptor.
///
/// The provider owns byte-equivalence and calls this while constructing playback.
/// Provider transport URLs, access tokens, and API-layer mode indexes stay outside
/// this contract. Room isolation is enforced by the room-keyed tracker and the
/// room/actor/swarm-bound capability ticket.
#[must_use]
pub(crate) fn provider_p2p_swarm_id(
    provider: &str,
    provider_instance: Option<&str>,
    resource_kind: &str,
    resource_id: &str,
) -> String {
    let canonical = format!(
        "{SWARM_ID_DOMAIN}\nprovider:{provider}\ninstance:{}\nkind:{resource_kind}\nresource:{resource_id}",
        provider_instance.unwrap_or_default(),
    );
    format!("sm3_{}", hex::encode(Sha256::digest(canonical.as_bytes())))
}

#[must_use]
pub fn playback_media_p2p_delivery(media: &PlaybackMedia) -> Option<P2pResourceDelivery> {
    media
        .p2p_swarm_id
        .as_deref()
        .filter(|swarm_id| !swarm_id.trim().is_empty())
        .map(|swarm_id| P2pResourceDelivery {
            swarm_id: swarm_id.to_string(),
        })
}

#[must_use]
pub fn playback_subtitle_p2p_delivery(subtitle: &PlaybackSubtitle) -> Option<P2pResourceDelivery> {
    subtitle
        .p2p_swarm_id
        .as_deref()
        .filter(|swarm_id| !swarm_id.trim().is_empty())
        .map(|swarm_id| P2pResourceDelivery {
            swarm_id: swarm_id.to_string(),
        })
}

#[must_use]
pub fn playback_danmaku_p2p_delivery(danmaku: &PlaybackDanmaku) -> Option<P2pResourceDelivery> {
    danmaku
        .p2p_swarm_id
        .as_deref()
        .filter(|swarm_id| !swarm_id.trim().is_empty())
        .map(|swarm_id| P2pResourceDelivery {
            swarm_id: swarm_id.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::media::{
        PlaybackDanmakuProvider, PlaybackDirectUrlDanmaku, PlaybackDirectUrlMedia,
        PlaybackDirectUrlSubtitle, PlaybackMediaProvider, PlaybackSubtitleProvider,
    };

    #[test]
    fn provider_resource_identity_is_stable_and_namespaced() {
        let first =
            provider_p2p_swarm_id("direct_url", None, "media", "stable-resource-descriptor");
        let repeated =
            provider_p2p_swarm_id("direct_url", None, "media", "stable-resource-descriptor");
        let other_provider =
            provider_p2p_swarm_id("cloudreve", None, "media", "stable-resource-descriptor");
        let other_instance = provider_p2p_swarm_id(
            "direct_url",
            Some("secondary"),
            "media",
            "stable-resource-descriptor",
        );
        let other_kind =
            provider_p2p_swarm_id("direct_url", None, "subtitle", "stable-resource-descriptor");

        assert_eq!(first, repeated);
        assert!(first.starts_with("sm3_"));
        assert_ne!(first, other_provider);
        assert_ne!(first, other_instance);
        assert_ne!(first, other_kind);
    }

    #[test]
    fn delivery_uses_only_provider_generated_identity() {
        let media = PlaybackMedia {
            name: "media".to_string(),
            format: "mp4".to_string(),
            expire_at: None,
            metadata: None,
            p2p_swarm_id: Some("sm3_media".to_string()),
            provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                url: "https://example.com/media.mp4".to_string(),
                headers: Default::default(),
            }),
        };
        let subtitle = PlaybackSubtitle {
            name: "subtitle".to_string(),
            language: "en".to_string(),
            format: "vtt".to_string(),
            p2p_swarm_id: Some("sm3_subtitle".to_string()),
            provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                url: "https://example.com/subtitle.vtt".to_string(),
                headers: Default::default(),
                expire_at: None,
            }),
        };
        let danmaku = PlaybackDanmaku {
            name: "danmaku".to_string(),
            format: Some("xml".to_string()),
            p2p_swarm_id: Some("sm3_danmaku".to_string()),
            provider: PlaybackDanmakuProvider::DirectUrl(PlaybackDirectUrlDanmaku {
                url: "https://example.com/danmaku.xml".to_string(),
                headers: Default::default(),
                expire_at: None,
            }),
        };

        assert_eq!(
            playback_media_p2p_delivery(&media).map(|delivery| delivery.swarm_id),
            Some("sm3_media".to_string())
        );
        assert_eq!(
            playback_subtitle_p2p_delivery(&subtitle).map(|delivery| delivery.swarm_id),
            Some("sm3_subtitle".to_string())
        );
        assert_eq!(
            playback_danmaku_p2p_delivery(&danmaku).map(|delivery| delivery.swarm_id),
            Some("sm3_danmaku".to_string())
        );
    }

    #[test]
    fn blank_provider_identity_disables_p2p_delivery() {
        let media = PlaybackMedia {
            name: "media".to_string(),
            format: "mp4".to_string(),
            expire_at: None,
            metadata: None,
            p2p_swarm_id: Some("  ".to_string()),
            provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                url: "https://example.com/media.mp4".to_string(),
                headers: Default::default(),
            }),
        };

        assert_eq!(playback_media_p2p_delivery(&media), None);
    }
}
