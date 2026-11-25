use std::path::PathBuf;

// Every window of the desktop, each compiled to a file of its own so that
// their names (two of them have a Row, three a DialogState) do not meet.
const WINDOWS: &[(&str, &str)] = &[
    ("settings", "../sengi-settings/ui/settings.slint"),
    ("installer", "../sengi-installer/ui/installer.slint"),
    ("greeter", "../sengi-greeter/ui/greeter.slint"),
    ("lock", "../sengi-lock/ui/lock.slint"),
    ("quick", "../sengi-quick/ui/quick.slint"),
    ("notify", "../sengi-notify/ui/notify.slint"),
    ("bar", "../sengi-bar/ui/bar.slint"),
    ("dock", "../sengi-dock/ui/dock.slint"),
    ("store", "../sengi-store/ui/store.slint"),
    ("launcher", "../sengi-launcher/ui/launcher.slint"),
    ("files", "../sengi-files/ui/files.slint"),
    ("desktop", "../sengi-desktop/ui/desktop.slint"),
    ("osd", "../sengi-osd/ui/osd.slint"),
];

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let here = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    for (name, path) in WINDOWS {
        let config = slint_build::CompilerConfiguration::new().with_include_paths(vec![
            here.join("../../ui"),
            here.join("../sengi-settings/ui"),
        ]);
        slint_build::compile_with_output_path(
            here.join(path),
            out.join(format!("{name}.rs")),
            config,
        )
        .unwrap_or_else(|error| panic!("{path} compiles: {error}"));
    }
    println!("cargo:rerun-if-changed=../../ui");
    for crate_dir in [
        "settings",
        "installer",
        "greeter",
        "lock",
        "quick",
        "notify",
        "bar",
        "dock",
        "store",
        "launcher",
        "files",
        "desktop",
        "osd",
    ] {
        println!("cargo:rerun-if-changed=../sengi-{crate_dir}/ui");
    }
}
