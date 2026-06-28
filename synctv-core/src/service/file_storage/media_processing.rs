use std::sync::Arc;

use crate::{
    models::{
        FileMetadata, FileObjectVariant, FileUploadPolicy, FileVariantMetadata, NewStoredFile,
    },
    repository::{FileStorageRepository, UpsertFileObjectGroup, UpsertFileObjectVariant},
    service::file_storage::{validation::validate_file_dimensions, FileObjectReader},
    Error, Result,
};
use image::{codecs::jpeg::JpegEncoder, DynamicImage, GenericImageView, ImageReader};

use super::{payload_len_i64, FileStorageService};

const IMAGE_VARIANT_MIME_TYPE: &str = "image/jpeg";
const IMAGE_VARIANT_QUALITY: u8 = 78;
const MIN_IMAGE_VARIANT_SAVINGS_PERCENT: i64 = 12;
const MAX_INLINE_PROCESSING_IMAGE_BYTES: i64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct ImageVariantSpec {
    key: &'static str,
    label: &'static str,
    max_edge: u32,
    sort_order: i32,
}

const IMAGE_VARIANT_SPECS: &[ImageVariantSpec] = &[
    ImageVariantSpec {
        key: "thumb",
        label: "Thumbnail",
        max_edge: 320,
        sort_order: 10,
    },
    ImageVariantSpec {
        key: "small",
        label: "Small",
        max_edge: 720,
        sort_order: 20,
    },
    ImageVariantSpec {
        key: "medium",
        label: "Medium",
        max_edge: 1280,
        sort_order: 30,
    },
];

#[derive(Debug, Clone)]
pub struct ProcessedFileVariants {
    pub variants: Vec<FileObjectVariant>,
}

pub(crate) async fn process_file_variants_for_object(
    storage: &dyn FileStorageService,
    repository: Arc<FileStorageRepository>,
    storage_backend: &str,
    object_key: &str,
    database_object_route_prefix: &str,
    upload_policy: &FileUploadPolicy,
) -> Result<ProcessedFileVariants> {
    let Some(mut object) = repository.get_object(storage_backend, object_key).await? else {
        return Err(Error::NotFound("File object not found".to_string()));
    };
    let media_kind = media_kind_from_mime_type(&object.mime_type);
    let mut decoded_image = None;
    if media_kind == "image" {
        let image = if object.size_bytes <= MAX_INLINE_PROCESSING_IMAGE_BYTES {
            let reader = storage
                .get_object_reader_by_key(storage_backend, object_key)
                .await?;
            let image = decode_image(reader).await?;
            let (width, height) = image.dimensions();
            decoded_image = Some(image);
            (width, height)
        } else {
            let reader = storage
                .get_object_reader_by_key(storage_backend, object_key)
                .await?;
            probe_image_dimensions(reader).await?
        };
        let (original_width, original_height) = image;
        let original_width_i32 = i32::try_from(original_width)
            .map_err(|_| Error::InvalidInput("image width exceeds supported limit".to_string()))?;
        let original_height_i32 = i32::try_from(original_height)
            .map_err(|_| Error::InvalidInput("image height exceeds supported limit".to_string()))?;
        validate_file_dimensions(
            upload_policy,
            &object.mime_type,
            Some(original_width_i32),
            Some(original_height_i32),
        )?;
        object.metadata.width = Some(original_width_i32);
        object.metadata.height = Some(original_height_i32);
        repository
            .update_object_metadata(storage_backend, object_key, &object.metadata)
            .await?;
    }
    let group_id = format!("fg_{}", synctv_common::snanoid!(24));
    let group = repository
        .upsert_object_group(UpsertFileObjectGroup {
            id: &group_id,
            storage_backend,
            original_object_key: object_key,
            media_kind,
            metadata: &object.metadata,
        })
        .await?;
    let group_id = group.id;

    let original_url =
        storage.object_url(storage_backend, object_key, database_object_route_prefix)?;
    repository
        .upsert_object_variant(UpsertFileObjectVariant {
            storage_backend,
            object_key,
            original_storage_backend: storage_backend,
            original_object_key: object_key,
            group_id: &group_id,
            variant_key: "original",
            label: "Original",
            url: original_url.as_deref(),
            mime_type: &object.mime_type,
            size_bytes: object.size_bytes,
            width: object.metadata.width,
            height: object.metadata.height,
            is_original: true,
            lossy: false,
            quality: None,
            sort_order: 1000,
            metadata: &FileVariantMetadata {
                width: object.metadata.width,
                height: object.metadata.height,
                blurhash: object.metadata.blurhash.clone(),
            },
        })
        .await?;

    if let Some(image) = decoded_image {
        let context = ImageVariantProcessingContext {
            storage,
            repository: repository.as_ref(),
            storage_backend,
            object_key,
            database_object_route_prefix,
            group_id: &group_id,
            original_size_bytes: object.size_bytes,
        };
        process_image_variants(context, &image).await?;
    }

    let variants = repository
        .list_object_variants(storage_backend, object_key)
        .await?;
    Ok(ProcessedFileVariants { variants })
}

pub(crate) async fn attach_variants_to_files(
    storage: &dyn FileStorageService,
    repository: &FileStorageRepository,
    files: &mut [NewStoredFile],
    database_object_route_prefix: &str,
) -> Result<()> {
    for file in files {
        let variants = object_variants_with_urls(
            storage,
            repository,
            &file.storage_backend,
            &file.object_key,
            database_object_route_prefix,
        )
        .await?;
        if let Some(preview) = preferred_preview_variant(&variants) {
            file.url.clone_from(&preview.url);
        }
        attach_variants_metadata(&mut file.metadata, &variants);
    }
    Ok(())
}

pub(crate) async fn attach_variants_to_chat_attachments(
    storage: &dyn FileStorageService,
    repository: &FileStorageRepository,
    attachments: &mut [crate::models::ChatAttachment],
    database_object_route_prefix: &str,
) -> Result<()> {
    for attachment in attachments {
        let variants = object_variants_with_urls(
            storage,
            repository,
            &attachment.storage_backend,
            &attachment.object_key,
            database_object_route_prefix,
        )
        .await?;
        if let Some(preview) = preferred_preview_variant(&variants) {
            attachment.url.clone_from(&preview.url);
        }
        attach_variants_metadata(&mut attachment.metadata, &variants);
    }
    Ok(())
}

async fn object_variants_with_urls(
    storage: &dyn FileStorageService,
    repository: &FileStorageRepository,
    storage_backend: &str,
    object_key: &str,
    database_object_route_prefix: &str,
) -> Result<Vec<FileObjectVariant>> {
    let mut variants = repository
        .list_object_variants(storage_backend, object_key)
        .await?;
    for variant in &mut variants {
        if variant.url.as_deref().is_none_or(str::is_empty) {
            variant.url = storage.object_url(
                &variant.storage_backend,
                &variant.object_key,
                database_object_route_prefix,
            )?;
        }
    }
    Ok(variants)
}

fn attach_variants_metadata(metadata: &mut FileMetadata, variants: &[FileObjectVariant]) {
    metadata.variants = variants.to_vec();
}

fn preferred_preview_variant(variants: &[FileObjectVariant]) -> Option<&FileObjectVariant> {
    variants
        .iter()
        .find(|variant| {
            !variant.is_original && variant.url.as_deref().is_some_and(|url| !url.is_empty())
        })
        .or_else(|| {
            variants.iter().find(|variant| {
                variant.is_original && variant.url.as_deref().is_some_and(|url| !url.is_empty())
            })
        })
}

struct ImageVariantProcessingContext<'a> {
    storage: &'a dyn FileStorageService,
    repository: &'a FileStorageRepository,
    storage_backend: &'a str,
    object_key: &'a str,
    database_object_route_prefix: &'a str,
    group_id: &'a str,
    original_size_bytes: i64,
}

async fn process_image_variants(
    context: ImageVariantProcessingContext<'_>,
    image: &DynamicImage,
) -> Result<()> {
    let (original_width, original_height) = image.dimensions();
    for spec in IMAGE_VARIANT_SPECS {
        let Some((width, height)) =
            scaled_dimensions(original_width, original_height, spec.max_edge)
        else {
            continue;
        };
        let variant = image.resize(width, height, image::imageops::FilterType::Lanczos3);
        let encoded = encode_jpeg(&variant, IMAGE_VARIANT_QUALITY)?;
        let size_bytes = payload_len_i64(encoded.len())?;
        if !compression_is_useful(context.original_size_bytes, size_bytes) {
            continue;
        }
        let variant_object_key = variant_object_key(context.object_key, spec.key);
        let variant_width = i32::try_from(width)
            .map_err(|_| Error::Internal("image variant width exceeds i32::MAX".to_string()))?;
        let variant_height = i32::try_from(height)
            .map_err(|_| Error::Internal("image variant height exceeds i32::MAX".to_string()))?;
        context
            .storage
            .put_object_by_key(
                context.storage_backend,
                &variant_object_key,
                IMAGE_VARIANT_MIME_TYPE,
                encoded,
                FileMetadata {
                    width: Some(variant_width),
                    height: Some(variant_height),
                    ..Default::default()
                },
            )
            .await?;
        let url = context.storage.object_url(
            context.storage_backend,
            &variant_object_key,
            context.database_object_route_prefix,
        )?;
        context
            .repository
            .upsert_object_variant(UpsertFileObjectVariant {
                storage_backend: context.storage_backend,
                object_key: &variant_object_key,
                original_storage_backend: context.storage_backend,
                original_object_key: context.object_key,
                group_id: context.group_id,
                variant_key: spec.key,
                label: spec.label,
                url: url.as_deref(),
                mime_type: IMAGE_VARIANT_MIME_TYPE,
                size_bytes,
                width: Some(variant_width),
                height: Some(variant_height),
                is_original: false,
                lossy: true,
                quality: Some(i32::from(IMAGE_VARIANT_QUALITY)),
                sort_order: spec.sort_order,
                metadata: &FileVariantMetadata {
                    width: Some(variant_width),
                    height: Some(variant_height),
                    blurhash: None,
                },
            })
            .await?;
    }
    Ok(())
}

async fn probe_image_dimensions(reader: FileObjectReader) -> Result<(u32, u32)> {
    tokio::task::spawn_blocking(move || {
        let reader = tokio_util::io::SyncIoBridge::new(reader);
        ImageReader::new(std::io::BufReader::new(reader))
            .with_guessed_format()
            .map_err(|error| Error::InvalidInput(format!("invalid image data: {error}")))?
            .into_dimensions()
            .map_err(|error| {
                Error::InvalidInput(format!("unsupported or invalid image data: {error}"))
            })
    })
    .await
    .map_err(|error| Error::Internal(format!("image dimension task failed: {error}")))?
}

async fn decode_image(reader: FileObjectReader) -> Result<DynamicImage> {
    tokio::task::spawn_blocking(move || {
        let reader = tokio_util::io::SyncIoBridge::new(reader);
        ImageReader::new(std::io::BufReader::new(reader))
            .with_guessed_format()
            .map_err(|error| Error::InvalidInput(format!("invalid image data: {error}")))?
            .decode()
            .map_err(|error| {
                Error::InvalidInput(format!("unsupported or invalid image data: {error}"))
            })
    })
    .await
    .map_err(|error| Error::Internal(format!("image decode task failed: {error}")))?
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let rgb = image.to_rgb8();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    encoder
        .encode_image(&rgb)
        .map_err(|error| Error::Internal(format!("failed to encode image variant: {error}")))?;
    Ok(out)
}

fn scaled_dimensions(width: u32, height: u32, max_edge: u32) -> Option<(u32, u32)> {
    let longest = width.max(height);
    if longest <= max_edge || width == 0 || height == 0 {
        return None;
    }
    let width64 = u64::from(width);
    let height64 = u64::from(height);
    let longest64 = u64::from(longest);
    let max64 = u64::from(max_edge);
    let scaled_width = ((width64 * max64) / longest64).max(1);
    let scaled_height = ((height64 * max64) / longest64).max(1);
    Some((
        u32::try_from(scaled_width).ok()?,
        u32::try_from(scaled_height).ok()?,
    ))
}

fn compression_is_useful(original_size_bytes: i64, variant_size_bytes: i64) -> bool {
    variant_size_bytes < original_size_bytes
        && savings_percent(original_size_bytes, variant_size_bytes)
            >= MIN_IMAGE_VARIANT_SAVINGS_PERCENT
}

fn savings_percent(original_size_bytes: i64, variant_size_bytes: i64) -> i64 {
    if original_size_bytes <= 0 || variant_size_bytes >= original_size_bytes {
        return 0;
    }
    ((original_size_bytes - variant_size_bytes) * 100) / original_size_bytes
}

fn media_kind_from_mime_type(mime_type: &str) -> &'static str {
    let mime_type = mime_type.trim().to_ascii_lowercase();
    if mime_type.starts_with("image/") {
        "image"
    } else if mime_type.starts_with("audio/") {
        "audio"
    } else if mime_type.starts_with("video/") {
        "video"
    } else {
        "file"
    }
}

fn variant_object_key(original_object_key: &str, variant_key: &str) -> String {
    let original = original_object_key.trim_end_matches('/');
    match original.rsplit_once('.') {
        Some((stem, _)) => format!("{stem}.{variant_key}.jpg"),
        None => format!("{original}.{variant_key}.jpg"),
    }
}
