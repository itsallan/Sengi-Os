//! Every window of the desktop, built from the real `.slint` files and
//! drawn without a display. Nothing here ships: it is for looking at the
//! screens and for tests that check them.

pub mod harness;

macro_rules! window {
    ($name:ident) => {
        #[allow(clippy::all, unused)]
        pub mod $name {
            include!(concat!(env!("OUT_DIR"), "/", stringify!($name), ".rs"));
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
