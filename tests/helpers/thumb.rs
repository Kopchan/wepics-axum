//! Preview geometry and request parsing.

use wepics_axum::helpers::thumb::{ALLOWED_SIZES, parse_spec, target_box};

#[test]
fn spec_parsing_snaps_sizes_up() {
    assert_eq!(parse_spec("w144").unwrap(), ('w', 144));
    assert_eq!(parse_spec("h200").unwrap(), ('h', 240));
    assert_eq!(parse_spec("q5000").unwrap(), ('q', 1080));
    assert!(parse_spec("w0").is_err());
    assert!(parse_spec("x100").is_err());
    assert!(parse_spec("wabc").is_err());
    assert!(parse_spec("").is_err());
}

#[test]
fn allowed_sizes_are_the_client_steps() {
    assert_eq!(ALLOWED_SIZES, [144, 240, 360, 480, 720, 1080]);
}

#[test]
fn boxes_keep_aspect_and_never_upscale() {
    // 800x600 landscape, width 144 -> 108 high.
    assert_eq!(target_box('w', 144, 800, 600), (144, 108));
    // Height 240 -> 320 wide.
    assert_eq!(target_box('h', 240, 800, 600), (320, 240));
    // Square: cover 144, then crop.
    assert_eq!(target_box('q', 144, 800, 600), (192, 144));
    // Portrait sources cover the other axis.
    assert_eq!(target_box('q', 144, 600, 800), (144, 192));
    // Small originals stay their original size.
    assert_eq!(target_box('w', 1080, 100, 50), (100, 50));
    assert_eq!(target_box('q', 1080, 100, 50), (100, 50));
    // Degenerate metadata never yields a zero-sized box.
    assert_eq!(target_box('w', 144, 0, 0), (0, 1));
}
