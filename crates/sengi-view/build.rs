fn main() {
    // The shared components.
    let config =
        slint_build::CompilerConfiguration::new().with_include_paths(vec!["../../ui".into()]);
    slint_build::compile_with_config("ui/view.slint", config).expect("ui/view.slint compiles");
}
