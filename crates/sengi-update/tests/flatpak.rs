//! The update of apps against a real flatpak and a real repository.
//!
//! It updates what is installed, so it runs only when asked for. HOME is to
//! be a throwaway directory whose user installation has updates waiting:
//!
//!     HOME=/tmp/throwaway SENGI_TEST_UPDATE=1 \
//!         cargo test -p sengi-update --test flatpak -- --ignored --nocapture

use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use sengi_update::client::{self, Control, Note, Outcome};
use sengi_update::{Event, Paths, Tools};

#[test]
#[ignore = "updates from a real repository"]
fn a_real_apps_update_reports_how_far_it_is() {
    if std::env::var_os("SENGI_TEST_UPDATE").is_none() {
        eprintln!("skipped: SENGI_TEST_UPDATE is not set");
        return;
    }
    // An empty root: not the live system, and no battery.
    let paths = Paths::under(&std::env::temp_dir().join("sengi-update-no-such-root"));
    let (_replies, from_us) = mpsc::channel();
    let control = Control {
        stop: Arc::new(AtomicBool::new(false)),
        replies: from_us,
    };
    let mut seen = Vec::new();
    let outcome = client::update_all(
        &Tools::system(),
        &paths,
        false,
        true,
        &control,
        &mut |note| {
            if let Note::Event(Event::Progress(p)) = note {
                println!(
                    "{:?} {:?} [{}] [{}]",
                    p.step,
                    p.percent,
                    p.size.as_deref().unwrap_or(""),
                    p.detail
                );
                seen.push(p);
            }
        },
    );
    assert!(matches!(outcome, Outcome::Done { .. }), "{outcome:?}");
    let last = seen.last().expect("progress");
    assert!(last.percent >= Some(90.0), "{last:?}");
    let of_last_step: Vec<f32> = seen
        .iter()
        .filter(|p| p.step == last.step)
        .filter_map(|p| p.percent)
        .collect();
    assert!(of_last_step.len() >= 8, "{of_last_step:?}");
    assert!(of_last_step[0] + 50.0 <= *of_last_step.last().unwrap());
    assert!(seen.iter().all(|p| p.size.is_some()));
}
