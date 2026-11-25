fn main() {
    let config =
        slint_build::CompilerConfiguration::new().with_include_paths(vec!["../../ui".into()]);
    slint_build::compile_with_config("ui/calendar.slint", config)
        .expect("ui/calendar.slint compiles");
    println!("cargo:rerun-if-changed=../../ui/components.slint");
}
