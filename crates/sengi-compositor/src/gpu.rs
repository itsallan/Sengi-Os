//! Deciding whether to render with the GPU or on the CPU.
//!
//! GLES is only worth having when hardware backs it. On a display without
//! acceleration Mesa would hand us llvmpipe, which is slower than pixman
//! for what a compositor does and maps LLVM, over a hundred megabytes, into
//! the process. Asking Mesa is itself what loads it, so the question is put
//! to a short-lived child process (`--probe-gl`) and this process only ever
//! opens EGL once the answer is "hardware".

use crate::cli::Wanted;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    Gles,
    Pixman,
}

/// KMS drivers that never have a GPU behind them, so there is no point in
/// even asking.
const UNACCELERATED_DRIVERS: &[&str] = &[
    "bochs",
    "bochs-drm",
    "cirrus",
    "cirrus-qemu",
    "hyperv_drm",
    "simple-framebuffer",
    "simpledrm",
    "vboxvideo",
    "vkms",
];

/// Whether a GL renderer string names a software rasteriser.
pub fn is_software_gl(renderer: &str) -> bool {
    let renderer = renderer.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "swrast", "lavapipe", "software"]
        .iter()
        .any(|name| renderer.contains(name))
}

/// Picks the renderer and says why, for the log.
///
/// `driver` is the display device's kernel driver, `render_node` whether it
/// has a render node at all, and `probe` asks what GL would run on, which
/// is only called when nothing cheaper has settled it.
pub fn choose(
    wanted: Wanted,
    driver: Option<&str>,
    render_node: bool,
    probe: impl FnOnce() -> Option<String>,
) -> (Renderer, String) {
    match wanted {
        Wanted::Pixman => return (Renderer::Pixman, "asked for".into()),
        Wanted::Gles => return (Renderer::Gles, "asked for".into()),
        Wanted::Auto => {}
    }
    if !render_node {
        return (Renderer::Pixman, "the device has no render node".into());
    }
    if let Some(driver) = driver.filter(|driver| UNACCELERATED_DRIVERS.contains(driver)) {
        return (
            Renderer::Pixman,
            format!("the {driver} driver has no acceleration"),
        );
    }
    match probe() {
        None => (Renderer::Pixman, "GL could not be started".into()),
        Some(name) if is_software_gl(&name) => (
            Renderer::Pixman,
            format!("GL would run in software: {name}"),
        ),
        Some(name) => (Renderer::Gles, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> Option<String> {
        panic!("the probe must not run for this case");
    }

    #[test]
    fn software_rasterisers_are_recognised() {
        assert!(is_software_gl("llvmpipe (LLVM 19.1.7, 256 bits)"));
        assert!(is_software_gl("softpipe"));
        assert!(is_software_gl("Software Rasterizer"));
        assert!(!is_software_gl("Mesa Intel(R) HD Graphics 620 (KBL GT2)"));
        assert!(!is_software_gl(
            "virgl (Mesa Intel(R) UHD Graphics 620 (KBL GT2))"
        ));
    }

    #[test]
    fn an_explicit_choice_is_obeyed_without_probing() {
        assert_eq!(
            choose(Wanted::Pixman, Some("i915"), true, never).0,
            Renderer::Pixman
        );
        assert_eq!(
            choose(Wanted::Gles, Some("bochs-drm"), false, never).0,
            Renderer::Gles
        );
    }

    #[test]
    fn devices_that_cannot_be_accelerated_are_not_probed() {
        let (renderer, why) = choose(Wanted::Auto, Some("i915"), false, never);
        assert_eq!(renderer, Renderer::Pixman);
        assert!(why.contains("render node"));

        let (renderer, why) = choose(Wanted::Auto, Some("bochs-drm"), true, never);
        assert_eq!(renderer, Renderer::Pixman);
        assert!(why.contains("bochs-drm"));
    }

    #[test]
    fn hardware_gl_is_used_and_software_gl_refused() {
        let hardware = || Some("Mesa Intel(R) HD Graphics 620 (KBL GT2)".to_string());
        let (renderer, why) = choose(Wanted::Auto, Some("i915"), true, hardware);
        assert_eq!(renderer, Renderer::Gles);
        assert!(why.contains("HD Graphics 620"));

        // virtio-gpu has a render node with and without a GPU behind it.
        let software = || Some("llvmpipe (LLVM 19.1.7, 256 bits)".to_string());
        let (renderer, why) = choose(Wanted::Auto, Some("virtio_gpu"), true, software);
        assert_eq!(renderer, Renderer::Pixman);
        assert!(why.contains("llvmpipe"));

        let (renderer, _) = choose(Wanted::Auto, Some("virtio_gpu"), true, || None);
        assert_eq!(renderer, Renderer::Pixman);
    }
}
