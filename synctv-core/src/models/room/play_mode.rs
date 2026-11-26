//! Selection from a room's ordered static playlist.
#[cfg(verus_only)]
use vstd::prelude::*;

/// Playback mode for auto-play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(
    not(verus_only),
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[cfg_attr(verus_only, verus_verify)]
pub enum PlayMode {
    /// Stop after the last item.
    #[default]
    Sequential,
    /// Repeat the current item.
    RepeatOne,
    /// Loop back to the first item.
    RepeatAll,
    /// Select another item at random when available.
    Shuffle,
}

#[derive(Clone, Copy)]
#[cfg_attr(verus_only, verus_verify)]
pub(crate) enum CurrentItem {
    Unset,
    Missing,
    At(usize),
}

#[cfg_attr(verus_only, verus_verify)]
pub(crate) enum StaticChoice {
    Stop,
    At(usize),
    SampleOthers,
}

impl PlayMode {
    #[cfg_attr(verus_only, verus_spec(choice => ensures
        choice matches StaticChoice::At(i) ==> i < len,
        (choice is Stop) == (len == 0 || (self is Sequential && (current matches CurrentItem::At(i) && i + 1 == len))),
        (choice is SampleOthers) == (len > 1 && self is Shuffle && !(current is Unset)),
        choice matches StaticChoice::At(i) ==> match current {
            CurrentItem::Unset | CurrentItem::Missing => i == 0,
            CurrentItem::At(pos) => match self {
                PlayMode::RepeatOne => i == if pos < len { pos } else { 0usize },
                PlayMode::Sequential | PlayMode::RepeatAll => i == if pos + 1 < len { pos + 1 } else { 0int },
                PlayMode::Shuffle => i == 0,
            },
        },
    ))]
    pub(crate) fn static_choice(self, len: usize, current: CurrentItem) -> StaticChoice {
        if len == 0 {
            return StaticChoice::Stop;
        }
        if matches!(current, CurrentItem::Unset) {
            return StaticChoice::At(0);
        }
        if matches!(self, Self::Shuffle) && len > 1 {
            return StaticChoice::SampleOthers;
        }
        let CurrentItem::At(pos) = current else {
            return StaticChoice::At(0);
        };
        if pos >= len {
            return StaticChoice::At(0);
        }
        match self {
            Self::RepeatOne => StaticChoice::At(pos),
            Self::Shuffle => StaticChoice::At(0),
            Self::Sequential | Self::RepeatAll => {
                if pos + 1 < len {
                    StaticChoice::At(pos + 1)
                } else if matches!(self, Self::RepeatAll) {
                    StaticChoice::At(0)
                } else {
                    StaticChoice::Stop
                }
            }
        }
    }
}
