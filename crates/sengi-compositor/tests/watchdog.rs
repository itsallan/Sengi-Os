//! The line the compositor leaves in its log when its main loop stalls.

mod common;
mod harness;

use std::time::{Duration, Instant};

use harness::Compositor;

#[test]
fn a_stalled_main_loop_is_reported_and_so_is_its_return() {
    let compositor = Compositor::start(None);
    assert!(compositor.said("watchdog").is_none());
    // Long enough for a look at the loop to fall inside it. The answer
    // comes only when the stall is over, so it is not waited for.
    compositor.control("stall 7000");
    let end = Instant::now() + Duration::from_secs(10);
    while compositor.said("answered again").is_none() {
        assert!(Instant::now() < end, "the loop's return was not reported");
        std::thread::sleep(Duration::from_millis(100));
    }
    let line = compositor
        .said("has not answered for")
        .expect("the stall was not reported");
    assert!(line.contains("main thread state"), "{line}");
}
