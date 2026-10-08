//! Turns the browser pane's keys and clicks into the browser's view input: Slint names keys by the
//! text they type, or a private character for keys that type nothing, while pages expect DOM key
//! codes and click counts.

use std::time::{Duration, Instant};

use tufo_client::browser::{KeyInput, KeyModifiers, MouseButton, ViewPoint};

/// The second press of a double click comes within this long of the first.
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);
/// And within this many logical pixels of it.
const DOUBLE_CLICK_DISTANCE_MAX: i32 = 4;
/// A triple click selects a paragraph; further presses start over.
const CLICK_COUNT_MAX: u8 = 3;

/// Slint's special keys and their DOM key codes.
const SPECIAL_KEYS: [(char, u32); 30] = [
    ('\u{0008}', 0x08),
    ('\u{0009}', 0x09),
    ('\u{0019}', 0x09),
    ('\u{000a}', 0x0D),
    ('\u{001b}', 0x1B),
    ('\u{007f}', 0x2E),
    ('\u{0010}', 0x10),
    ('\u{0011}', 0x11),
    ('\u{0012}', 0x12),
    ('\u{0013}', 0x12),
    ('\u{0014}', 0x14),
    ('\u{0015}', 0x10),
    ('\u{0016}', 0x11),
    ('\u{0017}', 0x5B),
    ('\u{0018}', 0x5C),
    ('\u{0020}', 0x20),
    ('\u{F700}', 0x26),
    ('\u{F701}', 0x28),
    ('\u{F702}', 0x25),
    ('\u{F703}', 0x27),
    ('\u{F727}', 0x2D),
    ('\u{F729}', 0x24),
    ('\u{F72B}', 0x23),
    ('\u{F72C}', 0x21),
    ('\u{F72D}', 0x22),
    ('\u{F72F}', 0x91),
    ('\u{F730}', 0x13),
    ('\u{F731}', 0x2C),
    ('\u{F734}', 0xB2),
    ('\u{F735}', 0x5D),
];
/// Slint's F1 to F24, which map to consecutive codes from F1's.
const FUNCTION_KEYS: std::ops::RangeInclusive<char> = '\u{F704}'..='\u{F71B}';
const FUNCTION_KEY_CODE_FIRST: u32 = 0x70;
/// The punctuation of a US keyboard and its keys' codes, shifted or not.
const PUNCTUATION_KEYS: [(char, char, u32); 11] = [
    (';', ':', 0xBA),
    ('=', '+', 0xBB),
    (',', '<', 0xBC),
    ('-', '_', 0xBD),
    ('.', '>', 0xBE),
    ('/', '?', 0xBF),
    ('`', '~', 0xC0),
    ('[', '{', 0xDB),
    ('\\', '|', 0xDC),
    (']', '}', 0xDD),
    ('\'', '"', 0xDE),
];
/// Shifted digits of a US keyboard, from ')' over 0 to '(' over 9.
const SHIFTED_DIGITS: [char; 10] = [')', '!', '@', '#', '$', '%', '^', '&', '*', '('];
/// Slint's private characters for keys that type nothing.
const PRIVATE_KEYS: std::ops::RangeInclusive<char> = '\u{F700}'..='\u{F8FF}';

/// The view input for one key press or release, or `None` for an empty event.
pub(crate) fn key_input(
    text: &str,
    pressed: bool,
    repeat: bool,
    modifiers: KeyModifiers,
) -> Option<KeyInput> {
    let mut characters = text.chars();
    let first = characters.next()?;
    let single = characters.next().is_none();
    let key_code = if single { key_code(first) } else { 0 };
    let typed = text
        .chars()
        .filter(|character| !character.is_control() && !PRIVATE_KEYS.contains(character))
        .collect::<String>();
    if key_code == 0 && typed.is_empty() {
        return None;
    }
    Some(KeyInput {
        key_code,
        text: typed,
        pressed,
        repeat,
        modifiers,
    })
}

fn key_code(key: char) -> u32 {
    if let Some((_, code)) = SPECIAL_KEYS.iter().find(|(special, _)| *special == key) {
        return *code;
    }
    if FUNCTION_KEYS.contains(&key) {
        return FUNCTION_KEY_CODE_FIRST + (u32::from(key) - u32::from(*FUNCTION_KEYS.start()));
    }
    if key.is_ascii_alphabetic() {
        return u32::from(key.to_ascii_uppercase());
    }
    if key.is_ascii_digit() {
        return u32::from(key);
    }
    if let Some(digit) = SHIFTED_DIGITS.iter().position(|shifted| *shifted == key) {
        return u32::from(b'0') + u32::try_from(digit).unwrap_or(0);
    }
    PUNCTUATION_KEYS
        .iter()
        .find(|(plain, shifted, _)| *plain == key || *shifted == key)
        .map_or(0, |(_, _, code)| *code)
}

/// Counts presses of one button at one place in quick succession, as double and triple clicks.
#[derive(Debug, Default)]
pub(crate) struct ClickCounter {
    last: Option<LastPress>,
}

#[derive(Debug, Clone, Copy)]
struct LastPress {
    at: Instant,
    position: ViewPoint,
    button: MouseButton,
    count: u8,
}

impl ClickCounter {
    /// The click count of a press at `now`.
    pub(crate) fn press(&mut self, button: MouseButton, position: ViewPoint, now: Instant) -> u8 {
        let continues = self.last.is_some_and(|last| {
            last.button == button
                && now.duration_since(last.at) <= DOUBLE_CLICK_INTERVAL
                && (last.position.x - position.x).abs() <= DOUBLE_CLICK_DISTANCE_MAX
                && (last.position.y - position.y).abs() <= DOUBLE_CLICK_DISTANCE_MAX
                && last.count < CLICK_COUNT_MAX
        });
        let count = match self.last {
            Some(last) if continues => last.count + 1,
            _ => 1,
        };
        self.last = Some(LastPress {
            at: now,
            position,
            button,
            count,
        });
        count
    }

    /// The count of the press a release ends.
    pub(crate) fn release(&self, button: MouseButton) -> u8 {
        self.last
            .filter(|last| last.button == button)
            .map_or(1, |last| last.count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> Option<(u32, String)> {
        key_input(text, true, false, KeyModifiers::default()).map(|key| (key.key_code, key.text))
    }

    #[test]
    fn printable_keys_carry_their_code_and_text() {
        assert_eq!(key("a"), Some((0x41, "a".to_owned())));
        assert_eq!(key("A"), Some((0x41, "A".to_owned())));
        assert_eq!(key("7"), Some((0x37, "7".to_owned())));
        assert_eq!(key("&"), Some((0x37, "&".to_owned())));
        assert_eq!(key("?"), Some((0xBF, "?".to_owned())));
        assert_eq!(key(" "), Some((0x20, " ".to_owned())));
    }

    #[test]
    fn special_keys_carry_a_code_and_type_nothing() {
        assert_eq!(key("\u{F702}"), Some((0x25, String::new())));
        assert_eq!(key("\u{0008}"), Some((0x08, String::new())));
        assert_eq!(key("\n"), Some((0x0D, String::new())));
        assert_eq!(key("\u{F70F}"), Some((0x7B, String::new())));
        assert_eq!(key("\u{0010}"), Some((0x10, String::new())));
    }

    #[test]
    fn other_characters_only_type() {
        assert_eq!(key("á"), Some((0, "á".to_owned())));
        assert_eq!(key("ção"), Some((0, "ção".to_owned())));
        assert_eq!(key(""), None);
        assert_eq!(key("\u{F7FF}"), None);
    }

    #[test]
    fn quick_presses_at_one_place_count_up_to_three() {
        let mut counter = ClickCounter::default();
        let start = Instant::now();
        let here = ViewPoint { x: 10, y: 10 };
        let near = ViewPoint { x: 12, y: 9 };

        assert_eq!(counter.press(MouseButton::Left, here, start), 1);
        assert_eq!(counter.release(MouseButton::Left), 1);
        assert_eq!(
            counter.press(MouseButton::Left, near, start + Duration::from_millis(200)),
            2
        );
        assert_eq!(counter.release(MouseButton::Left), 2);
        assert_eq!(
            counter.press(MouseButton::Left, near, start + Duration::from_millis(400)),
            3
        );
        assert_eq!(
            counter.press(MouseButton::Left, near, start + Duration::from_millis(600)),
            1
        );
        assert_eq!(
            counter.press(MouseButton::Right, near, start + Duration::from_millis(700)),
            1
        );
        assert_eq!(
            counter.press(
                MouseButton::Right,
                ViewPoint { x: 40, y: 9 },
                start + Duration::from_millis(800)
            ),
            1
        );
        assert_eq!(
            counter.press(MouseButton::Right, near, start + Duration::from_secs(2)),
            1
        );
    }
}
