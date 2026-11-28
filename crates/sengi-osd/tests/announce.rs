//! A volume key ends in `sengi-osd volume`: the bar has to hear of it.

use std::os::unix::net::UnixDatagram;
use std::process::Command;
use std::time::Duration;

#[test]
fn showing_the_volume_tells_the_bar() {
    let dir = std::env::temp_dir().join(format!("sengi-osd-announce-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bar = UnixDatagram::bind(dir.join("sengi-volume.sock")).unwrap();
    bar.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    // With no compositor the indicator itself cannot open, which is not
    // what is tested here.
    Command::new(env!("CARGO_BIN_EXE_sengi-osd"))
        .arg("volume")
        .env("XDG_RUNTIME_DIR", &dir)
        .env("PATH", &dir)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .output()
        .unwrap();

    let heard = bar.recv(&mut [0; 8]);
    let _ = std::fs::remove_dir_all(&dir);
    heard.expect("no notice reached the bar's socket");
}
