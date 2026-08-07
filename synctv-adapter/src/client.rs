use crate::{PublicIdCodec, PublicIdType};
use synctv_proto::client as client_proto;

use crate::{source_config::media_source_config_from_proto, AdapterError, AdapterResult};

pub const DEFAULT_MEDIA_TITLE: &str = "Unknown";

fn proto_id<T>(value: impl AsRef<str>, public_id_codec: &PublicIdCodec) -> AdapterResult<T>
where
    T: PublicIdType,
{
    public_id_codec
        .decode::<T>(value.as_ref().trim())
        .map_err(|error| AdapterError::invalid_input(format!("Invalid {}: {error}", T::TYPE_NAME)))
}

fn optional_playlist_id(
    value: Option<String>,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<Option<synctv_core::models::PlaylistId>> {
    value.map(|id| proto_id(id, public_id_codec)).transpose()
}

fn normalize_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn add_media_request_from_client_proto(
    request: client_proto::AddMediaRequest,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<synctv_core::service::AddMediaRequest> {
    synctv_proto::validate(&request)
        .map_err(|error| AdapterError::invalid_input(error.to_string()))?;
    let client_proto::AddMediaRequest {
        playlist_id,
        provider_instance_name,
        source_config,
        name,
        description,
    } = request;

    let playlist_id = optional_playlist_id(playlist_id, public_id_codec)?;
    let (source_provider, source_config) = media_source_config_from_proto(source_config)?;
    let name = if name.is_empty() {
        DEFAULT_MEDIA_TITLE.to_string()
    } else {
        synctv_core::validation::validate_media_name_input(&name)
            .map_err(|error| AdapterError::invalid_input(format!("Invalid media name: {error}")))?
    };

    Ok(synctv_core::service::AddMediaRequest {
        playlist_id,
        name,
        description,
        source_provider,
        provider_instance_name: normalize_non_empty(&provider_instance_name),
        source_config,
    })
}
