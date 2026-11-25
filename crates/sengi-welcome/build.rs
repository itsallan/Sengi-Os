fn main() {
    // The shared components, and Settings' kit of controls for a window.
    let config = slint_build::CompilerConfiguration::new()
        .with_include_paths(vec!["../../ui".into(), "../sengi-settings/ui".into()]);
    slint_build::compile_with_config("ui/welcome.slint", config)
        .expect("ui/welcome.slint compiles");
    println!("cargo:rerun-if-changed=../sengi-settings/ui/kit.slint");
    println!("cargo:rerun-if-changed=../sengi-settings/ui/icons.slint");
}
