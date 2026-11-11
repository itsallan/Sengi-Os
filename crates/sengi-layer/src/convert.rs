//! Small translations between what Wayland sends and what Slint wants.
//! Nothing here touches a connection, so all of it is unit tested.

use std::time::Duration;

use slint::platform::{Key, PointerEventButton};
use slint::SharedString;
use smithay_client_toolkit::seat::keyboard::Keysym;

// Linux input event codes for the mouse buttons.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// What Slint scrolls for one notch of a mouse wheel, in logical pixels.
const WHEEL_NOTCH: f32 = 60.0;

/// The most repeats delivered in one go after the loop has been held up, so
/// a stall does not turn into a burst of characters.
const MAX_REPEATS_AT_ONCE: u32 = 4;

pub(crate) fn pointer_button(code: u32) -> Option<PointerEventButton> {
    match code {
        BTN_LEFT => Some(PointerEventButton::Left),
        BTN_RIGHT => Some(PointerEventButton::Right),
        BTN_MIDDLE => Some(PointerEventButton::Middle),
        BTN_SIDE => Some(PointerEventButton::Back),
        BTN_EXTRA => Some(PointerEventButton::Forward),
        _ => None,
    }
}

/// Turns one axis of a Wayland scroll event into a Slint scroll delta.
///
/// Wayland counts scrolling down and right as positive, Slint counts it as
/// negative. A wheel reports whole notches in `discrete`; a touchpad
/// reports only a distance in `absolute`.
///
/// smithay-client-toolkit 0.19 binds wl_seat at version 7 at most, where
/// `discrete` counts notches. From version 8 the protocol sends 120ths of
/// a notch instead; if a later toolkit passes those through here, divide.
pub(crate) fn scroll_delta(absolute: f64, discrete: i32) -> f32 {
    if discrete != 0 {
        -(discrete as f32) * WHEEL_NOTCH
    } else {
        -(absolute as f32)
    }
}

/// The text Slint expects for a key press: one of its own codes for keys
/// such as Escape and the arrows, otherwise what the press typed in the
/// active layout. `None` for presses that type nothing.
///
/// `utf8` is what xkbcommon says the press produced, after dead keys and
/// compose sequences. It is `None` for a key that is part of a sequence
/// still being typed, and that key must not show up as text: Compose,
/// apostrophe, e is "é", not "'é". While Ctrl is held it is a control code,
/// and then the key's own symbol is used, so that Ctrl+A reaches Slint as
/// "a" with the control modifier set.
pub(crate) fn key_text(keysym: Keysym, utf8: Option<&str>) -> Option<SharedString> {
    if let Some(key) = special_key(keysym) {
        return Some(key.into());
    }
    let text = utf8?;
    if text.is_empty() {
        return None;
    }
    if !text.chars().any(char::is_control) {
        return Some(text.into());
    }
    keysym
        .key_char()
        .filter(|character| !character.is_control())
        .map(SharedString::from)
}

fn special_key(keysym: Keysym) -> Option<Key> {
    Some(match keysym {
        Keysym::BackSpace => Key::Backspace,
        Keysym::Tab | Keysym::KP_Tab => Key::Tab,
        Keysym::ISO_Left_Tab => Key::Backtab,
        Keysym::Return | Keysym::KP_Enter => Key::Return,
        Keysym::Escape => Key::Escape,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Up | Keysym::KP_Up => Key::UpArrow,
        Keysym::Down | Keysym::KP_Down => Key::DownArrow,
        Keysym::Left | Keysym::KP_Left => Key::LeftArrow,
        Keysym::Right | Keysym::KP_Right => Key::RightArrow,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
        Keysym::Shift_L => Key::Shift,
        Keysym::Shift_R => Key::ShiftR,
        Keysym::Control_L => Key::Control,
        Keysym::Control_R => Key::ControlR,
        Keysym::Alt_L | Keysym::Alt_R | Keysym::Meta_L | Keysym::Meta_R => Key::Alt,
        Keysym::ISO_Level3_Shift => Key::AltGr,
        Keysym::Super_L => Key::Meta,
        Keysym::Super_R => Key::MetaR,
        Keysym::Caps_Lock => Key::CapsLock,
        Keysym::Menu => Key::Menu,
        Keysym::F1 => Key::F1,
        Keysym::F2 => Key::F2,
        Keysym::F3 => Key::F3,
        Keysym::F4 => Key::F4,
        Keysym::F5 => Key::F5,
        Keysym::F6 => Key::F6,
        Keysym::F7 => Key::F7,
        Keysym::F8 => Key::F8,
        Keysym::F9 => Key::F9,
        Keysym::F10 => Key::F10,
        Keysym::F11 => Key::F11,
        Keysym::F12 => Key::F12,
        _ => return None,
    })
}

/// The modifiers Slint keeps track of, as booleans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModifierState {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

type ModifierGroup = (&'static [Key], fn(&ModifierState) -> bool);

/// The keys behind each modifier, the left-hand key first.
const MODIFIER_GROUPS: [ModifierGroup; 4] = [
    (&[Key::Shift, Key::ShiftR], |state| state.shift),
    (&[Key::Control, Key::ControlR], |state| state.control),
    (&[Key::Alt], |state| state.alt),
    (&[Key::Meta, Key::MetaR], |state| state.meta),
];

/// The key behind a key text, if the text is a single character.
fn single(text: &str) -> Option<char> {
    let mut characters = text.chars();
    match (characters.next(), characters.next()) {
        (Some(first), None) => Some(first),
        _ => None,
    }
}

/// True for the key texts Slint treats as modifier keys.
pub(crate) fn is_modifier(text: &str) -> bool {
    let Some(key) = single(text) else {
        return false;
    };
    key == char::from(Key::AltGr)
        || MODIFIER_GROUPS
            .iter()
            .flat_map(|(keys, _)| keys.iter())
            .any(|modifier| char::from(*modifier) == key)
}

/// Whether holding the key down should repeat it. Text, editing and
/// navigation keys do. Modifiers and locks never do, and a held Escape or
/// function key acting over and over helps nobody.
pub(crate) fn key_repeats(text: &str) -> bool {
    let Some(key) = single(text) else {
        return !text.is_empty();
    };
    let function_keys = char::from(Key::F1)..=char::from(Key::F24);
    !is_modifier(text)
        && !function_keys.contains(&key)
        && ![Key::CapsLock, Key::Escape, Key::Menu, Key::Insert]
            .iter()
            .any(|still| char::from(*still) == key)
}

/// Slint learns about modifiers from the presses and releases of the
/// modifier keys themselves. Those can be missed: a modifier already held
/// when the surface gets the keyboard is never seen going down. Given the
/// modifier keys Slint has been told are `held` and what the compositor says
/// is `actual`ly active, this returns the presses (`true`) and releases
/// (`false`) that bring Slint back in line.
pub(crate) fn modifier_fixes(held: &[char], actual: ModifierState) -> Vec<(char, bool)> {
    let mut fixes = Vec::new();
    for (keys, active) in MODIFIER_GROUPS {
        let mut told = keys
            .iter()
            .map(|key| char::from(*key))
            .filter(|key| held.contains(key))
            .peekable();
        if !active(&actual) {
            fixes.extend(told.map(|key| (key, false)));
        } else if told.peek().is_none() {
            fixes.push((keys[0].into(), true));
        }
    }
    fixes
}

/// How a held key repeats, from the compositor's repeat settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RepeatTiming {
    /// How long a key is held before it starts repeating.
    pub delay: Duration,
    /// Time between repeats after that.
    pub interval: Duration,
}

impl RepeatTiming {
    /// `rate` is in repeats per second and `delay` in milliseconds, as the
    /// compositor sends them. A rate of zero turns repeating off.
    pub(crate) fn new(rate: u32, delay: u32) -> Option<RepeatTiming> {
        if rate == 0 {
            return None;
        }
        Some(RepeatTiming {
            delay: Duration::from_millis(u64::from(delay)),
            interval: Duration::from_micros(1_000_000 / u64::from(rate)),
        })
    }
}

/// How many repeats are owed at `now` for a key whose next repeat was due at
/// `next`, and when the one after those is due. Times are measured from any
/// fixed starting point.
pub(crate) fn repeats_due(next: Duration, interval: Duration, now: Duration) -> (u32, Duration) {
    if now < next {
        return (0, next);
    }
    let interval = interval.max(Duration::from_micros(1));
    let owed = 1 + (now - next).as_micros() / interval.as_micros();
    if owed > u128::from(MAX_REPEATS_AT_ONCE) {
        return (MAX_REPEATS_AT_ONCE, now + interval);
    }
    let owed = owed as u32;
    (owed, next + interval * owed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(key: Key) -> SharedString {
        key.into()
    }

    #[test]
    fn mouse_buttons_map_to_slint_buttons() {
        assert_eq!(pointer_button(BTN_LEFT), Some(PointerEventButton::Left));
        assert_eq!(pointer_button(BTN_RIGHT), Some(PointerEventButton::Right));
        assert_eq!(pointer_button(BTN_MIDDLE), Some(PointerEventButton::Middle));
        assert_eq!(pointer_button(BTN_SIDE), Some(PointerEventButton::Back));
        assert_eq!(pointer_button(BTN_EXTRA), Some(PointerEventButton::Forward));
        assert_eq!(pointer_button(0x115), None);
    }

    #[test]
    fn scrolling_is_flipped_and_wheel_notches_are_whole_steps() {
        assert_eq!(scroll_delta(15.0, 1), -60.0);
        assert_eq!(scroll_delta(-30.0, -2), 120.0);
        assert_eq!(scroll_delta(7.5, 0), -7.5);
        assert_eq!(scroll_delta(0.0, 0), 0.0);
    }

    #[test]
    fn editing_and_navigation_keys_map_to_slint_codes() {
        let cases = [
            (Keysym::Escape, Key::Escape),
            (Keysym::Return, Key::Return),
            (Keysym::KP_Enter, Key::Return),
            (Keysym::BackSpace, Key::Backspace),
            (Keysym::Tab, Key::Tab),
            (Keysym::ISO_Left_Tab, Key::Backtab),
            (Keysym::Delete, Key::Delete),
            (Keysym::Up, Key::UpArrow),
            (Keysym::Down, Key::DownArrow),
            (Keysym::Left, Key::LeftArrow),
            (Keysym::Right, Key::RightArrow),
            (Keysym::Home, Key::Home),
            (Keysym::End, Key::End),
            (Keysym::Page_Up, Key::PageUp),
            (Keysym::Page_Down, Key::PageDown),
            (Keysym::Shift_L, Key::Shift),
            (Keysym::Shift_R, Key::ShiftR),
            (Keysym::Control_L, Key::Control),
            (Keysym::Alt_L, Key::Alt),
            (Keysym::ISO_Level3_Shift, Key::AltGr),
            (Keysym::Super_L, Key::Meta),
            (Keysym::F5, Key::F5),
        ];
        for (keysym, key) in cases {
            assert_eq!(key_text(keysym, None), Some(text(key)), "{keysym:?}");
        }
    }

    #[test]
    fn special_keys_win_over_the_control_codes_xkb_reports() {
        assert_eq!(
            key_text(Keysym::Return, Some("\r")),
            Some(text(Key::Return))
        );
        assert_eq!(
            key_text(Keysym::BackSpace, Some("\u{8}")),
            Some(text(Key::Backspace))
        );
        assert_eq!(
            key_text(Keysym::Escape, Some("\u{1b}")),
            Some(text(Key::Escape))
        );
    }

    #[test]
    fn typed_text_follows_the_layout() {
        assert_eq!(key_text(Keysym::a, Some("a")), Some("a".into()));
        assert_eq!(key_text(Keysym::A, Some("A")), Some("A".into()));
        assert_eq!(key_text(Keysym::space, Some(" ")), Some(" ".into()));
        assert_eq!(key_text(Keysym::eacute, Some("é")), Some("é".into()));
        // A compose sequence ends on a plain key but produces something else.
        assert_eq!(key_text(Keysym::e, Some("ë")), Some("ë".into()));
    }

    #[test]
    fn control_combinations_fall_back_to_the_symbol() {
        assert_eq!(key_text(Keysym::a, Some("\u{1}")), Some("a".into()));
        assert_eq!(key_text(Keysym::z, Some("\u{1a}")), Some("z".into()));
    }

    #[test]
    fn keys_inside_a_compose_sequence_type_nothing() {
        // Compose, apostrophe, e: only the last press produces text.
        assert_eq!(key_text(Keysym::Multi_key, None), None);
        assert_eq!(key_text(Keysym::apostrophe, None), None);
        assert_eq!(key_text(Keysym::e, Some("\u{e9}")), Some("\u{e9}".into()));
        // A dead key, and a key that types nothing at all.
        assert_eq!(key_text(Keysym::dead_acute, None), None);
        assert_eq!(key_text(Keysym::a, Some("")), None);
    }

    #[test]
    fn special_keys_work_inside_a_compose_sequence_too() {
        assert_eq!(key_text(Keysym::Escape, None), Some(text(Key::Escape)));
        assert_eq!(
            key_text(Keysym::BackSpace, None),
            Some(text(Key::Backspace))
        );
    }

    #[test]
    fn keys_slint_has_no_use_for_are_dropped() {
        assert_eq!(key_text(Keysym::XF86_AudioMute, None), None);
        assert_eq!(key_text(Keysym::Num_Lock, None), None);
        assert_eq!(key_text(Keysym::dead_acute, None), None);
    }

    #[test]
    fn only_keys_worth_repeating_repeat() {
        assert!(key_repeats("a"));
        assert!(key_repeats(" "));
        assert!(key_repeats("\u{e9}"));
        assert!(key_repeats(&text(Key::Backspace)));
        assert!(key_repeats(&text(Key::Delete)));
        assert!(key_repeats(&text(Key::LeftArrow)));
        assert!(key_repeats(&text(Key::DownArrow)));
        assert!(key_repeats(&text(Key::Return)));
        assert!(key_repeats(&text(Key::Tab)));
        assert!(!key_repeats(&text(Key::Shift)));
        assert!(!key_repeats(&text(Key::AltGr)));
        assert!(!key_repeats(&text(Key::CapsLock)));
        assert!(!key_repeats(&text(Key::Escape)));
        assert!(!key_repeats(&text(Key::F5)));
        assert!(!key_repeats(""));
    }

    #[test]
    fn modifier_keys_are_recognised() {
        assert!(is_modifier(&text(Key::AltGr)));
        assert!(is_modifier(&text(Key::Shift)));
        assert!(is_modifier(&text(Key::ControlR)));
        assert!(is_modifier(&text(Key::Meta)));
        assert!(!is_modifier(&text(Key::Escape)));
        assert!(!is_modifier("a"));
        assert!(!is_modifier(""));
    }

    #[test]
    fn a_modifier_held_before_focus_is_pressed_for_slint() {
        let actual = ModifierState {
            meta: true,
            ..Default::default()
        };
        assert_eq!(modifier_fixes(&[], actual), [(Key::Meta.into(), true)]);
    }

    #[test]
    fn modifiers_slint_already_knows_about_are_left_alone() {
        let held = [Key::ShiftR.into(), Key::Control.into()];
        let actual = ModifierState {
            shift: true,
            control: true,
            ..Default::default()
        };
        assert_eq!(modifier_fixes(&held, actual), []);
    }

    #[test]
    fn modifiers_released_out_of_sight_are_released_for_slint() {
        let held = [Key::Shift.into(), Key::ShiftR.into(), Key::Alt.into()];
        let actual = ModifierState {
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            modifier_fixes(&held, actual),
            [(Key::Shift.into(), false), (Key::ShiftR.into(), false)]
        );
    }

    #[test]
    fn repeat_timing_comes_from_rate_and_delay() {
        assert_eq!(
            RepeatTiming::new(25, 600),
            Some(RepeatTiming {
                delay: Duration::from_millis(600),
                interval: Duration::from_millis(40),
            })
        );
        assert_eq!(RepeatTiming::new(0, 600), None);
    }

    #[test]
    fn nothing_repeats_before_it_is_due() {
        let next = Duration::from_millis(600);
        let interval = Duration::from_millis(40);
        assert_eq!(
            repeats_due(next, interval, Duration::from_millis(599)),
            (0, next)
        );
    }

    #[test]
    fn repeats_keep_their_rhythm_when_the_loop_wakes_late() {
        let next = Duration::from_millis(600);
        let interval = Duration::from_millis(40);
        assert_eq!(
            repeats_due(next, interval, Duration::from_millis(600)),
            (1, Duration::from_millis(640))
        );
        assert_eq!(
            repeats_due(next, interval, Duration::from_millis(639)),
            (1, Duration::from_millis(640))
        );
        assert_eq!(
            repeats_due(next, interval, Duration::from_millis(690)),
            (3, Duration::from_millis(720))
        );
    }

    #[test]
    fn a_long_stall_does_not_flood_the_text_field() {
        let next = Duration::from_millis(600);
        let interval = Duration::from_millis(40);
        let now = Duration::from_secs(5);
        assert_eq!(
            repeats_due(next, interval, now),
            (MAX_REPEATS_AT_ONCE, now + interval)
        );
    }
}
