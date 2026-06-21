use tonic::Status;

fn encode_source_config(provider: &str, value: &serde_json::Value) -> Result<Vec<u8>, Status> {
    serde_json::to_vec(&value).map_err(|error| {
        tracing::error!(provider, error = %error, "failed to encode provider source config");
        Status::internal("failed to encode provider source config")
    })
}

fn trimmed_required(field_name: &str, value: &str) -> Result<String, Status> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must not be empty"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn alist_source_config(
    server_id: &str,
    path: &str,
    password: &str,
) -> Result<Vec<u8>, Status> {
    let mut source_config = serde_json::Map::new();
    source_config.insert(
        "server_id".to_string(),
        serde_json::Value::String(trimmed_required("server_id", server_id)?),
    );
    source_config.insert(
        "path".to_string(),
        serde_json::Value::String(trimmed_required("path", path)?),
    );
    let password = password.trim();
    if !password.is_empty() {
        source_config.insert(
            "password".to_string(),
            serde_json::Value::String(password.to_string()),
        );
    }
    encode_source_config("alist", &serde_json::Value::Object(source_config))
}

pub(crate) fn emby_source_config(server_id: &str, item_id: &str) -> Result<Vec<u8>, Status> {
    encode_source_config(
        "emby",
        &serde_json::json!({
            "server_id": trimmed_required("server_id", server_id)?,
            "item_id": trimmed_required("item_id", item_id)?,
        }),
    )
}

pub(crate) fn bilibili_video_source_config(
    bvid: &str,
    aid: Option<u64>,
    cid: u64,
    shared: bool,
) -> Result<Vec<u8>, Status> {
    if bvid.trim().is_empty() && aid.is_none() {
        return Err(Status::invalid_argument("bvid or aid is required"));
    }
    if cid == 0 {
        return Err(Status::invalid_argument("cid must be non-zero"));
    }

    let mut source_config = serde_json::Map::new();
    source_config.insert(
        "type".to_string(),
        serde_json::Value::String("video".to_string()),
    );
    let bvid = bvid.trim();
    if !bvid.is_empty() {
        source_config.insert(
            "bvid".to_string(),
            serde_json::Value::String(bvid.to_string()),
        );
    }
    if let Some(aid) = aid {
        source_config.insert("aid".to_string(), serde_json::Value::from(aid));
    }
    source_config.insert("cid".to_string(), serde_json::Value::from(cid));
    if shared {
        source_config.insert("shared".to_string(), serde_json::Value::Bool(true));
    }
    encode_source_config("bilibili", &serde_json::Value::Object(source_config))
}

pub(crate) fn bilibili_pgc_source_config(
    epid: u64,
    cid: u64,
    shared: bool,
) -> Result<Vec<u8>, Status> {
    if epid == 0 {
        return Err(Status::invalid_argument("epid must be non-zero"));
    }
    if cid == 0 {
        return Err(Status::invalid_argument("cid must be non-zero"));
    }
    let mut source_config = serde_json::Map::new();
    source_config.insert(
        "type".to_string(),
        serde_json::Value::String("pgc".to_string()),
    );
    source_config.insert("epid".to_string(), serde_json::Value::from(epid));
    source_config.insert("cid".to_string(), serde_json::Value::from(cid));
    if shared {
        source_config.insert("shared".to_string(), serde_json::Value::Bool(true));
    }
    encode_source_config("bilibili", &serde_json::Value::Object(source_config))
}

pub(crate) fn bilibili_live_source_config(
    room_live_id: u64,
    shared: bool,
) -> Result<Vec<u8>, Status> {
    if room_live_id == 0 {
        return Err(Status::invalid_argument("room_live_id must be non-zero"));
    }
    let mut source_config = serde_json::Map::new();
    source_config.insert(
        "type".to_string(),
        serde_json::Value::String("live".to_string()),
    );
    source_config.insert("room_id".to_string(), serde_json::Value::from(room_live_id));
    if shared {
        source_config.insert("shared".to_string(), serde_json::Value::Bool(true));
    }
    encode_source_config("bilibili", &serde_json::Value::Object(source_config))
}

pub(crate) fn direct_url_source_config(url: &str) -> Result<Vec<u8>, Status> {
    serde_json::to_vec(&serde_json::json!({ "url": url })).map_err(|error| {
        tracing::error!(error = %error, "failed to encode media source config");
        Status::internal("failed to encode media source config")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        alist_source_config, bilibili_live_source_config, bilibili_pgc_source_config,
        bilibili_video_source_config, direct_url_source_config, emby_source_config,
    };
    use serde_json::{json, Value};
    use tonic::Code;

    fn decode_config(config: &[u8]) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(config)
    }

    #[test]
    fn alist_source_config_trims_required_fields_and_optional_password(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&alist_source_config(
            "  server-a  ",
            "  /movies/file.mkv  ",
            "  secret  ",
        )?)?;

        assert_eq!(
            config,
            json!({
                "server_id": "server-a",
                "path": "/movies/file.mkv",
                "password": "secret",
            })
        );
        Ok(())
    }

    #[test]
    fn alist_source_config_omits_blank_password() -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&alist_source_config("server-a", "/movies/file.mkv", "  ")?)?;

        assert_eq!(
            config,
            json!({
                "server_id": "server-a",
                "path": "/movies/file.mkv",
            })
        );
        Ok(())
    }

    #[test]
    fn alist_source_config_rejects_blank_required_fields() {
        let status = alist_source_config("  ", "/movies/file.mkv", "")
            .expect_err("blank server_id should fail");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "server_id must not be empty");

        let status = alist_source_config("server-a", "  ", "").expect_err("blank path should fail");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "path must not be empty");
    }

    #[test]
    fn emby_source_config_trims_required_fields() -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&emby_source_config("  server-a  ", "  item-1  ")?)?;

        assert_eq!(
            config,
            json!({
                "server_id": "server-a",
                "item_id": "item-1",
            })
        );
        Ok(())
    }

    #[test]
    fn bilibili_video_source_config_accepts_bvid() -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&bilibili_video_source_config(
            "  BV1xx411c7mD  ",
            None,
            42,
            true,
        )?)?;

        assert_eq!(
            config,
            json!({
                "type": "video",
                "bvid": "BV1xx411c7mD",
                "cid": 42,
                "shared": true,
            })
        );
        Ok(())
    }

    #[test]
    fn bilibili_video_source_config_accepts_aid_without_bvid(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&bilibili_video_source_config("  ", Some(100), 42, false)?)?;

        assert_eq!(
            config,
            json!({
                "type": "video",
                "aid": 100,
                "cid": 42,
            })
        );
        Ok(())
    }

    #[test]
    fn bilibili_video_source_config_rejects_missing_identifiers() {
        let status = bilibili_video_source_config("  ", None, 42, false)
            .expect_err("missing bvid and aid should fail");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "bvid or aid is required");
    }

    #[test]
    fn bilibili_video_source_config_rejects_zero_cid() {
        let status = bilibili_video_source_config("BV1xx411c7mD", None, 0, false)
            .expect_err("zero cid should fail");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "cid must be non-zero");
    }

    #[test]
    fn bilibili_pgc_source_config_builds_pgc_payload() -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&bilibili_pgc_source_config(10, 20, true)?)?;

        assert_eq!(
            config,
            json!({
                "type": "pgc",
                "epid": 10,
                "cid": 20,
                "shared": true,
            })
        );
        Ok(())
    }

    #[test]
    fn bilibili_pgc_source_config_rejects_zero_ids() {
        let status = bilibili_pgc_source_config(0, 20, false).expect_err("zero epid should fail");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "epid must be non-zero");

        let status = bilibili_pgc_source_config(10, 0, false).expect_err("zero cid should fail");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "cid must be non-zero");
    }

    #[test]
    fn bilibili_live_source_config_builds_live_payload() -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&bilibili_live_source_config(123, true)?)?;

        assert_eq!(
            config,
            json!({
                "type": "live",
                "room_id": 123,
                "shared": true,
            })
        );
        Ok(())
    }

    #[test]
    fn bilibili_live_source_config_rejects_zero_room_live_id() {
        let status = bilibili_live_source_config(0, false).expect_err("zero room id should fail");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "room_live_id must be non-zero");
    }

    #[test]
    fn direct_url_source_config_preserves_url() -> Result<(), Box<dyn std::error::Error>> {
        let config = decode_config(&direct_url_source_config(
            "  https://example.test/video.mp4  ",
        )?)?;

        assert_eq!(
            config,
            json!({
                "url": "  https://example.test/video.mp4  ",
            })
        );
        Ok(())
    }
}
