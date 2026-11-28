//! The Store's own calls against a real flatpak and a real repository.
//!
//! It installs an app, so it runs only when asked for. HOME is to be a
//! throwaway directory whose user installation has a remote named
//! `flathub` that serves the app slowly enough to be watched:
//!
//!     HOME=/tmp/throwaway SENGI_TEST_APP=org.example.App \
//!         cargo test -p sengi-store --test flatpak -- --ignored --nocapture
//!
//! With `SENGI_TEST_EXPECT=space` or `offline` the install is expected to
//! fail for that reason: fill the disk, or stop the server on the way.

use std::sync::Mutex;
use std::time::Instant;

use sengi_store::flatpak::{Cancel, Flatpak};
use sengi_store::problem::Problem;
use sengi_text::progress::Progress;

#[test]
#[ignore = "installs from a real repository"]
fn a_real_install_reports_how_far_it_is() {
    let Ok(app) = std::env::var("SENGI_TEST_APP") else {
        eprintln!("skipped: SENGI_TEST_APP is not set");
        return;
    };
    let expect = std::env::var("SENGI_TEST_EXPECT").unwrap_or_default();
    let seen = Mutex::new(Vec::<Progress>::new());
    let started = Instant::now();
    let report = |progress: Progress| {
        println!(
            "{:7.2}s  {:>4}  [{}]  [{}]",
            started.elapsed().as_secs_f32(),
            progress
                .percent
                .map(|p| format!("{p}%"))
                .unwrap_or_default(),
            progress.detail(),
            progress.pace()
        );
        seen.lock().unwrap().push(progress);
    };
    let result = Flatpak::default().install_with(&app, Some(&report), &Cancel::default());
    println!("{:7.2}s  {result:?}", started.elapsed().as_secs_f32());
    let seen = seen.into_inner().unwrap();

    match expect.as_str() {
        "space" => assert_eq!(Problem::of(&result.unwrap_err()), Problem::Space),
        "offline" => {
            let problem = Problem::of(&result.unwrap_err());
            assert!(
                matches!(problem, Problem::Offline | Problem::Slow),
                "{problem:?}"
            );
        }
        _ => {
            result.unwrap();
            let last = seen.last().expect("progress");
            assert!(last.percent >= Some(90), "{last:?}");
            let of_last_step: Vec<u8> = seen
                .iter()
                .filter(|p| p.step == last.step)
                .filter_map(|p| p.percent)
                .collect();
            assert!(of_last_step.len() >= 8, "{of_last_step:?}");
            assert!(
                of_last_step[0] + 50 <= *of_last_step.last().unwrap(),
                "{of_last_step:?}"
            );
            // Flatpak corrects itself by a few points now and then.
            assert!(
                of_last_step.windows(2).all(|w| w[1] + 15 >= w[0]),
                "{of_last_step:?}"
            );
            assert!(seen.iter().any(|p| p.speed.is_some()));
            assert!(seen.iter().any(|p| p.left.is_some()));
            assert!(seen
                .iter()
                .all(|p| p.size.as_ref().is_some_and(|s| s.starts_with("up to "))));
        }
    }
}
