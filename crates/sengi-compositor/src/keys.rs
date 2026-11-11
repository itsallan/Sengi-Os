//! The built-in key bindings. They match what labwc is configured with in
//! the image, so the session behaves the same on either compositor.

/// Keysyms used by the bindings. Letters are their ASCII codes.
const SPACE: u32 = 0x0020;
const RETURN: u32 = 0xff0d;
const TAB: u32 = 0xff09;
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

/// What locks the session unless `--locker` says otherwise. The shell
/// replaces itself, so the locker has the pid we started.
pub const DEFAULT_LOCKER: &str = "exec swaylock -f";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Launcher,
    Terminal,
    Lock,
    CloseWindow,
    NextWindow,
    PreviousWindow,
    /// Ctrl+Alt+F1 to F12, which xkb turns into one keysym each.
    SwitchVt(i32),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Mods {
    pub logo: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// The action bound to a key press, if any. `keysym` is the symbol with the
/// held modifiers applied.
pub fn binding(mods: Mods, keysym: u32) -> Option<Action> {
    if let Some(vt) = vt_for(keysym) {
        return Some(Action::SwitchVt(vt));
    }
    let letter = char::from_u32(keysym)
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_lowercase());
    let only_logo = mods.logo && !mods.alt && !mods.ctrl && !mods.shift;
    let only_alt = mods.alt && !mods.logo && !mods.ctrl;

    if only_logo {
        return match (keysym, letter) {
            (SPACE, _) => Some(Action::Launcher),
            (RETURN, _) => Some(Action::Terminal),
            (_, Some('l')) => Some(Action::Lock),
            (_, Some('q')) => Some(Action::CloseWindow),
            _ => None,
        };
    }
    if only_alt && (keysym == TAB || keysym == ISO_LEFT_TAB) {
        return Some(if mods.shift {
            Action::PreviousWindow
        } else {
            Action::NextWindow
        });
    }
    None
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
        // The locker is configurable and tracked, see `State::spawn_locker`.
        Action::Lock
        | Action::CloseWindow
        | Action::NextWindow
        | Action::PreviousWindow
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
        assert!(DEFAULT_LOCKER.contains("swaylock -f"));
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
}
