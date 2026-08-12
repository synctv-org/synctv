use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProviderTarget {
    Alist(AlistTarget),
    Bilibili(BilibiliTarget),
    Emby(EmbyTarget),
    Cloudreve(CloudreveTarget),
    Twitch(TwitchTarget),
    Youtube(YoutubeTarget),
    Douyin(DouyinTarget),
    Fnos(FnosTarget),
    Qnap(QnapTarget),
    Synology(SynologyTarget),
    Nextcloud(NextcloudTarget),
    Seafile(SeafileTarget),
    TrueNas(TrueNasTarget),
    TikTok(TikTokTarget),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BilibiliTarget {
    Video {
        bvid: String,
        aid: u64,
    },
    VideoPart {
        bvid: String,
        aid: u64,
        cid: u64,
        page: u32,
    },
    PgcEpisode {
        epid: u64,
        cid: u64,
    },
    Live {
        room_id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlistTarget {
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EmbyTarget {
    Item { item_id: String },
    Person { person_id: String },
    PersonItem { person_id: String, item_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudreveTarget {
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosTarget {
    pub target: FnosTargetKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QnapTarget {
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudTarget {
    pub path: String,
    pub file_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeafileTarget {
    pub repository_id: String,
    pub path: String,
    pub object_id: String,
    pub has_thumbnail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrueNasTarget {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SynologyTarget {
    File {
        relative_path: String,
    },
    LibraryItem {
        kind: super::SynologyLibraryItemKind,
        item_id: i64,
        file_id: i64,
        parent_id: Option<i64>,
    },
    TvShow {
        library_id: i64,
        tv_show_id: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FnosTargetKind {
    File {
        relative_path: String,
    },
    MediaItem {
        item_guid: String,
        media_guid: Option<String>,
        library_guid: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TwitchTargetKind {
    Video,
    Clip,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwitchTarget {
    pub kind: TwitchTargetKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeTarget {
    pub video_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DouyinTarget {
    pub aweme_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TikTokTarget {
    pub video_id: String,
}

impl ProviderTarget {
    #[must_use]
    pub fn alist(relative_path: String) -> Self {
        Self::Alist(AlistTarget { relative_path })
    }

    #[must_use]
    pub fn bilibili_video(bvid: String, aid: u64) -> Self {
        Self::Bilibili(BilibiliTarget::Video { bvid, aid })
    }

    #[must_use]
    pub fn bilibili_video_part(bvid: String, aid: u64, cid: u64, page: u32) -> Self {
        Self::Bilibili(BilibiliTarget::VideoPart {
            bvid,
            aid,
            cid,
            page,
        })
    }

    #[must_use]
    pub fn bilibili_pgc_episode(epid: u64, cid: u64) -> Self {
        Self::Bilibili(BilibiliTarget::PgcEpisode { epid, cid })
    }

    #[must_use]
    pub fn bilibili_live(room_id: u64) -> Self {
        Self::Bilibili(BilibiliTarget::Live { room_id })
    }

    #[must_use]
    pub fn emby(item_id: String) -> Self {
        Self::Emby(EmbyTarget::Item { item_id })
    }

    #[must_use]
    pub fn emby_person(person_id: String) -> Self {
        Self::Emby(EmbyTarget::Person { person_id })
    }

    #[must_use]
    pub fn emby_person_item(person_id: String, item_id: String) -> Self {
        Self::Emby(EmbyTarget::PersonItem { person_id, item_id })
    }

    #[must_use]
    pub fn cloudreve(relative_path: String) -> Self {
        Self::Cloudreve(CloudreveTarget { relative_path })
    }

    #[must_use]
    pub fn twitch(kind: TwitchTargetKind, id: String) -> Self {
        Self::Twitch(TwitchTarget { kind, id })
    }

    #[must_use]
    pub fn youtube(video_id: String) -> Self {
        Self::Youtube(YoutubeTarget { video_id })
    }

    #[must_use]
    pub fn douyin(aweme_id: String) -> Self {
        Self::Douyin(DouyinTarget { aweme_id })
    }

    #[must_use]
    pub fn tiktok(video_id: String) -> Self {
        Self::TikTok(TikTokTarget { video_id })
    }

    #[must_use]
    pub fn fnos(relative_path: String) -> Self {
        Self::Fnos(FnosTarget {
            target: FnosTargetKind::File { relative_path },
        })
    }

    #[must_use]
    pub fn fnos_media(
        item_guid: String,
        media_guid: Option<String>,
        library_guid: Option<String>,
    ) -> Self {
        Self::Fnos(FnosTarget {
            target: FnosTargetKind::MediaItem {
                item_guid,
                media_guid,
                library_guid,
            },
        })
    }

    #[must_use]
    pub fn qnap(relative_path: String) -> Self {
        Self::Qnap(QnapTarget { relative_path })
    }

    #[must_use]
    pub fn nextcloud(path: String, file_id: u64) -> Self {
        Self::Nextcloud(NextcloudTarget { path, file_id })
    }

    #[must_use]
    pub fn seafile(
        repository_id: String,
        path: String,
        object_id: String,
        has_thumbnail: bool,
    ) -> Self {
        Self::Seafile(SeafileTarget {
            repository_id,
            path,
            object_id,
            has_thumbnail,
        })
    }

    #[must_use]
    pub fn truenas(path: String) -> Self {
        Self::TrueNas(TrueNasTarget { path })
    }

    #[must_use]
    pub fn synology_file(relative_path: String) -> Self {
        Self::Synology(SynologyTarget::File { relative_path })
    }

    #[must_use]
    pub fn synology_library_item(
        kind: super::SynologyLibraryItemKind,
        item_id: i64,
        file_id: i64,
        parent_id: Option<i64>,
    ) -> Self {
        Self::Synology(SynologyTarget::LibraryItem {
            kind,
            item_id,
            file_id,
            parent_id,
        })
    }

    #[must_use]
    pub fn synology_tv_show(library_id: i64, tv_show_id: i64) -> Self {
        Self::Synology(SynologyTarget::TvShow {
            library_id,
            tv_show_id,
        })
    }

    pub fn stable_bytes(&self) -> crate::Result<Vec<u8>> {
        fn push_field(bytes: &mut Vec<u8>, value: &str) -> crate::Result<()> {
            let value = value.as_bytes();
            let len = u32::try_from(value.len()).map_err(|_| {
                crate::Error::InvalidInput("provider target field exceeds u32::MAX".to_string())
            })?;
            bytes.extend_from_slice(&len.to_be_bytes());
            bytes.extend_from_slice(value);
            Ok(())
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"synctv.provider_target.v1\0");
        match self {
            Self::Alist(target) => {
                bytes.push(1);
                push_field(&mut bytes, &target.relative_path)?;
            }
            Self::Bilibili(target) => {
                bytes.push(14);
                match target {
                    BilibiliTarget::Video { bvid, aid } => {
                        bytes.push(1);
                        push_field(&mut bytes, bvid)?;
                        bytes.extend_from_slice(&aid.to_be_bytes());
                    }
                    BilibiliTarget::VideoPart {
                        bvid,
                        aid,
                        cid,
                        page,
                    } => {
                        bytes.push(2);
                        push_field(&mut bytes, bvid)?;
                        bytes.extend_from_slice(&aid.to_be_bytes());
                        bytes.extend_from_slice(&cid.to_be_bytes());
                        bytes.extend_from_slice(&page.to_be_bytes());
                    }
                    BilibiliTarget::PgcEpisode { epid, cid } => {
                        bytes.push(3);
                        bytes.extend_from_slice(&epid.to_be_bytes());
                        bytes.extend_from_slice(&cid.to_be_bytes());
                    }
                    BilibiliTarget::Live { room_id } => {
                        bytes.push(4);
                        bytes.extend_from_slice(&room_id.to_be_bytes());
                    }
                }
            }
            Self::Emby(target) => {
                bytes.push(2);
                match target {
                    EmbyTarget::Item { item_id } => {
                        bytes.push(1);
                        push_field(&mut bytes, item_id)?;
                    }
                    EmbyTarget::Person { person_id } => {
                        bytes.push(2);
                        push_field(&mut bytes, person_id)?;
                    }
                    EmbyTarget::PersonItem { person_id, item_id } => {
                        bytes.push(3);
                        push_field(&mut bytes, person_id)?;
                        push_field(&mut bytes, item_id)?;
                    }
                }
            }
            Self::Cloudreve(target) => {
                bytes.push(3);
                push_field(&mut bytes, &target.relative_path)?;
            }
            Self::Twitch(target) => {
                bytes.push(4);
                bytes.push(match target.kind {
                    TwitchTargetKind::Video => 1,
                    TwitchTargetKind::Clip => 2,
                    TwitchTargetKind::Live => 3,
                });
                push_field(&mut bytes, &target.id)?;
            }
            Self::Youtube(target) => {
                bytes.push(12);
                push_field(&mut bytes, &target.video_id)?;
            }
            Self::Douyin(target) => {
                bytes.push(11);
                push_field(&mut bytes, &target.aweme_id)?;
            }
            Self::TikTok(target) => {
                bytes.push(13);
                push_field(&mut bytes, &target.video_id)?;
            }
            Self::Fnos(target) => {
                bytes.push(5);
                match &target.target {
                    FnosTargetKind::File { relative_path } => {
                        bytes.push(1);
                        push_field(&mut bytes, relative_path)?;
                    }
                    FnosTargetKind::MediaItem {
                        item_guid,
                        media_guid,
                        library_guid,
                    } => {
                        bytes.push(2);
                        push_field(&mut bytes, item_guid)?;
                        push_field(&mut bytes, media_guid.as_deref().unwrap_or_default())?;
                        push_field(&mut bytes, library_guid.as_deref().unwrap_or_default())?;
                    }
                }
            }
            Self::Qnap(target) => {
                bytes.push(6);
                push_field(&mut bytes, &target.relative_path)?;
            }
            Self::Synology(target) => {
                bytes.push(7);
                match target {
                    SynologyTarget::File { relative_path } => {
                        bytes.push(1);
                        push_field(&mut bytes, relative_path)?;
                    }
                    SynologyTarget::LibraryItem {
                        kind,
                        item_id,
                        file_id,
                        parent_id,
                    } => {
                        bytes.push(2);
                        bytes.push(match kind {
                            super::SynologyLibraryItemKind::Movie => 1,
                            super::SynologyLibraryItemKind::Episode => 2,
                            super::SynologyLibraryItemKind::HomeVideo => 3,
                            super::SynologyLibraryItemKind::TvRecording => 4,
                        });
                        bytes.extend_from_slice(&item_id.to_be_bytes());
                        bytes.extend_from_slice(&file_id.to_be_bytes());
                        bytes.extend_from_slice(&parent_id.unwrap_or_default().to_be_bytes());
                    }
                    SynologyTarget::TvShow {
                        library_id,
                        tv_show_id,
                    } => {
                        bytes.push(3);
                        bytes.extend_from_slice(&library_id.to_be_bytes());
                        bytes.extend_from_slice(&tv_show_id.to_be_bytes());
                    }
                }
            }
            Self::Nextcloud(target) => {
                bytes.push(8);
                push_field(&mut bytes, &target.path)?;
                bytes.extend_from_slice(&target.file_id.to_be_bytes());
            }
            Self::Seafile(target) => {
                bytes.push(9);
                push_field(&mut bytes, &target.repository_id)?;
                push_field(&mut bytes, &target.path)?;
                push_field(&mut bytes, &target.object_id)?;
                bytes.push(u8::from(target.has_thumbnail));
            }
            Self::TrueNas(target) => {
                bytes.push(10);
                push_field(&mut bytes, &target.path)?;
            }
        }
        Ok(bytes)
    }

    pub fn hash(&self) -> crate::Result<String> {
        Ok(hex::encode(Sha256::digest(self.stable_bytes()?)))
    }
}

pub fn hash_optional_provider_target(target: Option<&ProviderTarget>) -> crate::Result<String> {
    target.map_or_else(|| Ok(hash_empty_provider_target()), ProviderTarget::hash)
}

#[must_use]
pub fn hash_empty_provider_target() -> String {
    hex::encode(Sha256::digest([]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_target_ignores_unknown_persisted_fields() {
        let target: ProviderTarget = serde_json::from_value(serde_json::json!({
            "provider": "synology",
            "type": "file",
            "relative_path": "/movies/demo.mkv",
            "futureTargetField": "ignored"
        }))
        .expect("provider target should ignore unknown persisted fields");

        assert_eq!(
            target,
            ProviderTarget::synology_file("/movies/demo.mkv".to_string())
        );
    }
}
