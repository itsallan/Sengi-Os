//! The built-in key bindings. They match what labwc is configured with in
//! the image, and the labwc defaults people reach for without thinking, so
//! the session behaves the same on either compositor.

/// Keysyms used by the bindings. Letters are their ASCII codes.
const SPACE: u32 = 0x0020;
pub const RETURN: u32 = 0xff0d;
pub const KP_ENTER: u32 = 0xff8d;
pub const ESCAPE: u32 = 0xff1b;
pub const DELETE: u32 = 0xffff;
const TAB: u32 = 0xff09;
const F4: u32 = 0xffc1;
pub const LEFT: u32 = 0xff51;
pub const UP: u32 = 0xff52;
pub const RIGHT: u32 = 0xff53;
pub const DOWN: u32 = 0xff54;
/// The volume and mute keys of most keyboards.
const AUDIO_LOWER: u32 = 0x1008_ff11;
const AUDIO_MUTE: u32 = 0x1008_ff12;
const AUDIO_RAISE: u32 = 0x1008_ff13;
/// The brightness keys of the screen and of the keyboard light, and Print.
const MON_BRIGHTNESS_UP: u32 = 0x1008_ff02;
const MON_BRIGHTNESS_DOWN: u32 = 0x1008_ff03;
const KBD_LIGHT_TOGGLE: u32 = 0x1008_ff04;
const KBD_BRIGHTNESS_UP: u32 = 0x1008_ff05;
const KBD_BRIGHTNESS_DOWN: u32 = 0x1008_ff06;
const PRINT: u32 = 0xff61;
/// What Alt+Print produces: xkb puts Sys_Req on the Print key's second level.
const SYS_REQ: u32 = 0xff15;
/// What Shift+Tab produces.
const ISO_LEFT_TAB: u32 = 0xfe20;
/// XF86Switch_VT_1; the other eleven follow it.
const SWITCH_VT_1: u32 = 0x1008_fe01;

/// The virtual terminal a keysym asks for, if it is one of the VT keys.
/// These work everywhere, the lock screen included.
pub fn vt_for(keysym: u32) -> Option<i32> {
    (SWITCH_VT_1..SWITCH_VT_1 + 12)
        .contains(&keysym)
        .then(|| (keysym - SWITCH_VT_1) as i32 + 1)
}

/// What locks the session unless `--locker` says otherwise: a program and
/// its arguments. It is started without a shell.
pub const DEFAULT_LOCKER: &str = "sengi-lock -f";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Launcher,
    Terminal,
    Lock,
    /// Super+V: the history of what was copied.
    ClipboardHistory,
    /// Super+/: the cheat sheet of these bindings.
    Shortcuts,
    CloseWindow,
    NextWindow,
    PreviousWindow,
    /// Ctrl+Alt+F1 to F12, which xkb turns into one keysym each.
    SwitchVt(i32),
    /// Super+A: maximise, or back.
    Maximise,
    /// Super+an arrow: half of the screen on that side.
    Snap(Edge),
    /// Alt+an arrow: the window to that edge of the screen.
    MoveToEdge(Edge),
    Volume(Volume),
    Brightness(Brightness),
    Screenshot(Shot),
    /// A key while the switcher or a menu is open.
    Overlay(Step),
}

/// What a key does to the switcher or a menu that is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Previous,
    Up,
    Down,
    Activate,
    Cancel,
    /// Delete: drops the entry of the clipboard history that is selected.
    Remove,
}

/// A side of a screen, for snapping and moving windows to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Up,
    Down,
}

/// What the brightness keys do. The program that carries them out is
/// sengi-brightness; the compositor itself has no bus to ask logind on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brightness {
    Up,
    Down,
    KeyboardUp,
    KeyboardDown,
    KeyboardToggle,
}

/// What Print takes a picture of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shot {
    Screen,
    /// The window that has the keyboard.
    Window,
}

/// What the volume keys do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volume {
    Up,
    Down,
    Mute,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Mods {
    pub logo: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// The symbol to look a binding up by. Letter bindings mean the key where
/// that letter is on a Latin keyboard, so on a layout whose keys give
/// other letters (Cyrillic, Greek) the key's position decides. `keycode`
/// is the xkb one, eight above the kernel's.
pub fn binding_symbol(keysym: u32, keycode: u32) -> u32 {
    let latin = char::from_u32(keysym).is_some_and(|c| c.is_ascii());
    // Function and control keys live at 0xfd00 and above.
    if latin || keysym >= 0xfd00 {
        return keysym;
    }
    match keycode.wrapping_sub(8) {
        16 => u32::from(b'q'),
        38 => u32::from(b'l'),
        47 => u32::from(b'v'),
        _ => keysym,
    }
}

/// Which of the held modifiers a binding takes.
type When = fn(Mods) -> bool;

fn only_logo(m: Mods) -> bool {
    m.logo && !m.alt && !m.ctrl && !m.shift
}

fn only_alt(m: Mods) -> bool {
    m.alt && !m.logo && !m.ctrl && !m.shift
}

fn alt_and_shift(m: Mods) -> bool {
    m.alt && m.shift && !m.logo && !m.ctrl
}

/// The keys of the keyboard's own media row work whatever is held.
fn anything(_: Mods) -> bool {
    true
}

fn plain(m: Mods) -> bool {
    !m.logo && !m.alt && !m.ctrl && !m.shift
}

/// Print with Alt or Shift, or with both.
fn marked(m: Mods) -> bool {
    !m.logo && !m.ctrl && (m.alt || m.shift)
}

fn not_logo_or_ctrl(m: Mods) -> bool {
    !m.logo && !m.ctrl
}

const LOGO: Mods = Mods {
    logo: true,
    alt: false,
    ctrl: false,
    shift: false,
};
const ALT: Mods = Mods {
    logo: false,
    alt: true,
    ctrl: false,
    shift: false,
};
const ALT_SHIFT: Mods = Mods {
    logo: false,
    alt: true,
    ctrl: false,
    shift: true,
};
const NONE: Mods = Mods {
    logo: false,
    alt: false,
    ctrl: false,
    shift: false,
};

/// What a shortcut is about, for grouping the cheat sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Apps,
    Windows,
    Screen,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Section::Apps => "Apps and session",
            Section::Windows => "Windows",
            Section::Screen => "Screen and sound",
        }
    }
}

/// One key binding. This table is what `binding` looks keys up in and
/// what the shortcut cheat sheet lists, so the sheet cannot say anything
/// the compositor does not do.
pub struct Bind {
    pub section: Section,
    /// The key as the sheet names it, without the modifiers.
    pub key: &'static str,
    /// The modifiers the sheet names before the key, and the ones the
    /// tests hold down.
    pub mods: Mods,
    /// Keysyms that trigger it. Letters are lower case; a capital, with
    /// Caps Lock on, counts as the same.
    syms: &'static [u32],
    when: When,
    pub action: Action,
    pub what: &'static str,
    /// Whether the sheet lists it. A few alternative spellings of a
    /// binding are bound but listed once.
    pub listed: bool,
}

const fn bind(
    section: Section,
    key: &'static str,
    mods: Mods,
    syms: &'static [u32],
    when: When,
    action: Action,
    what: &'static str,
) -> Bind {
    Bind {
        section,
        key,
        mods,
        syms,
        when,
        action,
        what,
        listed: true,
    }
}

const fn unlisted(mut bind: Bind) -> Bind {
    bind.listed = false;
    bind
}

const LETTER_A: u32 = b'a' as u32;
const LETTER_L: u32 = b'l' as u32;
const LETTER_Q: u32 = b'q' as u32;
const LETTER_V: u32 = b'v' as u32;
const SLASH: u32 = b'/' as u32;
const F1: u32 = 0xffbe;

#[rustfmt::skip]
pub const BINDINGS: &[Bind] = &[
    bind(Section::Apps, "Space", LOGO, &[SPACE], only_logo, Action::Launcher, "Search and open apps"),
    bind(Section::Apps, "Return", LOGO, &[RETURN], only_logo, Action::Terminal, "Open a terminal"),
    bind(Section::Apps, "L", LOGO, &[LETTER_L], only_logo, Action::Lock, "Lock the screen"),
    bind(Section::Apps, "V", LOGO, &[LETTER_V], only_logo, Action::ClipboardHistory, "Clipboard history (turn it on in Settings)"),
    bind(Section::Apps, "/", LOGO, &[SLASH, F1], only_logo, Action::Shortcuts, "Show these shortcuts (also Super+F1)"),
    bind(Section::Windows, "Q", LOGO, &[LETTER_Q], only_logo, Action::CloseWindow, "Close the window"),
    bind(Section::Windows, "F4", ALT, &[F4], only_alt, Action::CloseWindow, "Close the window"),
    bind(Section::Windows, "Tab", ALT, &[TAB, ISO_LEFT_TAB], only_alt, Action::NextWindow, "Switch to the next window"),
    bind(Section::Windows, "Tab", ALT_SHIFT, &[TAB, ISO_LEFT_TAB], alt_and_shift, Action::PreviousWindow, "Switch to the previous window"),
    bind(Section::Windows, "A", LOGO, &[LETTER_A], only_logo, Action::Maximise, "Maximise or restore the window"),
    bind(Section::Windows, "Left", LOGO, &[LEFT], only_logo, Action::Snap(Edge::Left), "Fill the left half of the screen"),
    bind(Section::Windows, "Right", LOGO, &[RIGHT], only_logo, Action::Snap(Edge::Right), "Fill the right half of the screen"),
    bind(Section::Windows, "Up", LOGO, &[UP], only_logo, Action::Snap(Edge::Up), "Fill the top half of the screen"),
    bind(Section::Windows, "Down", LOGO, &[DOWN], only_logo, Action::Snap(Edge::Down), "Fill the bottom half of the screen"),
    bind(Section::Windows, "Left", ALT, &[LEFT], only_alt, Action::MoveToEdge(Edge::Left), "Move the window to the left edge"),
    bind(Section::Windows, "Right", ALT, &[RIGHT], only_alt, Action::MoveToEdge(Edge::Right), "Move the window to the right edge"),
    bind(Section::Windows, "Up", ALT, &[UP], only_alt, Action::MoveToEdge(Edge::Up), "Move the window to the top edge"),
    bind(Section::Windows, "Down", ALT, &[DOWN], only_alt, Action::MoveToEdge(Edge::Down), "Move the window to the bottom edge"),
    bind(Section::Screen, "Print", NONE, &[PRINT], plain, Action::Screenshot(Shot::Screen), "Take a picture of the screen"),
    // With Shift, or Alt, or both: the window in front.
    bind(Section::Screen, "Print", ALT, &[PRINT], marked, Action::Screenshot(Shot::Window), "Take a picture of the window (Shift+Print too)"),
    // xkb turns Alt+Print into Sys_Req.
    unlisted(bind(Section::Screen, "Sys_Req", ALT, &[SYS_REQ], not_logo_or_ctrl, Action::Screenshot(Shot::Window), "")),
    bind(Section::Screen, "Volume Up", NONE, &[AUDIO_RAISE], anything, Action::Volume(Volume::Up), "Volume up"),
    bind(Section::Screen, "Volume Down", NONE, &[AUDIO_LOWER], anything, Action::Volume(Volume::Down), "Volume down"),
    bind(Section::Screen, "Mute", NONE, &[AUDIO_MUTE], anything, Action::Volume(Volume::Mute), "Mute or unmute"),
    bind(Section::Screen, "Brightness Up", NONE, &[MON_BRIGHTNESS_UP], anything, Action::Brightness(Brightness::Up), "Screen brighter"),
    bind(Section::Screen, "Brightness Down", NONE, &[MON_BRIGHTNESS_DOWN], anything, Action::Brightness(Brightness::Down), "Screen dimmer"),
    bind(Section::Screen, "Keyboard Light Up", NONE, &[KBD_BRIGHTNESS_UP], anything, Action::Brightness(Brightness::KeyboardUp), "Keyboard light brighter"),
    bind(Section::Screen, "Keyboard Light Down", NONE, &[KBD_BRIGHTNESS_DOWN], anything, Action::Brightness(Brightness::KeyboardDown), "Keyboard light dimmer"),
    bind(Section::Screen, "Keyboard Light", NONE, &[KBD_LIGHT_TOGGLE], anything, Action::Brightness(Brightness::KeyboardToggle), "Keyboard light on or off"),
];

/// The keys to the other virtual terminals are not in the table, they are
/// twelve of one kind: `vt_for`. The sheet lists them as one line.
pub const CONSOLE_KEYS: (&str, &str) = ("Ctrl+Alt+F1 to F12", "Switch to another console");

/// The action bound to a key press, if any. `keysym` is the symbol with the
/// held modifiers applied.
pub fn binding(mods: Mods, keysym: u32) -> Option<Action> {
    if let Some(vt) = vt_for(keysym) {
        return Some(Action::SwitchVt(vt));
    }
    let symbol = match char::from_u32(keysym).filter(char::is_ascii_alphabetic) {
        Some(letter) => u32::from(letter.to_ascii_lowercase()),
        None => keysym,
    };
    BINDINGS
        .iter()
        .find(|bind| bind.syms.contains(&symbol) && (bind.when)(mods))
        .map(|bind| bind.action)
}

/// The key as the sheet shows it: modifiers, then the key.
pub fn key_label(bind: &Bind) -> String {
    let mut label = String::new();
    if bind.mods.logo {
        label.push_str("Super+");
    }
    if bind.mods.ctrl {
        label.push_str("Ctrl+");
    }
    if bind.mods.alt {
        label.push_str("Alt+");
    }
    if bind.mods.shift {
        label.push_str("Shift+");
    }
    label.push_str(bind.key);
    label
}

/// One section of the cheat sheet: what to press, and what it does.
pub type SheetSection = (Section, Vec<(String, &'static str)>);

/// The lines of the cheat sheet, by section. Made from `BINDINGS`, so
/// nothing here is written twice.
pub fn cheat_sheet() -> Vec<SheetSection> {
    let mut sections: Vec<SheetSection> = Vec::new();
    let mut add = |section: Section, line: (String, &'static str)| match sections
        .iter_mut()
        .find(|(known, _)| *known == section)
    {
        Some((_, lines)) => lines.push(line),
        None => sections.push((section, vec![line])),
    };
    for bind in BINDINGS.iter().filter(|bind| bind.listed) {
        add(bind.section, (key_label(bind), bind.what));
    }
    add(Section::Apps, (CONSOLE_KEYS.0.to_string(), CONSOLE_KEYS.1));
    sections
}

/// The tail of the volume commands: what the indicator shows is read from
/// the sink again, so it is what the keys really did. Skipped on a system
/// without it.
macro_rules! show_volume {
    () => {
        "command -v sengi-osd >/dev/null 2>&1 && exec sengi-osd volume"
    };
}

/// True for the keys of the keyboard's own media row. They change a level
/// and show it, which is no more than the lock screen may be told to do.
pub fn works_when_locked(action: Action) -> bool {
    matches!(action, Action::Volume(_) | Action::Brightness(_))
}

/// The shell command an action runs, for the ones that start a program.
pub fn command(action: Action) -> Option<&'static str> {
    match action {
        // The session's own launcher; fuzzel where it is not installed.
        Action::Launcher => Some(
            "if command -v sengi-launcher >/dev/null 2>&1; then exec sengi-launcher; else exec fuzzel; fi",
        ),
        // sengi-run puts the app in its own cgroup; plain foot will do on a
        // system without it.
        Action::Terminal => Some(
            "if command -v sengi-run >/dev/null 2>&1; then exec sengi-run foot; else exec foot; fi",
        ),
        // wpctl is what PipeWire's session manager ships; the sink is the
        // one the session chose.
        // Each then has the on-screen indicator show where it ended up.
        Action::Volume(Volume::Up) => Some(concat!(
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ 0; wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+; ",
            show_volume!()
        )),
        Action::Volume(Volume::Down) => Some(concat!(
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ 0; wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-; ",
            show_volume!()
        )),
        Action::Volume(Volume::Mute) => Some(concat!(
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle; ",
            show_volume!()
        )),
        Action::Brightness(Brightness::Up) => Some("exec sengi-brightness up"),
        Action::Brightness(Brightness::Down) => Some("exec sengi-brightness down"),
        Action::Brightness(Brightness::KeyboardUp) => Some("exec sengi-brightness kbd-up"),
        Action::Brightness(Brightness::KeyboardDown) => Some("exec sengi-brightness kbd-down"),
        Action::Brightness(Brightness::KeyboardToggle) => Some("exec sengi-brightness kbd-toggle"),
        Action::Screenshot(Shot::Screen) => Some("exec sengi-shot"),
        // The locker is configurable and tracked, see `State::spawn_locker`.
        // The window actions are carried out by `State::run_action`.
        Action::Lock
        | Action::CloseWindow
        | Action::NextWindow
        | Action::PreviousWindow
        | Action::Overlay(_)
        | Action::Shortcuts
        | Action::ClipboardHistory
        | Action::Maximise
        | Action::Snap(_)
        | Action::MoveToEdge(_)
        | Action::Screenshot(Shot::Window)
        | Action::SwitchVt(_) => None,
    }
}

/// Keys whose press triggered a binding. Their release must not reach the
/// client either, or it sees a key go up that never went down.
#[derive(Debug, Default)]
pub struct Swallowed {
    keys: Vec<u32>,
}

impl Swallowed {
    pub fn press(&mut self, keycode: u32) {
        if !self.keys.contains(&keycode) {
            self.keys.push(keycode);
        }
    }

    /// True if the release of `keycode` has to be swallowed as well.
    pub fn release(&mut self, keycode: u32) -> bool {
        let before = self.keys.len();
        self.keys.retain(|key| *key != keycode);
        self.keys.len() != before
    }
}

/// The next position when stepping through `len` windows.
pub fn cycle(len: usize, index: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGO: Mods = Mods {
        logo: true,
        alt: false,
        ctrl: false,
        shift: false,
    };
    const ALT: Mods = Mods {
        logo: false,
        alt: true,
        ctrl: false,
        shift: false,
    };

    #[test]
    fn super_a_maximises_and_super_arrows_snap() {
        assert_eq!(binding(LOGO, 'a' as u32), Some(Action::Maximise));
        assert_eq!(binding(LOGO, LEFT), Some(Action::Snap(Edge::Left)));
        assert_eq!(binding(LOGO, DOWN), Some(Action::Snap(Edge::Down)));
        // Alt with an arrow moves the window to that edge.
        assert_eq!(binding(ALT, RIGHT), Some(Action::MoveToEdge(Edge::Right)));
        let alt_shift = Mods { shift: true, ..ALT };
        assert_eq!(binding(alt_shift, RIGHT), None);
    }

    #[test]
    fn the_volume_keys_run_wpctl_whatever_the_modifiers() {
        assert_eq!(
            binding(Mods::default(), AUDIO_RAISE),
            Some(Action::Volume(Volume::Up))
        );
        assert_eq!(
            binding(LOGO, AUDIO_MUTE),
            Some(Action::Volume(Volume::Mute))
        );
        let up = command(Action::Volume(Volume::Up)).unwrap();
        assert!(up.contains("wpctl set-volume") && up.contains("5%+"));
    }

    #[test]
    fn the_brightness_keys_run_sengi_brightness_and_work_on_the_lock_screen() {
        let none = Mods::default();
        for (keysym, key, word) in [
            (MON_BRIGHTNESS_UP, Brightness::Up, "up"),
            (MON_BRIGHTNESS_DOWN, Brightness::Down, "down"),
            (KBD_BRIGHTNESS_UP, Brightness::KeyboardUp, "kbd-up"),
            (KBD_BRIGHTNESS_DOWN, Brightness::KeyboardDown, "kbd-down"),
            (KBD_LIGHT_TOGGLE, Brightness::KeyboardToggle, "kbd-toggle"),
        ] {
            let action = Action::Brightness(key);
            assert_eq!(binding(none, keysym), Some(action));
            assert_eq!(binding(LOGO, keysym), Some(action));
            assert_eq!(
                command(action),
                Some(&*format!("exec sengi-brightness {word}"))
            );
            assert!(works_when_locked(action));
        }
        assert!(works_when_locked(Action::Volume(Volume::Mute)));
        assert!(!works_when_locked(Action::Launcher));
        assert!(!works_when_locked(Action::Screenshot(Shot::Screen)));
    }

    #[test]
    fn the_volume_keys_show_the_indicator_after_the_change() {
        for volume in [Volume::Up, Volume::Down, Volume::Mute] {
            let text = command(Action::Volume(volume)).unwrap();
            let (change, show) = text.split_once("; command -v").unwrap();
            assert!(change.contains("wpctl"));
            assert!(show.contains("sengi-osd volume"));
        }
    }

    #[test]
    fn print_takes_the_screen_and_alt_or_shift_print_the_window() {
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        assert_eq!(
            binding(Mods::default(), PRINT),
            Some(Action::Screenshot(Shot::Screen))
        );
        assert_eq!(binding(ALT, PRINT), Some(Action::Screenshot(Shot::Window)));
        assert_eq!(
            binding(shift, PRINT),
            Some(Action::Screenshot(Shot::Window))
        );
        // xkb turns Alt+Print into Sys_Req.
        assert_eq!(
            binding(ALT, SYS_REQ),
            Some(Action::Screenshot(Shot::Window))
        );
        assert_eq!(
            binding(
                Mods {
                    ctrl: true,
                    ..Mods::default()
                },
                PRINT
            ),
            None
        );
        assert_eq!(
            command(Action::Screenshot(Shot::Screen)),
            Some("exec sengi-shot")
        );
        assert_eq!(command(Action::Screenshot(Shot::Window)), None);
    }

    #[test]
    fn super_bindings_match_the_labwc_configuration() {
        assert_eq!(binding(LOGO, SPACE), Some(Action::Launcher));
        assert_eq!(binding(LOGO, RETURN), Some(Action::Terminal));
        assert_eq!(binding(LOGO, 'l' as u32), Some(Action::Lock));
        assert_eq!(binding(LOGO, 'q' as u32), Some(Action::CloseWindow));
        // Caps lock must not disable them.
        assert_eq!(binding(LOGO, 'Q' as u32), Some(Action::CloseWindow));
    }

    #[test]
    fn vt_keys_switch_terminals_whatever_else_is_held() {
        assert_eq!(vt_for(SWITCH_VT_1), Some(1));
        assert_eq!(vt_for(SWITCH_VT_1 + 11), Some(12));
        assert_eq!(vt_for(SWITCH_VT_1 + 12), None);
        assert_eq!(vt_for('q' as u32), None);
        let held = Mods {
            ctrl: true,
            alt: true,
            ..Mods::default()
        };
        assert_eq!(binding(held, SWITCH_VT_1 + 1), Some(Action::SwitchVt(2)));
    }

    #[test]
    fn alt_tab_cycles_both_ways() {
        assert_eq!(binding(ALT, TAB), Some(Action::NextWindow));
        let shifted = Mods { shift: true, ..ALT };
        assert_eq!(binding(shifted, ISO_LEFT_TAB), Some(Action::PreviousWindow));
    }

    #[test]
    fn alt_f4_closes_like_super_q() {
        assert_eq!(binding(ALT, F4), Some(Action::CloseWindow));
        assert_eq!(binding(Mods::default(), F4), None);
        assert_eq!(binding(LOGO, F4), None);
        assert_eq!(binding(Mods { shift: true, ..ALT }, F4), None);
    }

    #[test]
    fn other_keys_and_modifier_mixes_pass_through() {
        assert_eq!(binding(Mods::default(), 'q' as u32), None);
        assert_eq!(binding(Mods::default(), TAB), None);
        assert_eq!(binding(LOGO, 'x' as u32), None);
        assert_eq!(binding(LOGO, TAB), None);
        assert_eq!(binding(Mods { ctrl: true, ..LOGO }, 'q' as u32), None);
        assert_eq!(
            binding(
                Mods {
                    shift: true,
                    ..LOGO
                },
                'Q' as u32
            ),
            None
        );
        assert_eq!(binding(Mods { ctrl: true, ..ALT }, TAB), None);
        assert_eq!(binding(ALT, SPACE), None);
    }

    #[test]
    fn programs_have_commands_and_window_actions_do_not() {
        assert!(command(Action::Launcher)
            .unwrap()
            .contains("exec sengi-launcher"));
        assert!(command(Action::Launcher).unwrap().contains("exec fuzzel"));
        assert!(command(Action::Terminal)
            .unwrap()
            .contains("sengi-run foot"));
        assert!(command(Action::Terminal).unwrap().contains("exec foot"));
        assert!(DEFAULT_LOCKER.contains("sengi-lock -f"));
        assert_eq!(command(Action::Lock), None);
        assert_eq!(command(Action::CloseWindow), None);
        assert_eq!(command(Action::NextWindow), None);
    }

    #[test]
    fn a_swallowed_press_swallows_its_release_once() {
        let mut swallowed = Swallowed::default();
        assert!(!swallowed.release(24));
        swallowed.press(24);
        swallowed.press(24);
        assert!(swallowed.release(24));
        assert!(!swallowed.release(24));
    }

    #[test]
    fn cycling_wraps_around() {
        assert_eq!(cycle(3, 0, true), 1);
        assert_eq!(cycle(3, 2, true), 0);
        assert_eq!(cycle(3, 0, false), 2);
        assert_eq!(cycle(1, 0, true), 0);
        assert_eq!(cycle(0, 0, true), 0);
    }

    #[test]
    fn every_row_of_the_table_does_what_it_says() {
        for bind in BINDINGS {
            assert!((bind.when)(bind.mods), "{} is not held right", bind.key);
            for sym in bind.syms {
                assert_eq!(
                    binding(bind.mods, *sym),
                    Some(bind.action),
                    "{} with {:?}",
                    key_label(bind),
                    sym
                );
            }
        }
    }

    /// Whatever key and modifiers are tried, an action that `binding`
    /// gives is one the sheet lists. A binding added outside the table
    /// would fail here.
    #[test]
    fn the_sheet_lists_every_action_a_key_can_trigger() {
        let listed: Vec<Action> = BINDINGS
            .iter()
            .filter(|bind| bind.listed)
            .map(|bind| bind.action)
            .collect();
        let symbols = (0x20..0x7f)
            .chain(0xfd00..0x10000)
            .chain(0x1008_fe00..0x1008_ff40);
        for symbol in symbols {
            for bits in 0..16 {
                let mods = Mods {
                    logo: bits & 1 != 0,
                    alt: bits & 2 != 0,
                    ctrl: bits & 4 != 0,
                    shift: bits & 8 != 0,
                };
                match binding(mods, symbol) {
                    None | Some(Action::SwitchVt(_)) => {}
                    Some(action) => assert!(
                        listed.contains(&action),
                        "{symbol:#x} with {mods:?} does {action:?}, which the sheet does not list"
                    ),
                }
            }
        }
    }

    #[test]
    fn the_sheet_is_the_table_in_words() {
        let sheet = cheat_sheet();
        let lines: Vec<&(String, &str)> = sheet.iter().flat_map(|(_, lines)| lines).collect();
        let listed = BINDINGS.iter().filter(|bind| bind.listed).count();
        assert_eq!(lines.len(), listed + 1, "the table and the console line");
        let has = |keys: &str, what: &str| lines.iter().any(|l| l.0 == keys && l.1.contains(what));
        assert!(has("Super+Space", "open apps"));
        assert!(has("Super+Left", "left half"));
        assert!(has("Alt+Shift+Tab", "previous window"));
        assert!(has("Super+/", "shortcuts"));
        assert!(has("Ctrl+Alt+F1 to F12", "console"));
        assert!(lines.iter().all(|l| !l.0.is_empty() && !l.1.is_empty()));
        // The same line is not shown twice.
        for (index, line) in lines.iter().enumerate() {
            assert!(!lines[..index].contains(line), "{line:?} twice");
        }
    }

    #[test]
    fn the_cheat_sheet_opens_on_super_slash_and_super_f1() {
        assert_eq!(binding(LOGO, u32::from(b'/')), Some(Action::Shortcuts));
        assert_eq!(binding(LOGO, 0xffbe), Some(Action::Shortcuts));
        assert_eq!(binding(Mods::default(), u32::from(b'/')), None);
        assert_eq!(binding(ALT, u32::from(b'/')), None);
        assert_eq!(command(Action::Shortcuts), None);
        assert!(!works_when_locked(Action::Shortcuts));
    }

    #[test]
    fn super_v_opens_the_clipboard_history_and_the_sheet_says_so() {
        assert_eq!(
            binding(LOGO, u32::from(b'v')),
            Some(Action::ClipboardHistory)
        );
        assert_eq!(
            binding(LOGO, u32::from(b'V')),
            Some(Action::ClipboardHistory)
        );
        assert_eq!(binding(Mods::default(), u32::from(b'v')), None);
        assert_eq!(binding(Mods { ctrl: true, ..LOGO }, u32::from(b'v')), None);
        assert_eq!(command(Action::ClipboardHistory), None);
        assert!(!works_when_locked(Action::ClipboardHistory));
        // The Cyrillic key where V is.
        assert_eq!(binding_symbol(0x06cd, 47 + 8), u32::from(b'v'));
        let sheet = cheat_sheet();
        assert!(sheet
            .iter()
            .flat_map(|(_, lines)| lines)
            .any(|line| line.0 == "Super+V" && line.1.contains("Clipboard history")));
    }

    #[test]
    fn letter_bindings_follow_the_key_on_other_alphabets() {
        // Cyrillic de is where L is; the kernel calls that key 38.
        let de = 0x06c4;
        assert_eq!(binding_symbol(de, 38 + 8), u32::from(b'l'));
        let logo = Mods {
            logo: true,
            ..Mods::default()
        };
        assert_eq!(binding(logo, binding_symbol(de, 46)), Some(Action::Lock));
        // A Latin layout that moves letters about keeps its own letters.
        assert_eq!(binding_symbol(u32::from(b'a'), 16 + 8), u32::from(b'a'));
        assert_eq!(binding_symbol(RETURN, 28 + 8), RETURN);
    }
}
