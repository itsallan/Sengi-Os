fn main() {
    // Compiling the gallery pulls in every component, so a broken component
    // or a missing token fails the build.
    slint_build::compile("../../ui/gallery.slint").expect("ui/gallery.slint compiles");
}
