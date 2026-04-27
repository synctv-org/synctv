use std::fmt;
use std::sync::Arc;

use crate::config::ExternalIdsConfig;
use crate::models::{BanRecordId, MediaId, PlaylistId, ReviewRequestId, RoomId, TypedId, UserId};

const USER_ID_TAG: u64 = 1;
const ROOM_ID_TAG: u64 = 2;
const MEDIA_ID_TAG: u64 = 3;
const PLAYLIST_ID_TAG: u64 = 4;
const REVIEW_REQUEST_ID_TAG: u64 = 5;
const BAN_RECORD_ID_TAG: u64 = 6;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PublicIdKind {
    User,
    Room,
    Media,
    Playlist,
    ReviewRequest,
    BanRecord,
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

/// Shared encoder/decoder for externally visible resource identifiers.
///
/// Core boundary APIs may accept and emit sqids strings. After decoding, core
/// services and repositories continue to use numeric typed IDs.
#[derive(Clone)]
pub struct PublicIdCodec {
    sqids: Arc<sqids::Sqids>,
}

impl std::fmt::Debug for PublicIdCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicIdCodec").finish_non_exhaustive()
    }
}

impl PublicIdCodec {
    pub fn from_config(config: &ExternalIdsConfig) -> Result<Self, String> {
        let options = sqids::Options::new(
            config
                .alphabet
                .clone()
                .filter(|alphabet| !alphabet.is_empty()),
            Some(config.min_length),
            None,
        );
        let sqids = sqids::Sqids::new(Some(options))
            .map_err(|error| format!("invalid external_ids sqids configuration: {error}"))?;
        Ok(Self {
            sqids: Arc::new(sqids),
        })
    }

    #[must_use]
    pub fn default_for_tests() -> Self {
        Self::from_config(&ExternalIdsConfig::default())
            .expect("default external ID config must be valid")
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
        self.decode_i64(value, T::PUBLIC_ID_KIND).map(T::from)
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

    fn encode_i64(&self, id: i64, kind: PublicIdKind) -> Result<String, String> {
        validate_positive_id(id, kind)?;
        let id = u64::try_from(id)
            .map_err(|_| format!("invalid {kind}: expected a positive integer"))?;
        self.sqids
            .encode(&[kind.tag(), id])
            .map_err(|error| format!("failed to encode {kind}: {error}"))
    }

    fn decode_i64(&self, value: &str, kind: PublicIdKind) -> Result<i64, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(format!("invalid {kind}: expected a non-empty sqid"));
        }

        let decoded = self.sqids.decode(trimmed);
        let [decoded_type_tag, id] = decoded.as_slice() else {
            return Err(format!("invalid {kind}: expected a typed sqid"));
        };
        if *decoded_type_tag != kind.tag() {
            return Err(format!("invalid {kind}: sqid type mismatch"));
        }
        let id = i64::try_from(*id).map_err(|_| format!("invalid {kind}: sqid exceeds i64"))?;
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

    #[test]
    fn typed_ids_use_distinct_public_domains() {
        let codec = PublicIdCodec::default_for_tests();

        let user = codec.encode_user_id(UserId::from(1)).unwrap();
        let room = codec.encode_room_id(RoomId::from(1)).unwrap();
        let media = codec.encode_media_id(MediaId::from(1)).unwrap();
        let playlist = codec.encode_playlist_id(PlaylistId::from(1)).unwrap();

        assert_ne!(user, room);
        assert_ne!(user, media);
        assert_ne!(user, playlist);
        assert_ne!(room, media);
        assert_ne!(room, playlist);
        assert_ne!(media, playlist);
    }

    #[test]
    fn typed_decode_rejects_wrong_domain() {
        let codec = PublicIdCodec::default_for_tests();
        let user = codec.encode_user_id(UserId::from(1)).unwrap();

        assert!(codec.decode_room_id(&user).is_err());
        assert!(codec.decode_media_id(&user).is_err());
        assert!(codec.decode_playlist_id(&user).is_err());
    }

    #[test]
    fn generic_public_ids_are_domain_separated() {
        let codec = PublicIdCodec::default_for_tests();
        let review = codec
            .encode_review_request_id(ReviewRequestId::from(1))
            .unwrap();
        let ban = codec.encode_ban_record_id(BanRecordId::from(1)).unwrap();

        assert_ne!(review, ban);
        assert_eq!(
            codec.decode_review_request_id(&review).unwrap(),
            ReviewRequestId::from(1)
        );
        assert!(codec.decode::<BanRecordId>(&review).is_err());
    }

    #[test]
    fn public_id_kind_displays_human_readable_label() {
        assert_eq!(PublicIdKind::ReviewRequest.to_string(), "ReviewRequestId");
        assert_eq!(PublicIdKind::BanRecord.to_string(), "BanRecordId");
    }
}
