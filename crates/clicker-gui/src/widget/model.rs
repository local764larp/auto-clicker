use crate::render::Elevation;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WidgetState {
    Idle,
    Hover,
    Pressed,
    Focused,
    Disabled,
}

impl WidgetState {
    /// Rendering elevation for this state. Focus is drawn as an accent ring on
    /// top, so a focused control still reads as raised.
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
pub fn transition(state: WidgetState, ev: WidgetEvent) -> (WidgetState, Option<Action>) {
    use WidgetEvent::*;
    if state == WidgetState::Disabled {
        return (WidgetState::Disabled, None); // only a host call re-enables
    }
    match (state, ev) {
        (WidgetState::Idle, PointerEnter) => (WidgetState::Hover, None),
        (WidgetState::Hover, PointerLeave) => (WidgetState::Idle, None),
        (WidgetState::Hover, PointerDown { .. }) => (WidgetState::Pressed, None),
        (WidgetState::Pressed, PointerUp { .. }) => (WidgetState::Hover, Some(Action::Fire)),
        // Drag-off: pointer left while pressed. A later release must not fire.
        (WidgetState::Pressed, PointerLeave) => (WidgetState::Idle, None),
        (WidgetState::Idle, FocusGained) | (WidgetState::Hover, FocusGained) => {
            (WidgetState::Focused, None)
        }
        (WidgetState::Focused, FocusLost) => (WidgetState::Idle, None),
        (WidgetState::Focused, Key(KeyCode::Space))
        | (WidgetState::Focused, Key(KeyCode::Enter)) => {
            (WidgetState::Focused, Some(Action::Fire))
        }
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
    fn space_and_enter_fire_when_focused() {
        for k in [KeyCode::Space, KeyCode::Enter] {
            let (s, a) = transition(WidgetState::Focused, WidgetEvent::Key(k));
            assert_eq!(a, Some(Action::Fire));
            assert_eq!(s, WidgetState::Focused);
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
    fn focus_gained_and_lost() {
        assert_eq!(
            transition(WidgetState::Idle, WidgetEvent::FocusGained).0,
            WidgetState::Focused
        );
        assert_eq!(
            transition(WidgetState::Focused, WidgetEvent::FocusLost).0,
            WidgetState::Idle
        );
    }

    #[test]
    fn state_maps_to_elevation() {
        assert_eq!(WidgetState::Pressed.elevation(), Elevation::Inset);
        assert_eq!(WidgetState::Disabled.elevation(), Elevation::Flat);
        assert_eq!(WidgetState::Idle.elevation(), Elevation::Raised);
        assert_eq!(WidgetState::Hover.elevation(), Elevation::Raised);
        assert_eq!(WidgetState::Focused.elevation(), Elevation::Raised);
    }
}
