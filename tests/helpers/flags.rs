//! Bare query flags.

use wepics_axum::helpers::flags::flag;

#[test]
fn bare_flags_mean_true_and_falsey_values_mean_false() {
    assert!(!flag(None));
    assert!(flag(Some("")));
    assert!(flag(Some("1")));
    assert!(flag(Some("true")));
    assert!(!flag(Some("0")));
    assert!(!flag(Some("FALSE")));
    assert!(!flag(Some("no")));
    assert!(!flag(Some("off")));
}
