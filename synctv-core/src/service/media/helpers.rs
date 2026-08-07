use crate::{
    models::{Media, MediaId, UserId},
    Error, Result,
};

pub(super) const MAX_BATCH_SIZE: usize = 100;
pub(super) const MEDIA_BATCH_PREPARE_CONCURRENCY: usize = 8;
const MEDIA_BATCH_POSITION_STEP: f64 = 1024.0;

pub(super) fn batch_media_position(index: usize, start_position: f64) -> Result<f64> {
    let index = u32::try_from(index)
        .map_err(|_| Error::InvalidInput("Media batch index exceeds u32::MAX".to_string()))?;
    Ok(MEDIA_BATCH_POSITION_STEP.mul_add(f64::from(index), start_position))
}

pub(super) fn validate_media_name(name: &str) -> Result<()> {
    crate::validation::validate_media_name(name).map_err(|e| Error::InvalidInput(e.to_string()))
}

pub(super) fn ensure_media_creator_can_edit(media: &Media, user_id: &UserId) -> Result<()> {
    if media.creator_id.as_ref() == Some(user_id) {
        Ok(())
    } else {
        Err(Error::Authorization(
            "Only the media creator can edit media".to_string(),
        ))
    }
}

pub(super) fn dedup_media_ids(media_ids: Vec<MediaId>) -> Vec<MediaId> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::with_capacity(media_ids.len());
    for media_id in media_ids {
        if seen.insert(media_id) {
            deduped.push(media_id);
        }
    }
    deduped
}

pub(super) fn media_source_config_error(
    item_name: Option<&str>,
    error: impl std::fmt::Display,
) -> Error {
    match item_name {
        Some(item_name) => Error::InvalidInput(format!(
            "Invalid source_config for item '{item_name}': {error}"
        )),
        None => Error::InvalidInput(format!("Invalid source_config: {error}")),
    }
}

pub(super) fn media_source_prepare_error(
    item_name: Option<&str>,
    error: impl std::fmt::Display,
) -> Error {
    match item_name {
        Some(item_name) => Error::Internal(format!(
            "Failed to prepare source_config for item '{item_name}': {error}"
        )),
        None => Error::Internal(format!("Failed to prepare source_config: {error}")),
    }
}
