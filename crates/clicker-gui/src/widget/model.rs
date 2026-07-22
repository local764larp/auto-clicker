use crate::render::Elevation;

/// Interaction state, orthogonal to focus. Focus is tracked separately by the
/// `FocusRing` and drawn as an accent ring on top, because a widget can be
/// focused *and* hovered/pressed at the same time — folding focus into this
/// enum makes a focused control ignore the mouse.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WidgetState {
    Idle,
    Hover,
    Pressed,
    Disabled,
}

impl WidgetState {
    /// Rendering elevation for this state.
    pub fn elevation(self) -> Elevation {
        match self {
            WidgetState::Pressed => Elevation::Inset,
            WidgetState::Disabled => Elevation::Flat,
            _ => Elevation::Raised,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Space,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Tab,
    Backspace,
    Digit(u8),
    Other,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum WidgetEvent {
    PointerEnter,
    PointerLeave,
    PointerDown { x: f32, y: f32 },
    PointerUp { x: f32, y: f32 },
    FocusGained,
    FocusLost,
    Key(KeyCode),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Fire,
    ValueChanged,
}

/// Pure state transition shared by push-button-like widgets. Widgets with
/// richer behaviour (slider drag, field editing) layer their value math
/// (`value.rs`) on top of this and interpret `Action` themselves.
///
/// Focus events do not change interaction state (focus is orthogonal); they are
/// accepted so the host can route them uniformly. Keyboard fire works from any
/// non-disabled state because a keyboard-focused widget is usually `Idle`, not
/// `Hover`.
pub fn transition(state: WidgetState, ev: WidgetEvent) -> (WidgetState, Option<Action>) {
    use WidgetEvent::*;
    if state == WidgetState::Disabled {
        return (WidgetState::Disabled, None); // only a host call re-enables
    }
    match (state, ev) {
        (WidgetState::Idle, PointerEnter) => (WidgetState::Hover, None),
        (WidgetState::Hover, PointerLeave) => (WidgetState::Idle, None),
        // A press registers even without a prior hover (a synthesized click, or
        // focus-then-click, never sends PointerEnter first).
        (WidgetState::Idle, PointerDown { .. }) | (WidgetState::Hover, PointerDown { .. }) => {
            (WidgetState::Pressed, None)
        }
        (WidgetState::Pressed, PointerUp { .. }) => (WidgetState::Hover, Some(Action::Fire)),
        // Drag-off: pointer left while pressed. A later release must not fire.
        (WidgetState::Pressed, PointerLeave) => (WidgetState::Idle, None),
        // Keyboard activation fires from any non-disabled state; focus is
        // tracked externally, so a focused-but-unhovered widget is `Idle` here.
        (_, Key(KeyCode::Space)) | (_, Key(KeyCode::Enter)) => (state, Some(Action::Fire)),
        // Focus events are orthogonal to interaction state.
        (_, FocusGained) | (_, FocusLost) => (state, None),
        _ => (state, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Elevation;

    #[test]
    fn hover_enter_and_leave() {
        assert_eq!(transition(WidgetState::Idle, WidgetEvent::PointerEnter).0, WidgetState::Hover);
        assert_eq!(transition(WidgetState::Hover, WidgetEvent::PointerLeave).0, WidgetState::Idle);
    }

    #[test]
    fn press_then_release_fires() {
        let (s, a) = transition(WidgetState::Hover, WidgetEvent::PointerDown { x: 1.0, y: 1.0 });
        assert_eq!(s, WidgetState::Pressed);
        assert_eq!(a, None);
        let (s, a) = transition(s, WidgetEvent::PointerUp { x: 1.0, y: 1.0 });
        assert_eq!(s, WidgetState::Hover);
        assert_eq!(a, Some(Action::Fire));
    }

    #[test]
    fn release_after_leaving_does_not_fire() {
        let (s, _) = transition(WidgetState::Hover, WidgetEvent::PointerDown { x: 1.0, y: 1.0 });
        let (s, _) = transition(s, WidgetEvent::PointerLeave);
        let (s, a) = transition(s, WidgetEvent::PointerUp { x: 1.0, y: 1.0 });
        assert_eq!(a, None, "releasing after drag-off must not fire");
        assert_eq!(s, WidgetState::Idle);
    }

    #[test]
    fn click_without_prior_hover_still_fires() {
        // A synthesized click, or a focus-then-click, sends PointerDown from
        // Idle with no PointerEnter first. It must still press and fire.
        let (s, a) = transition(WidgetState::Idle, WidgetEvent::PointerDown { x: 1.0, y: 1.0 });
        assert_eq!(s, WidgetState::Pressed);
        assert_eq!(a, None);
        let (_, a) = transition(s, WidgetEvent::PointerUp { x: 1.0, y: 1.0 });
        assert_eq!(a, Some(Action::Fire));
    }

    #[test]
    fn space_and_enter_fire_from_any_non_disabled_state() {
        for st in [WidgetState::Idle, WidgetState::Hover, WidgetState::Pressed] {
            for k in [KeyCode::Space, KeyCode::Enter] {
                let (s, a) = transition(st, WidgetEvent::Key(k));
                assert_eq!(a, Some(Action::Fire), "{st:?} + {k:?} should fire");
                assert_eq!(s, st, "keyboard fire must not change interaction state");
            }
        }
    }

    #[test]
    fn disabled_ignores_everything() {
        for ev in [
            WidgetEvent::PointerEnter,
            WidgetEvent::PointerDown { x: 0.0, y: 0.0 },
            WidgetEvent::Key(KeyCode::Space),
        ] {
            assert_eq!(transition(WidgetState::Disabled, ev), (WidgetState::Disabled, None));
        }
    }

    #[test]
    fn focus_events_do_not_change_interaction_state() {
        // Focus is orthogonal, tracked by the FocusRing, not this enum.
        assert_eq!(
            transition(WidgetState::Hover, WidgetEvent::FocusGained),
            (WidgetState::Hover, None)
        );
        assert_eq!(
            transition(WidgetState::Pressed, WidgetEvent::FocusLost),
            (WidgetState::Pressed, None)
        );
    }

    #[test]
    fn state_maps_to_elevation() {
        assert_eq!(WidgetState::Pressed.elevation(), Elevation::Inset);
        assert_eq!(WidgetState::Disabled.elevation(), Elevation::Flat);
        assert_eq!(WidgetState::Idle.elevation(), Elevation::Raised);
        assert_eq!(WidgetState::Hover.elevation(), Elevation::Raised);
    }
}
