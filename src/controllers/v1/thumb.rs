//! `GET /thumb/v1/{album}/{media}/{direction}{size}` — generated WebP previews.

use std::io::Cursor;
use std::path::Path as FsPath;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageEncoder};

use crate::controllers::album::resolve_album;
use crate::error::AppError;
use crate::models::{Media, MediaType};
use crate::state::AppState;

use super::secure::verify_link;
use super::types::{LinkQuery, MEDIA_BY_HASH};

/// Sizes exposed by the client.
const ALLOWED_SIZES: [u32; 6] = [144, 240, 360, 480, 720, 1080];

/// `GET /thumb/v1/{album_hash}/{media_hash}/{spec}?hash=&exp=`.
pub async fn get_thumb(
    State(state): State<AppState>,
    Path((album_hash, media_hash, spec)): Path<(String, String, String)>,
    Query(query): Query<LinkQuery>,
) -> Result<Response, AppError> {
    verify_link(&state, &album_hash, &media_hash, &query)?;

    let (direction, size) = parse_spec(&spec)?;
    let album = resolve_album(&state, &album_hash).await?;
    let media = load_media(&state, album.id, &media_hash).await?;

    if !matches!(media.r#type, MediaType::Image | MediaType::Animated) {
        return Err(AppError::BadRequest(
            "previews are only available for images".into(),
        ));
    }

    let source = state.media_path(&album, &media).ok_or(AppError::NotFound)?;
    if !tokio::fs::metadata(&source).await?.is_file() {
        return Err(AppError::NotFound);
    }

    let cache_path = state
        .thumb_dir()
        .join(format!("{direction}{size}"))
        .join(format!("{}.webp", media.hash));

    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        return Ok(webp_response(bytes));
    }

    let bytes = generate(&source, direction, size).await?;

    if let Some(parent) = cache_path.parent() {
        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            eprintln!(
                "cannot create thumbnail directory `{}`: {err}",
                parent.display()
            );
        } else if let Err(err) = tokio::fs::write(&cache_path, &bytes).await {
            eprintln!("cannot cache thumbnail `{}`: {err}", cache_path.display());
        }
    }

    Ok(webp_response(bytes))
}

async fn load_media(state: &AppState, album_id: i64, media_hash: &str) -> Result<Media, AppError> {
    sqlx::query_as(MEDIA_BY_HASH)
        .bind(album_id)
        .bind(media_hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

/// Parses `w480` / `q240`; sizes between the allowed steps are snapped up,
/// matching Laravel's redirect behaviour without an extra round-trip.
fn parse_spec(spec: &str) -> Result<(char, u32), AppError> {
    let mut chars = spec.chars();
    let direction = chars
        .next()
        .ok_or_else(|| AppError::BadRequest("empty preview specifier".into()))?;
    if !matches!(direction, 'w' | 'h' | 'q') {
        return Err(AppError::BadRequest(
            "preview direction must be w, h or q".into(),
        ));
    }

    let requested: u32 = chars
        .as_str()
        .parse()
        .map_err(|_| AppError::BadRequest("invalid preview size".into()))?;
    if requested == 0 {
        return Err(AppError::BadRequest("preview size must be positive".into()));
    }

    let size = ALLOWED_SIZES
        .iter()
        .copied()
        .find(|allowed| *allowed >= requested)
        .unwrap_or_else(|| *ALLOWED_SIZES.last().expect("sizes are not empty"));

    Ok((direction, size))
}

async fn generate(source: &FsPath, direction: char, size: u32) -> Result<Vec<u8>, AppError> {
    let source = source.to_path_buf();
    tokio::task::spawn_blocking(move || render(&source, direction, size))
        .await
        .map_err(|err| AppError::Internal(format!("thumbnail worker failed: {err}")))?
        .map_err(AppError::Internal)
}

/// Decodes, orients, resizes and encodes one preview. Runs on a blocking
/// thread because image decoding and Lanczos3 resampling are CPU-heavy.
fn render(source: &FsPath, direction: char, size: u32) -> Result<Vec<u8>, String> {
    let image = image::open(source)
        .map_err(|err| format!("cannot decode `{}`: {err}", source.display()))?;
    let image = apply_exif_orientation(source, image);
    let image = resize(image, direction, size);

    let (width, height) = image.dimensions();
    let rgb = image.to_rgb8();
    let mut output = Cursor::new(Vec::new());
    image::codecs::webp::WebPEncoder::new_lossless(&mut output)
        .write_image(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
        .map_err(|err| format!("cannot encode webp: {err}"))?;

    Ok(output.into_inner())
}

fn resize(image: DynamicImage, direction: char, size: u32) -> DynamicImage {
    let (width, height) = image.dimensions();

    match direction {
        'w' if width > size => {
            let target_height = scaled(height, size, width);
            image.resize_exact(size, target_height, FilterType::Lanczos3)
        }
        'h' if height > size => {
            let target_width = scaled(width, size, height);
            image.resize_exact(target_width, size, FilterType::Lanczos3)
        }
        'q' => cover_down(image, size),
        _ => image,
    }
}

fn scaled(value: u32, target: u32, total: u32) -> u32 {
    ((u64::from(value) * u64::from(target)) / u64::from(total)).max(1) as u32
}

/// Resizes to cover a square and center-crops it, without upscaling.
fn cover_down(image: DynamicImage, size: u32) -> DynamicImage {
    let (width, height) = image.dimensions();
    let scale = (f64::from(size) / f64::from(width))
        .max(f64::from(size) / f64::from(height))
        .min(1.0);

    let target_width = (f64::from(width) * scale).round().max(1.0) as u32;
    let target_height = (f64::from(height) * scale).round().max(1.0) as u32;
    let resized = image.resize_exact(target_width, target_height, FilterType::Lanczos3);

    let side = target_width.min(target_height).min(size);
    if side == 0 {
        return resized;
    }

    let x = (target_width - side) / 2;
    let y = (target_height - side) / 2;
    resized.crop_imm(x, y, side, side)
}

/// Applies the EXIF orientation flag, so previews match what browsers show for
/// the original file.
fn apply_exif_orientation(path: &FsPath, image: DynamicImage) -> DynamicImage {
    let Ok(file) = std::fs::File::open(path) else {
        return image;
    };

    let mut reader = std::io::BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return image;
    };
    let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) else {
        return image;
    };
    let Some(orientation) = field.value.get_uint(0) else {
        return image;
    };

    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

fn webp_response(bytes: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/webp")
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=86400, immutable"),
        )
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes))
        .expect("static response parts are valid")
}

#[cfg(test)]
mod tests {
    use super::{ALLOWED_SIZES, parse_spec, scaled};

    #[test]
    fn spec_parsing_snaps_sizes_up() {
        assert_eq!(parse_spec("w144").unwrap(), ('w', 144));
        assert_eq!(parse_spec("h200").unwrap(), ('h', 240));
        assert_eq!(parse_spec("q5000").unwrap(), ('q', 1080));
        assert!(parse_spec("w0").is_err());
        assert!(parse_spec("x100").is_err());
        assert!(parse_spec("wabc").is_err());
    }

    #[test]
    fn aspect_ratio_rounds_up_to_at_least_one_pixel() {
        assert_eq!(scaled(1, 10, 1000), 1);
        assert_eq!(scaled(600, 480, 800), 360);
        assert_eq!(ALLOWED_SIZES, [144, 240, 360, 480, 720, 1080]);
    }
}
