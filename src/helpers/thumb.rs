//! Preview geometry: request parsing and resampling boxes.

use std::path::Path;

use vips::{VipsImage, VipsInteresting, VipsSize};

use crate::error::AppError;

/// Sizes exposed by the client.
pub const ALLOWED_SIZES: [u32; 6] = [144, 240, 360, 480, 720, 1080];

/// WebP encoder settings applied to every preview.
pub const WEBP_SUFFIX: &str = ".webp[Q=90]";

/// Upper limit 
const UNCONSTRAINED: u32 = 10_000_000;

/// Parses `w480` / `q240`; sizes between the allowed steps are snapped up,
/// matching Laravel's redirect behaviour without an extra round-trip.
pub fn parse_spec(spec: &str) -> Result<(char, u32), AppError> {
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

/// Decodes, resizes and encodes one preview. Runs on a blocking thread because
/// libvips decoding and resampling are CPU-heavy.
pub fn render(
    source: &Path,
    direction: char,
    size: u32,
    source_width: i64,
    source_height: i64,
) -> Result<Vec<u8>, String> {
    crate::config::vips::ensure()?;

    let (source_width, source_height) = known_dimensions(source, source_width, source_height)?;
    let (width, height) = target_box(direction, size, source_width, source_height);

    let image = VipsImage::thumbnail_file(source, width, height, VipsSize::VIPS_SIZE_DOWN)
        .map_err(|err| format!("cannot render `{}`: {err}", source.display()))?;

    let image = if direction == 'q' {
        center_crop(image, size)?
    } else {
        image
    };

    image
        .write_to_buffer(WEBP_SUFFIX)
        .map_err(|err| format!("cannot encode webp: {err}"))
}

/// Returns the source size, falling back to a header-only read when the index
/// has no dimensions (e.g. rows written before metadata probing).
fn known_dimensions(source: &Path, width: i64, height: i64) -> Result<(u32, u32), String> {
    if width > 0 && height > 0 {
        let width = u32::try_from(width).map_err(|_| "invalid source width".to_string())?;
        let height = u32::try_from(height).map_err(|_| "invalid source height".to_string())?;
        return Ok((width, height));
    }

    let image = VipsImage::from_file(source)
        .map_err(|err| format!("cannot read `{}`: {err}", source.display()))?;
    Ok((image.width().max(1), image.height().max(1)))
}

/// Box passed to `vips_thumbnail`. `DOWN` keeps small originals untouched, so
/// previews never upscale.
pub fn target_box(direction: char, size: u32, width: u32, height: u32) -> (u32, u32) {
    match direction {
        'w' => (size.min(width), UNCONSTRAINED),
        'h' => (UNCONSTRAINED, size.min(height)),
        // Square preview: cover the box, then crop it in the centre.
        _ => {
            let shorter = width.min(height).max(1);
            let target = size.min(shorter);
            (
                scaled(width, target, shorter),
                scaled(height, target, shorter),
            )
        }
    }
}

fn scaled(value: u32, target: u32, total: u32) -> u32 {
    if total == 0 {
        return 1;
    }
    ((u64::from(value) * u64::from(target)) / u64::from(total)).max(1) as u32
}

/// Crops the cover-sized preview to a centre square. Images smaller than the
/// requested size are returned as is, matching the no-upscale rule.
fn center_crop(image: VipsImage<'static>, size: u32) -> Result<VipsImage<'static>, String> {
    if image.width() < size || image.height() < size {
        return Ok(image);
    }

    image
        .smartcrop(
            size as i32,
            size as i32,
            Some(VipsInteresting::VIPS_INTERESTING_CENTRE),
        )
        .map_err(|err| format!("cannot crop preview: {err}"))
}
