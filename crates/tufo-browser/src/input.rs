//! Turns a view's pointer and keys into the events Chromium takes for a page without a window.

use cef::{KeyEvent, KeyEventType, MouseButtonType, MouseEvent, sys::cef_event_flags_t};
use tufo_wire::browser::{KeyInput, KeyModifiers, MouseButton, ViewInput, ViewPoint};

/// The DOM keyCode of Return, whose press also types a carriage return.
const RETURN_KEY_CODE: u32 = 0x0D;
const CARRIAGE_RETURN: u16 = 0x0D;

/// One call to make on the page's browser host.
#[derive(Debug, Clone)]
pub(crate) enum PageEvent {
    MouseMove {
        event: MouseEvent,
        leaves: bool,
    },
    MouseClick {
        event: MouseEvent,
        button: MouseButtonType,
        released: bool,
        click_count: i32,
    },
    MouseWheel {
        event: MouseEvent,
        delta_x: i32,
        delta_y: i32,
    },
    Key(KeyEvent),
    Focus(bool),
    CommitText(String),
}

/// Remembers which buttons are held, since Chromium reads drags from moves that carry them.
#[derive(Debug, Default)]
pub(crate) struct InputTranslator {
    held_buttons: u32,
    last_position: ViewPoint,
}

impl InputTranslator {
    /// The events for one input; resizing is the view's geometry, not a page event.
    pub(crate) fn translate(&mut self, input: ViewInput) -> Vec<PageEvent> {
        match input {
            ViewInput::Resize(_) => Vec::new(),
            ViewInput::PointerMoved {
                position,
                modifiers,
            } => {
                self.last_position = position;
                vec![PageEvent::MouseMove {
                    event: self.mouse_event(position, modifiers),
                    leaves: false,
                }]
            }
            ViewInput::PointerLeft => vec![PageEvent::MouseMove {
                event: self.mouse_event(self.last_position, KeyModifiers::default()),
                leaves: true,
            }],
            ViewInput::PointerButton {
                position,
                button,
                pressed,
                click_count,
                modifiers,
            } => {
                self.last_position = position;
                let (button_type, held) = match button {
                    MouseButton::Left => (
                        MouseButtonType::LEFT,
                        flag_bits(cef_event_flags_t::EVENTFLAG_LEFT_MOUSE_BUTTON),
                    ),
                    MouseButton::Middle => (
                        MouseButtonType::MIDDLE,
                        flag_bits(cef_event_flags_t::EVENTFLAG_MIDDLE_MOUSE_BUTTON),
                    ),
                    MouseButton::Right => (
                        MouseButtonType::RIGHT,
                        flag_bits(cef_event_flags_t::EVENTFLAG_RIGHT_MOUSE_BUTTON),
                    ),
                };
                if pressed {
                    self.held_buttons |= held;
                } else {
                    self.held_buttons &= !held;
                }
                vec![PageEvent::MouseClick {
                    event: self.mouse_event(position, modifiers),
                    button: button_type,
                    released: !pressed,
                    click_count: i32::from(click_count),
                }]
            }
            ViewInput::PointerWheel {
                position,
                delta_x,
                delta_y,
                modifiers,
            } => {
                self.last_position = position;
                vec![PageEvent::MouseWheel {
                    event: self.mouse_event(position, modifiers),
                    delta_x: whole_pixels(delta_x),
                    delta_y: whole_pixels(delta_y),
                }]
            }
            ViewInput::Key(key) => key_events(&key).into_iter().map(PageEvent::Key).collect(),
            ViewInput::Focus(focused) => vec![PageEvent::Focus(focused)],
            ViewInput::InsertText(text) => vec![PageEvent::CommitText(text)],
        }
    }

    fn mouse_event(&self, position: ViewPoint, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            x: position.x,
            y: position.y,
            modifiers: modifier_flags(modifiers) | self.held_buttons,
        }
    }
}

fn whole_pixels(delta: f32) -> i32 {
    // Deltas are finite, and a scroll beyond the i32 range saturates.
    #[expect(clippy::cast_possible_truncation, reason = "the cast saturates")]
    let pixels = delta.round() as i32;
    pixels
}

fn modifier_flags(modifiers: KeyModifiers) -> u32 {
    [
        (modifiers.shift, cef_event_flags_t::EVENTFLAG_SHIFT_DOWN),
        (modifiers.control, cef_event_flags_t::EVENTFLAG_CONTROL_DOWN),
        (modifiers.alt, cef_event_flags_t::EVENTFLAG_ALT_DOWN),
        (modifiers.meta, cef_event_flags_t::EVENTFLAG_COMMAND_DOWN),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .fold(0, |flags, (_, flag)| flags | flag_bits(flag))
}

/// The flag as events carry it; C enums are unsigned on Linux and signed on Windows.
#[cfg(not(windows))]
fn flag_bits(flag: cef_event_flags_t) -> u32 {
    flag.0
}

#[cfg(windows)]
fn flag_bits(flag: cef_event_flags_t) -> u32 {
    flag.0.cast_unsigned()
}

/// A key press is a raw key down followed by the characters it types; a release is a key up.
/// Shortcuts type nothing, and a key without a code, such as a composed character, only types.
fn key_events(key: &KeyInput) -> Vec<KeyEvent> {
    let mut modifiers = modifier_flags(key.modifiers);
    if key.repeat {
        modifiers |= flag_bits(cef_event_flags_t::EVENTFLAG_IS_REPEAT);
    }
    let key_code = i32::try_from(key.key_code).unwrap_or(0);
    let event = |type_, character| KeyEvent {
        type_,
        modifiers,
        windows_key_code: key_code,
        character,
        unmodified_character: character,
        ..KeyEvent::default()
    };
    // Windows reads a typed character from the key code, Linux from the character.
    let char_event = |character: u16| KeyEvent {
        windows_key_code: if cfg!(windows) {
            i32::from(character)
        } else {
            key_code
        },
        ..event(KeyEventType::CHAR, character)
    };
    if !key.pressed {
        return if key_code == 0 {
            Vec::new()
        } else {
            vec![event(KeyEventType::KEYUP, 0)]
        };
    }
    let mut events = Vec::new();
    if key_code != 0 {
        events.push(event(KeyEventType::RAWKEYDOWN, 0));
    }
    if key.modifiers.control || key.modifiers.meta {
        return events;
    }
    if key.key_code == RETURN_KEY_CODE {
        events.push(char_event(CARRIAGE_RETURN));
        return events;
    }
    events.extend(key.text.encode_utf16().map(char_event));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key_code: u32, text: &str, pressed: bool, modifiers: KeyModifiers) -> KeyInput {
        KeyInput {
            key_code,
            text: text.to_owned(),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    fn kinds(events: &[KeyEvent]) -> Vec<(KeyEventType, u16)> {
        events
            .iter()
            .map(|event| (event.type_, event.character))
            .collect()
    }

    #[test]
    fn typing_a_letter_presses_the_key_then_types_it() {
        let events = key_events(&key(0x41, "a", true, KeyModifiers::default()));

        assert_eq!(
            kinds(&events),
            [
                (KeyEventType::RAWKEYDOWN, 0),
                (KeyEventType::CHAR, u16::from(b'a'))
            ]
        );
        let typed_key_code = if cfg!(windows) { i32::from(b'a') } else { 0x41 };
        assert_eq!(
            events
                .iter()
                .map(|event| event.windows_key_code)
                .collect::<Vec<_>>(),
            [0x41, typed_key_code]
        );
    }

    #[test]
    fn shortcuts_type_nothing_and_return_types_a_carriage_return() {
        let control = KeyModifiers {
            control: true,
            ..KeyModifiers::default()
        };

        assert_eq!(
            kinds(&key_events(&key(0x41, "a", true, control))),
            [(KeyEventType::RAWKEYDOWN, 0)]
        );
        assert_eq!(
            kinds(&key_events(&key(0x0D, "\n", true, KeyModifiers::default()))),
            [(KeyEventType::RAWKEYDOWN, 0), (KeyEventType::CHAR, 0x0D)]
        );
        assert_eq!(
            kinds(&key_events(&key(0x41, "a", false, KeyModifiers::default()))),
            [(KeyEventType::KEYUP, 0)]
        );
    }

    #[test]
    fn composed_characters_without_a_key_code_only_type() {
        assert_eq!(
            kinds(&key_events(&key(0, "á", true, KeyModifiers::default()))),
            [(KeyEventType::CHAR, 0xE1)]
        );
        assert!(key_events(&key(0, "á", false, KeyModifiers::default())).is_empty());
    }

    #[test]
    fn moves_carry_the_buttons_held_since_their_press() {
        let mut translator = InputTranslator::default();
        let position = ViewPoint { x: 4, y: 9 };
        translator.translate(ViewInput::PointerButton {
            position,
            button: MouseButton::Left,
            pressed: true,
            click_count: 1,
            modifiers: KeyModifiers::default(),
        });

        let dragging = translator.translate(ViewInput::PointerMoved {
            position: ViewPoint { x: 6, y: 9 },
            modifiers: KeyModifiers {
                shift: true,
                ..KeyModifiers::default()
            },
        });
        translator.translate(ViewInput::PointerButton {
            position,
            button: MouseButton::Left,
            pressed: false,
            click_count: 1,
            modifiers: KeyModifiers::default(),
        });
        let left = translator.translate(ViewInput::PointerLeft);

        let dragging_flags = flag_bits(cef_event_flags_t::EVENTFLAG_LEFT_MOUSE_BUTTON)
            | flag_bits(cef_event_flags_t::EVENTFLAG_SHIFT_DOWN);
        assert!(matches!(
            &dragging[..],
            [PageEvent::MouseMove { event, leaves: false }] if event.modifiers == dragging_flags
        ));
        assert!(matches!(
            &left[..],
            [PageEvent::MouseMove { event, leaves: true }]
                if (event.x, event.y, event.modifiers) == (4, 9, 0)
        ));
    }
}
