//! Bilibili provider tests
//!
//! Tests for URL matching and public API surface of `BilibiliClient`.
//! Private methods (`is_wbi_stale_error`, `build_cookie_header`) are tested
//! inline in src/bilibili/client.rs via #[cfg(test)].

#![allow(clippy::unwrap_used)]
use synctv_media_providers::bilibili::BilibiliResource;
use synctv_media_providers::BilibiliClient;

#[test]
fn test_match_url_bvid_standard() {
    let matched = BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7mD").unwrap();
    assert_eq!(
        matched.resource,
        BilibiliResource::Video {
            bvid: "BV1xx411c7mD".to_string(),
            aid: 0,
            page: 0,
        }
    );
}

#[test]
fn test_match_url_bangumi_ep() {
    let matched =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep123456").unwrap();
    assert_eq!(
        matched.resource,
        BilibiliResource::PgcEpisode {
            episode_id: 123_456
        }
    );
}

#[test]
fn test_match_url_bangumi_ss() {
    let matched =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss654321").unwrap();
    assert_eq!(
        matched.resource,
        BilibiliResource::PgcSeason { season_id: 654_321 }
    );
}

#[test]
fn test_match_url_live_room() {
    let matched = BilibiliClient::match_url("https://live.bilibili.com/live/12345").unwrap();
    assert_eq!(matched.resource, BilibiliResource::Live { room_id: 12345 });
}

#[test]
fn test_match_dynamic_playlist_urls() {
    let up = BilibiliClient::match_url("https://space.bilibili.com/42/video").unwrap();
    assert_eq!(up.resource, BilibiliResource::UpVideos { mid: 42 });

    let favorite =
        BilibiliClient::match_url("https://space.bilibili.com/42/favlist?fid=99").unwrap();
    assert_eq!(
        favorite.resource,
        BilibiliResource::FavoriteVideos { media_id: 99 }
    );

    let collection =
        BilibiliClient::match_url("https://space.bilibili.com/42/lists/77?type=season").unwrap();
    assert_eq!(
        collection.resource,
        BilibiliResource::CollectionVideos {
            mid: 42,
            season_id: 77,
        }
    );

    let series =
        BilibiliClient::match_url("https://space.bilibili.com/42/lists/88?type=series").unwrap();
    assert_eq!(
        series.resource,
        BilibiliResource::SeriesVideos {
            mid: 42,
            series_id: 88,
        }
    );

    let watch_later = BilibiliClient::match_url("https://www.bilibili.com/watchlater").unwrap();
    assert_eq!(watch_later.resource, BilibiliResource::WatchLater);
}

#[test]
fn test_match_url_unrecognized_returns_err() {
    let result = BilibiliClient::match_url("https://example.com/not-bilibili");
    assert!(result.is_err());

    let result = BilibiliClient::match_url("https://www.bilibili.com/unknown/page");
    assert!(result.is_err());

    let result = BilibiliClient::match_url("");
    assert!(result.is_err());
}
