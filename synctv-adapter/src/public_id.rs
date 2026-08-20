use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use synctv_core::models::{
    BanRecordId, ContentReportId, MediaId, PlaylistId, ReviewRequestId, RoomCategoryId, RoomId,
    RoomLabelId, TypedId, UserId,
};

// Public IDs are external API/management presentation identifiers.
// Keep this codec in the adapter layer so core remains unaware of public ID
// encoding, prefixes, and transport-facing identifier formats. File and
// environment parsing live in the top-level `synctv` crate.

const USER_ID_TAG: u64 = 1;
const ROOM_ID_TAG: u64 = 2;
const MEDIA_ID_TAG: u64 = 3;
const PLAYLIST_ID_TAG: u64 = 4;
const REVIEW_REQUEST_ID_TAG: u64 = 5;
const BAN_RECORD_ID_TAG: u64 = 6;
const CONTENT_REPORT_ID_TAG: u64 = 7;
const ROOM_CATEGORY_ID_TAG: u64 = 8;
const ROOM_LABEL_ID_TAG: u64 = 9;
const PLAYBACK_HISTORY_ENTRY_ID_TAG: u64 = 10;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PublicIdKind {
    User,
    Room,
    Media,
    Playlist,
    ReviewRequest,
    BanRecord,
    ContentReport,
    RoomCategory,
    RoomLabel,
    PlaybackHistoryEntry,
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
            Self::RoomCategory => "RoomCategoryId",
            Self::RoomLabel => "RoomLabelId",
            Self::PlaybackHistoryEntry => "PlaybackHistoryEntryId",
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
            Self::RoomCategory => ROOM_CATEGORY_ID_TAG,
            Self::RoomLabel => ROOM_LABEL_ID_TAG,
            Self::PlaybackHistoryEntry => PLAYBACK_HISTORY_ENTRY_ID_TAG,
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
            Self::RoomCategory => "roomcat_",
            Self::RoomLabel => "roomlbl_",
            Self::PlaybackHistoryEntry => "ph_",
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

impl PublicIdType for RoomCategoryId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::RoomCategory;
}

impl PublicIdType for RoomLabelId {
    const PUBLIC_ID_KIND: PublicIdKind = PublicIdKind::RoomLabel;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicIdConfig {
    /// Optional sqids configuration for externally visible resource identifiers.
    ///
    /// Leave unset to use the default prefixed decimal format.
    pub sqids: Option<PublicIdSqidsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicIdSqidsConfig {
    /// Optional sqids alphabet. Leave empty/None to use the crate default.
    pub alphabet: Option<String>,
    /// Minimum encoded resource ID length.
    pub min_length: u8,
}

impl Default for PublicIdSqidsConfig {
    fn default() -> Self {
        Self {
            alphabet: None,
            min_length: 12,
        }
    }
}

impl PublicIdConfig {
    pub fn validate(&self) -> Result<(), String> {
        let Some(sqids) = self.sqids.as_ref() else {
            return Ok(());
        };

        let Some(alphabet) = sqids
            .alphabet
            .as_deref()
            .map(str::trim)
            .filter(|alphabet| !alphabet.is_empty())
        else {
            return Ok(());
        };

        validate_sqids_alphabet(alphabet)
    }
}

/// Shared encoder/decoder for externally visible resource identifiers.
///
/// API entrypoints decode external IDs before calling core services and encode
/// typed IDs before rendering responses.
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

    pub fn from_config(config: &PublicIdConfig) -> Result<Self, String> {
        let Some(sqids_config) = config.sqids.as_ref() else {
            return Ok(Self::plain());
        };

        let alphabet = sqids_config
            .alphabet
            .clone()
            .filter(|alphabet| !alphabet.is_empty());
        if let Some(alphabet) = alphabet.as_deref() {
            validate_sqids_alphabet(alphabet)?;
        }

        let options = sqids::Options::new(alphabet, Some(sqids_config.min_length), None);
        let sqids = sqids::Sqids::new(Some(options))
            .map_err(|error| format!("invalid public_ids.sqids configuration: {error}"))?;
        Ok(Self {
            encoding: PublicIdEncoding::Sqids(Arc::new(sqids)),
        })
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

    pub fn encode_room_category_id(&self, id: RoomCategoryId) -> Result<String, String> {
        self.encode(id)
    }

    pub fn encode_room_label_id(&self, id: RoomLabelId) -> Result<String, String> {
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

    pub fn encode_playback_history_entry_id(&self, id: i64) -> Result<String, String> {
        self.encode_i64(id, PublicIdKind::PlaybackHistoryEntry)
    }

    pub fn decode_user_id(&self, value: &str) -> Result<UserId, String> {
        self.decode(value)
    }

    pub fn decode_room_id(&self, value: &str) -> Result<RoomId, String> {
        self.decode(value)
    }

    pub fn decode_room_category_id(&self, value: &str) -> Result<RoomCategoryId, String> {
        self.decode(value)
    }

    pub fn decode_room_label_id(&self, value: &str) -> Result<RoomLabelId, String> {
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

    pub fn decode_playback_history_entry_id(&self, value: &str) -> Result<i64, String> {
        self.decode_i64(value, PublicIdKind::PlaybackHistoryEntry)
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

fn validate_sqids_alphabet(alphabet: &str) -> Result<(), String> {
    if alphabet.chars().count() < 3 {
        return Err(
            "invalid public_ids.sqids.alphabet: expected at least 3 characters".to_string(),
        );
    }
    if !alphabet.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(
            "invalid public_ids.sqids.alphabet: expected ASCII alphanumeric characters only"
                .to_string(),
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    if !alphabet.chars().all(|character| seen.insert(character)) {
        return Err("invalid public_ids.sqids.alphabet: expected unique characters".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn id<T>(value: i64) -> T
    where
        T: TryFrom<i64, Error = String>,
    {
        ok(T::try_from(value), "test ID should be positive")
    }

    #[test]
    fn default_codec_uses_prefixed_decimal_ids() {
        let codec = PublicIdCodec::plain();

        assert_eq!(
            ok(
                codec.encode_user_id(id::<UserId>(1)),
                "user ID should encode",
            ),
            "usr_1"
        );
        assert_eq!(
            ok(
                codec.encode_room_id(id::<RoomId>(1)),
                "room ID should encode",
            ),
            "room_1"
        );
        assert_eq!(
            ok(
                codec.encode_media_id(id::<MediaId>(1)),
                "media ID should encode",
            ),
            "med_1"
        );
        assert_eq!(
            ok(
                codec.encode_playlist_id(id::<PlaylistId>(1)),
                "playlist ID should encode",
            ),
            "pl_1"
        );
        assert_eq!(
            ok(
                codec.encode_review_request_id(id::<ReviewRequestId>(1)),
                "review request ID should encode",
            ),
            "rev_1"
        );
        assert_eq!(
            ok(
                codec.encode_ban_record_id(id::<BanRecordId>(1)),
                "ban record ID should encode",
            ),
            "ban_1"
        );
        assert_eq!(
            ok(
                codec.encode_room_category_id(id::<RoomCategoryId>(1)),
                "room category ID should encode",
            ),
            "roomcat_1"
        );
        assert_eq!(
            ok(
                codec.encode_room_label_id(id::<RoomLabelId>(1)),
                "room label ID should encode",
            ),
            "roomlbl_1"
        );
        assert_eq!(
            ok(
                codec.encode_playback_history_entry_id(1),
                "playback history ID should encode",
            ),
            "ph_1"
        );
        assert_eq!(
            ok(
                codec.decode_playback_history_entry_id("ph_1"),
                "playback history ID should decode",
            ),
            1
        );
    }

    #[test]
    fn default_decode_requires_correct_prefix() {
        let codec = PublicIdCodec::plain();

        assert_eq!(
            ok(codec.decode_user_id("usr_1"), "user ID should decode"),
            id::<UserId>(1)
        );
        assert!(codec.decode_room_id("usr_1").is_err());
        assert!(codec.decode_user_id("room_1").is_err());
        assert!(codec.decode_room_category_id("1").is_err());
        assert!(codec.decode_room_category_id("roomlbl_1").is_err());
        assert!(codec.decode_room_label_id("roomcat_1").is_err());
        assert!(codec.decode_user_id("1").is_err());
    }

    #[test]
    fn default_decode_rejects_invalid_payload() {
        let codec = PublicIdCodec::plain();

        assert!(codec.decode_user_id("usr_").is_err());
        assert!(codec.decode_user_id("usr_0").is_err());
        assert!(codec.decode_user_id("usr_-1").is_err());
        assert!(codec.decode_user_id("usr_abc").is_err());
    }

    #[test]
    fn sqids_mode_keeps_prefix_and_type_domain() {
        let codec = PublicIdCodec::from_config(&PublicIdConfig {
            sqids: Some(PublicIdSqidsConfig::default()),
        })
        .map_err(|error| error.clone());
        let codec = ok(codec, "sqids codec should build");

        let user = ok(
            codec.encode_user_id(id::<UserId>(1)),
            "user ID should encode",
        );

        assert!(user.starts_with("usr_"));
        assert_ne!(user, "usr_1");
        assert_eq!(
            ok(codec.decode_user_id(&user), "user ID should decode"),
            id::<UserId>(1)
        );
        assert!(codec.decode_room_id(&user).is_err());
    }

    #[test]
    fn sqids_mode_rejects_invalid_alphabet() {
        let codec = PublicIdCodec::from_config(&PublicIdConfig {
            sqids: Some(PublicIdSqidsConfig {
                alphabet: Some("aa".to_string()),
                min_length: 8,
            }),
        });

        assert!(codec.is_err());
    }

    #[test]
    fn sqids_mode_rejects_alphabet_outside_api_id_body_grammar() {
        let codec = PublicIdCodec::from_config(&PublicIdConfig {
            sqids: Some(PublicIdSqidsConfig {
                alphabet: Some(
                    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-".to_string(),
                ),
                min_length: 8,
            }),
        });

        let error = codec.expect_err("punctuation alphabet should be rejected");
        assert!(
            error.contains("ASCII alphanumeric"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn generic_public_ids_are_domain_separated_by_prefix() {
        let codec = PublicIdCodec::plain();
        let review = ok(
            codec.encode_review_request_id(id::<ReviewRequestId>(1)),
            "review request ID should encode",
        );
        let ban = ok(
            codec.encode_ban_record_id(id::<BanRecordId>(1)),
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
            id::<ReviewRequestId>(1)
        );
        assert!(codec.decode::<BanRecordId>(&review).is_err());
    }

    #[test]
    fn public_id_kind_displays_human_readable_label() {
        assert_eq!(PublicIdKind::ReviewRequest.to_string(), "ReviewRequestId");
        assert_eq!(PublicIdKind::BanRecord.to_string(), "BanRecordId");
        assert_eq!(PublicIdKind::RoomCategory.to_string(), "RoomCategoryId");
        assert_eq!(PublicIdKind::RoomLabel.to_string(), "RoomLabelId");
    }
}
