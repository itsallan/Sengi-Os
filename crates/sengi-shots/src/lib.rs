//! Every window of the desktop, built from the real `.slint` files and
//! drawn without a display. Nothing here ships: it is for looking at the
//! screens and for tests that check them.
//!
//! `SENGI_THEME=light` in the environment draws every window with the light
//! palette, and the pictures are saved with `-light` in their names.

pub mod harness;

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
                themed_as(window, $crate::harness::light())
            }

            /// The window with one palette or the other.
            pub fn themed_as<T>(window: T, light: bool) -> T
            where
                T: slint::ComponentHandle,
                for<'a> ThemeSwitch<'a>: slint::Global<'a, T>,
            {
                window.global::<ThemeSwitch>().set_light(light);
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
