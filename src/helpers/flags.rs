//! Bare query flags such as `?force`.

/// Interprets the value of a bare flag.
///
/// `?force` reaches the handler as an empty string, while `?force=0` and
/// `?force=false` explicitly disable it. Anything else means `true`.
pub fn flag(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
    }
}
