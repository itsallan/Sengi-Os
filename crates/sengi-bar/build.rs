fn main() {
    let config =
        slint_build::CompilerConfiguration::new().with_include_paths(vec!["../../ui".into()]);
    slint_build::compile_with_config("ui/bar.slint", config).expect("ui/bar.slint compiles");
}
