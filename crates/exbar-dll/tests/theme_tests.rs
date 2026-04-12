#[path = "../src/theme.rs"]
mod theme;

#[test]
fn scale_at_100_percent() {
    assert_eq!(theme::scale(24, 96), 24);
}

#[test]
fn scale_at_150_percent() {
    assert_eq!(theme::scale(24, 144), 36);
}

#[test]
fn scale_at_200_percent() {
    assert_eq!(theme::scale(24, 192), 48);
}

#[test]
fn scale_at_125_percent() {
    // 24 * 120 / 96 = 30
    assert_eq!(theme::scale(24, 120), 30);
}

#[test]
fn detect_dark_mode_returns_bool() {
    // Just verify it doesn't crash — actual value depends on system setting
    let _is_dark = theme::is_dark_mode();
}
