//! Pure point-in-button hit testing.

use crate::layout::ButtonLayout;

/// Returns the index of the first button whose rect contains `(x, y)`, or
/// `None` if no button is hit.
///
/// Coordinates are in toolbar-client physical pixels.
pub fn hit_test(buttons: &[ButtonLayout], x: i32, y: i32) -> Option<usize> {
    buttons.iter().position(|b| b.rect.contains(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FolderEntry;
    use crate::layout::Rect;
    use proptest::prelude::*;

    fn mk_button(rect: Rect) -> ButtonLayout {
        ButtonLayout {
            rect,
            folder: FolderEntry { name: "x".into(), path: "p".into(), icon: None },
            is_add: false,
        }
    }

    #[test]
    fn empty_buttons_returns_none() {
        assert_eq!(hit_test(&[], 0, 0), None);
    }

    #[test]
    fn point_inside_button_returns_index() {
        let buttons = [
            mk_button(Rect { left: 0,  top: 0, right: 50,  bottom: 28 }),
            mk_button(Rect { left: 60, top: 0, right: 110, bottom: 28 }),
        ];
        assert_eq!(hit_test(&buttons, 25, 14), Some(0));
        assert_eq!(hit_test(&buttons, 80, 14), Some(1));
    }

    #[test]
    fn point_outside_all_buttons_returns_none() {
        let buttons = [
            mk_button(Rect { left: 0, top: 0, right: 50, bottom: 28 }),
        ];
        assert_eq!(hit_test(&buttons, 100, 100), None);
    }

    #[test]
    fn top_left_corner_inclusive() {
        let buttons = [mk_button(Rect { left: 10, top: 20, right: 50, bottom: 60 })];
        assert_eq!(hit_test(&buttons, 10, 20), Some(0));
    }

    #[test]
    fn right_edge_exclusive() {
        let buttons = [mk_button(Rect { left: 10, top: 20, right: 50, bottom: 60 })];
        assert_eq!(hit_test(&buttons, 50, 30), None);
    }

    #[test]
    fn bottom_edge_exclusive() {
        let buttons = [mk_button(Rect { left: 10, top: 20, right: 50, bottom: 60 })];
        assert_eq!(hit_test(&buttons, 30, 60), None);
    }

    #[test]
    fn point_in_gap_between_buttons_returns_none() {
        let buttons = [
            mk_button(Rect { left: 0,  top: 0, right: 50,  bottom: 28 }),
            mk_button(Rect { left: 60, top: 0, right: 110, bottom: 28 }),
        ];
        assert_eq!(hit_test(&buttons, 55, 14), None);
    }

    #[test]
    fn returns_first_overlapping_button_if_overlapping() {
        let buttons = [
            mk_button(Rect { left: 0,  top: 0, right: 50, bottom: 28 }),
            mk_button(Rect { left: 20, top: 0, right: 80, bottom: 28 }),
        ];
        assert_eq!(hit_test(&buttons, 30, 14), Some(0));
    }

    proptest! {
        #[test]
        fn hit_test_result_is_consistent_with_rect_contains(
            rects in prop::collection::vec(
                (0i32..=500, 0i32..=500, 1i32..=200, 1i32..=100)
                    .prop_map(|(l, t, w, h)| Rect { left: l, top: t, right: l + w, bottom: t + h }),
                0..10,
            ),
            x in 0i32..=1000,
            y in 0i32..=1000,
        ) {
            let buttons: Vec<ButtonLayout> = rects.iter().map(|r| mk_button(*r)).collect();
            let result = hit_test(&buttons, x, y);
            match result {
                Some(i) => {
                    prop_assert!(buttons[i].rect.contains(x, y));
                    // Must be the FIRST button to contain the point.
                    for (earlier_idx, earlier) in buttons.iter().enumerate().take(i) {
                        prop_assert!(!earlier.rect.contains(x, y), "earlier button #{} also contains", earlier_idx);
                    }
                }
                None => {
                    for b in &buttons {
                        prop_assert!(!b.rect.contains(x, y));
                    }
                }
            }
        }
    }
}
