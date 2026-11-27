fn main() {
    // The shared components, the text field of Settings' kit, and the text
    // area that is in ui/.
    let config = slint_build::CompilerConfiguration::new()
        .with_include_paths(vec!["../../ui".into(), "../sengi-settings/ui".into()]);
    slint_build::compile_with_config("ui/edit.slint", config).expect("ui/edit.slint compiles");
    println!("cargo:rerun-if-changed=../sengi-settings/ui/kit.slint");
    println!("cargo:rerun-if-changed=../sengi-settings/ui/icons.slint");
    println!("cargo:rerun-if-changed=../../ui/text-area.slint");
}
