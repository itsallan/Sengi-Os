//! Every window of the desktop, built from the real `.slint` files and
//! drawn without a display. Nothing here ships: it is for looking at the
//! screens and for tests that check them.
//!
//! `SENGI_THEME=light` in the environment draws every window with the light
//! palette, and the pictures are saved with `-light` in their names.

pub mod harness;

/// Gives a window's `ThemeSwitch` the palette, text size and motion the
/// environment asks for, as `apply_theme!` does from the settings in the
/// programs themselves: `apply_look!(window.global::<ThemeSwitch>())`.
#[macro_export]
macro_rules! apply_look {
    ($switch:expr) => {{
        let look = $crate::harness::Look::from_env();
        let switch = $switch;
        switch.set_light(look.light);
        switch.set_high_contrast(look.high_contrast);
        switch.set_text_scale(look.text_scale);
        switch.set_reduce_motion(look.reduce_motion);
    }};
}

macro_rules! window {
    ($name:ident) => {
        #[allow(clippy::all, unused)]
        pub mod $name {
            include!(concat!(env!("OUT_DIR"), "/", stringify!($name), ".rs"));

            /// The window with the palette the environment asks for.
            pub fn themed<T>(window: T) -> T
            where
                T: slint::ComponentHandle,
                for<'a> ThemeSwitch<'a>: slint::Global<'a, T>,
            {
                themed_look(window, $crate::harness::Look::from_env())
            }

            /// The window with one palette or the other, and the type
            /// size and motion the environment asks for.
            pub fn themed_as<T>(window: T, light: bool) -> T
            where
                T: slint::ComponentHandle,
                for<'a> ThemeSwitch<'a>: slint::Global<'a, T>,
            {
                let look = $crate::harness::Look::from_env();
                themed_look(
                    window,
                    $crate::harness::Look {
                        light,
                        high_contrast: false,
                        ..look
                    },
                )
            }

            /// The window as `look` says.
            pub fn themed_look<T>(window: T, look: $crate::harness::Look) -> T
            where
                T: slint::ComponentHandle,
                for<'a> ThemeSwitch<'a>: slint::Global<'a, T>,
            {
                let switch = window.global::<ThemeSwitch>();
                switch.set_light(look.light);
                switch.set_high_contrast(look.high_contrast);
                switch.set_text_scale(look.text_scale);
                switch.set_reduce_motion(look.reduce_motion);
                window
            }
        }
    };
}

window!(settings);
window!(installer);
window!(greeter);
window!(lock);
window!(quick);
window!(notify);
window!(bar);
window!(dock);
window!(store);
window!(launcher);
window!(files);
window!(desktop);
window!(osd);
window!(motion);
