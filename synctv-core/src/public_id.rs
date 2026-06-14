use std::fmt;
use std::sync::Arc;

use crate::config::PublicIdsConfig;
use crate::models::{
    BanRecordId, ContentReportId, MediaId, PlaylistId, ReviewRequestId, RoomId, TypedId, UserId,
};

const USER_ID_TAG: u64 = 1;
const ROOM_ID_TAG: u64 = 2;
const MEDIA_ID_TAG: u64 = 3;
const PLAYLIST_ID_TAG: u64 = 4;
const REVIEW_REQUEST_ID_TAG: u64 = 5;
const BAN_RECORD_ID_TAG: u64 = 6;
const CONTENT_REPORT_ID_TAG: u64 = 7;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PublicIdKind {
    User,
    Room,
    Media,
    Playlist,
    ReviewRequest,
    BanRecord,
    ContentReport,
}

impl PublicIdKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::User => "UserId",
            Self::Room => "RoomId",
            Self::Media => "MediaId",
            Self::Playlist => "PlaylistId",
            Self::ReviewRequest => "ReviewRequestId",
            Self::BanRecord => "BanRecordId",
            Self::ContentReport => "ContentReportId",
        }
    }

    #[must_use]
    pub const fn tag(self) -> u64 {
        match self {
            Self::User => USER_ID_TAG,
            Self::Room => ROOM_ID_TAG,
            Self::Media => MEDIA_ID_TAG,
            Self::Playlist => PLAYLIST_ID_TAG,
            Self::ReviewRequest => REVIEW_REQUEST_ID_TAG,
            Self::BanRecord => BAN_RECORD_ID_TAG,
            Self::ContentReport => CONTENT_REPORT_ID_TAG,
        }
    }

    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::User => "usr_",
            Self::Room => "room_",
            Self::Media => "med_",
            Self::Playlist => "pl_",
            Self::ReviewRequest => "rev_",
            Self::BanRecord => "ban_",
            Self::ContentReport => "report_",
        }
    }
}

impl fmt::Display for PublicIdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

pub trait PublicIdType: TypedId {
    const PUBLIC_ID_KIND: PublicIdKind;
}

impl PublicIdType for UserId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::User;
}

impl PublicIdType for RoomId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::Room;
}

impl PublicIdType for MediaId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::Media;
}

impl PublicIdType for PlaylistId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::Playlist;
}

impl PublicIdType for ReviewRequestId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::ReviewRequest;
}

impl PublicIdType for BanRecordId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::BanRecord;
}

impl PublicIdType for ContentReportId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::ContentReport;
}

/// Shared encoder/decoder for externally visible resource identifiers.
///
/// Core boundary APIs may accept and emit sqids strings. After decoding, core
/// services and repositories continue to use numeric typed IDs.
#[derive(Clone)]
pub struct PublicIdCodec {
    encoding: PublicIdEncoding,
}

#[derive(Clone)]
enum PublicIdEncoding {
    Plain,
    Sqids(Arc<sqids::Sqids>),
}

impl std::fmt::Debug for PublicIdCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicIdCodec").finish_non_exhaustive()
    }
}

impl PublicIdCodec {
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            encoding: PublicIdEncoding::Plain,
        }
    }

    pub fn from_config(config: &PublicIdsConfig) -> Result<Self, String> {
        let Some(sqids_config) = config.sqids.as_ref() else {
            return Ok(Self::plain());
        };

        let options = sqids::Options::new(
            sqids_config
                .alphabet
                .clone()
                .filter(|alphabet| !alphabet.is_empty()),
            Some(sqids_config.min_length),
            None,
        );
        let sqids = sqids::Sqids::new(Some(options))
            .map_err(|error| format!("invalid public_ids.sqids configuration: {error}"))?;
        Ok(Self {
            encoding: PublicIdEncoding::Sqids(Arc::new(sqids)),
        })
    }

    #[cfg(test)]
    #[must_use]
    pub fn default_for_tests() -> Self {
        Self::plain()
    }

    pub fn encode<T>(&self, id: T) -> Result<String, String>
    where
        T: PublicIdType,
    {
        self.encode_i64(id.into(), T::PUBLIC_ID_KIND)
    }

    pub fn decode<T>(&self, value: &str) -> Result<T, String>
    where
        T: PublicIdType,
    {
        T::try_from(self.decode_i64(value, T::PUBLIC_ID_KIND)?)
    }

    pub fn encode_user_id(&self, id: UserId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn encode_room_id(&self, id: RoomId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn encode_media_id(&self, id: MediaId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn encode_playlist_id(&self, id: PlaylistId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn encode_review_request_id(&self, id: ReviewRequestId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn encode_ban_record_id(&self, id: BanRecordId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn encode_content_report_id(&self, id: ContentReportId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn decode_user_id(&self, value: &str) -> Result<UserId, String> {
        self.decode(value)
    }

    pub fn decode_room_id(&self, value: &str) -> Result<RoomId, String> {
        self.decode(value)
    }

    pub fn decode_media_id(&self, value: &str) -> Result<MediaId, String> {
        self.decode(value)
    }

    pub fn decode_playlist_id(&self, value: &str) -> Result<PlaylistId, String> {
        self.decode(value)
    }

    pub fn decode_review_request_id(&self, value: &str) -> Result<ReviewRequestId, String> {
        self.decode(value)
    }

    pub fn decode_content_report_id(&self, value: &str) -> Result<ContentReportId, String> {
        self.decode(value)
    }

    fn encode_i64(&self, id: i64, kind: PublicIdKind) -> Result<String, String> {
        validate_positive_id(id, kind)?;
        let id = u64::try_from(id)
            .map_err(|_| format!("invalid {kind}: expected a positive integer"))?;
        let payload = match &self.encoding {
            PublicIdEncoding::Plain => id.to_string(),
            PublicIdEncoding::Sqids(sqids) => sqids
                .encode(&[kind.tag(), id])
                .map_err(|error| format!("failed to encode {kind}: {error}"))?,
        };
        Ok(format!("{}{payload}", kind.prefix()))
    }

    fn decode_i64(&self, value: &str, kind: PublicIdKind) -> Result<i64, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(format!("invalid {kind}: expected a non-empty public ID"));
        }

        let Some(payload) = trimmed.strip_prefix(kind.prefix()) else {
            return Err(format!(
                "invalid {kind}: expected public ID prefix `{}`",
                kind.prefix()
            ));
        };
        if payload.is_empty() {
            return Err(format!(
                "invalid {kind}: expected a non-empty public ID body"
            ));
        }

        let id = match &self.encoding {
            PublicIdEncoding::Plain => payload
                .parse::<i64>()
                .map_err(|_| format!("invalid {kind}: expected a decimal public ID body"))?,
            PublicIdEncoding::Sqids(sqids) => {
                let decoded = sqids.decode(payload);
                let [decoded_type_tag, id] = decoded.as_slice() else {
                    return Err(format!("invalid {kind}: expected a typed sqid body"));
                };
                if *decoded_type_tag != kind.tag() {
                    return Err(format!("invalid {kind}: sqid type mismatch"));
                }
                i64::try_from(*id).map_err(|_| format!("invalid {kind}: sqid exceeds i64"))?
            }
        };
        validate_positive_id(id, kind)?;
        Ok(id)
    }
}

fn validate_positive_id(id: i64, kind: PublicIdKind) -> Result<(), String> {
    if id > 0 {
        Ok(())
    } else {
        Err(format!("invalid {kind}: expected a positive integer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PublicIdsSqidsConfig;

    fn ok<T, E: fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn default_codec_uses_prefixed_decimal_ids() {
        let codec = PublicIdCodec::default_for_tests();

        assert_eq!(
            ok(
                codec.encode_user_id(UserId::expect_positive(1)),
                "user ID should encode",
            ),
            "usr_1"
        );
        assert_eq!(
            ok(
                codec.encode_room_id(RoomId::expect_positive(1)),
                "room ID should encode",
            ),
            "room_1"
        );
        assert_eq!(
            ok(
                codec.encode_media_id(MediaId::expect_positive(1)),
                "media ID should encode",
            ),
            "med_1"
        );
        assert_eq!(
            ok(
                codec.encode_playlist_id(PlaylistId::expect_positive(1)),
                "playlist ID should encode",
            ),
            "pl_1"
        );
        assert_eq!(
            ok(
                codec.encode_review_request_id(ReviewRequestId::expect_positive(1)),
                "review request ID should encode",
            ),
            "rev_1"
        );
        assert_eq!(
            ok(
                codec.encode_ban_record_id(BanRecordId::expect_positive(1)),
                "ban record ID should encode",
            ),
            "ban_1"
        );
    }

    #[test]
    fn default_decode_requires_correct_prefix() {
        let codec = PublicIdCodec::default_for_tests();

        assert_eq!(
            ok(codec.decode_user_id("usr_1"), "user ID should decode"),
            UserId::expect_positive(1)
        );
        assert!(codec.decode_room_id("usr_1").is_err());
        assert!(codec.decode_user_id("room_1").is_err());
        assert!(codec.decode_user_id("1").is_err());
    }

    #[test]
    fn default_decode_rejects_invalid_payload() {
        let codec = PublicIdCodec::default_for_tests();

        assert!(codec.decode_user_id("usr_").is_err());
        assert!(codec.decode_user_id("usr_0").is_err());
        assert!(codec.decode_user_id("usr_-1").is_err());
        assert!(codec.decode_user_id("usr_abc").is_err());
    }

    #[test]
    fn sqids_mode_keeps_prefix_and_type_domain() {
        let codec = PublicIdCodec::from_config(&PublicIdsConfig {
            sqids: Some(PublicIdsSqidsConfig::default()),
        })
        .map_err(|error| error.clone());
        let codec = ok(codec, "sqids codec should build");

        let user = ok(
            codec.encode_user_id(UserId::expect_positive(1)),
            "user ID should encode",
        );

        assert!(user.starts_with("usr_"));
        assert_ne!(user, "usr_1");
        assert_eq!(
            ok(codec.decode_user_id(&user), "user ID should decode"),
            UserId::expect_positive(1)
        );
        assert!(codec.decode_room_id(&user).is_err());
    }

    #[test]
    fn generic_public_ids_are_domain_separated_by_prefix() {
        let codec = PublicIdCodec::default_for_tests();
        let review = ok(
            codec.encode_review_request_id(ReviewRequestId::expect_positive(1)),
            "review request ID should encode",
        );
        let ban = ok(
            codec.encode_ban_record_id(BanRecordId::expect_positive(1)),
            "ban record ID should encode",
        );

        assert_eq!(review, "rev_1");
        assert_eq!(ban, "ban_1");
        assert_ne!(review, ban);
        assert_eq!(
            ok(
                codec.decode_review_request_id(&review),
                "review request ID should decode",
            ),
            ReviewRequestId::expect_positive(1)
        );
        assert!(codec.decode::<BanRecordId>(&review).is_err());
    }

    #[test]
    fn public_id_kind_displays_human_readable_label() {
        assert_eq!(PublicIdKind::ReviewRequest.to_string(), "ReviewRequestId");
        assert_eq!(PublicIdKind::BanRecord.to_string(), "BanRecordId");
    }
}
