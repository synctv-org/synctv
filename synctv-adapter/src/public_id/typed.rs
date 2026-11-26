#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(verus_only, verus_verify)]
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
    #[cfg_attr(verus_only, verus_spec(result => ensures
        (result == 1) == (self is User),
        (result == 2) == (self is Room),
        (result == 3) == (self is Media),
        (result == 4) == (self is Playlist),
        (result == 5) == (self is ReviewRequest),
        (result == 6) == (self is BanRecord),
        (result == 7) == (self is ContentReport),
        (result == 8) == (self is RoomCategory),
        (result == 9) == (self is RoomLabel),
        (result == 10) == (self is PlaybackHistoryEntry),
    ))]
    pub const fn tag(self) -> u64 {
        match self {
            Self::User => 1,
            Self::Room => 2,
            Self::Media => 3,
            Self::Playlist => 4,
            Self::ReviewRequest => 5,
            Self::BanRecord => 6,
            Self::ContentReport => 7,
            Self::RoomCategory => 8,
            Self::RoomLabel => 9,
            Self::PlaybackHistoryEntry => 10,
        }
    }
}
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum BodyError {
    Shape,
    Kind,
    Overflow,
    Zero,
}

/// A typed Sqids payload contains exactly the expected resource tag and one positive database ID.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (words.len() == 2 && words[0] == expected && 0 < words[1] <= i64::MAX),
    out matches Ok(id) ==> id == words[1],
    (out matches Err(BodyError::Shape)) == (words.len() != 2),
    (out matches Err(BodyError::Kind)) == (words.len() == 2 && words[0] != expected),
    (out matches Err(BodyError::Overflow)) == (words.len() == 2 && words[0] == expected && words[1] > i64::MAX),
    (out matches Err(BodyError::Zero)) == (words.len() == 2 && words[0] == expected && words[1] == 0),
))]
pub(super) fn decode(words: &[u64], expected: u64) -> Result<i64, BodyError> {
    if words.len() != 2 {
        return Err(BodyError::Shape);
    }
    if words[0] != expected {
        return Err(BodyError::Kind);
    }
    if words[1] == 0 {
        return Err(BodyError::Zero);
    }
    match i64::try_from(words[1]) {
        Ok(id) => Ok(id),
        Err(_) => Err(BodyError::Overflow),
    }
}
