fn main() {
    let config =
        slint_build::CompilerConfiguration::new().with_include_paths(vec!["../../ui".into()]);
    slint_build::compile_with_config("ui/launcher.slint", config)
        .expect("ui/launcher.slint compiles");
}
