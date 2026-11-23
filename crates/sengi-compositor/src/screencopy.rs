//! wlr-screencopy: a client asks for a picture of an output, which is how
//! `grim` and the tools built on it take screenshots.
//!
//! Smithay does not ship this protocol, so it is implemented here, in its
//! first version: the client is told what buffer to bring, brings one made
//! of shared memory, and gets the frame copied into it. The frame is drawn
//! once more into memory by the backend for the purpose; it is the picture
//! the screen shows, cursor included.
//!
//! Who may: nobody while the session is locked, when what is on screen is
//! the lock screen and what is behind it is nobody's to look at; and no
//! client that came in through a sandbox, ever. A program that runs as the
//! user without one can read the user's files anyway.

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            backend::GlobalId,
            protocol::{wl_buffer::WlBuffer, wl_shm::Format},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    wayland::shm::with_buffer_contents_mut,
};

use crate::state::State;

const VERSION: u32 = 1;

/// A picture of an output: width and height in pixels, then red, green
/// and blue bytes, top row first.
pub type Picture = (i32, i32, Vec<u8>);

/// Draws an output into memory, where the backend can.
pub type Capture = Box<dyn FnMut(&State, &Output) -> Option<Picture>>;

pub struct ScreencopyState {
    _global: GlobalId,
}

impl ScreencopyState {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<State, ZwlrScreencopyManagerV1, ()>(VERSION, ()),
        }
    }
}

/// What one frame is to show: the output, and the part of it in pixels,
/// left, top, width and height.
pub struct Frame {
    output: Output,
    part: (i32, i32, i32, i32),
    /// A frame is copied once; a second copy of it is an error.
    used: std::sync::atomic::AtomicBool,
    /// Whether the pointer is in the picture.
    overlay: bool,
}

/// The part of a `width` by `height` picture that a client's rectangle
/// covers, in pixels; `None` if it covers none of it. The rectangle is in
/// the output's own coordinates, which `scale` turns into pixels.
pub fn part_in_pixels(
    (width, height): (i32, i32),
    scale: f64,
    (x, y, w, h): (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let px = |v: i32| (f64::from(v) * scale).round() as i32;
    let (left, top) = (px(x).clamp(0, width), px(y).clamp(0, height));
    let right = px(x.saturating_add(w)).clamp(0, width);
    let bottom = px(y.saturating_add(h)).clamp(0, height);
    (right > left && bottom > top).then_some((left, top, right - left, bottom - top))
}

/// Copies the part `(left, top, width, height)` of a picture of red, green
/// and blue bytes, `picture_width` pixels wide, into XRGB8888 rows.
pub fn to_xrgb(rgb: &[u8], picture_width: i32, part: (i32, i32, i32, i32)) -> Vec<u8> {
    let (left, top, width, height) = part;
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in top..top + height {
        let start = ((row * picture_width + left) * 3) as usize;
        for px in rgb[start..start + (width * 3) as usize].as_chunks::<3>().0 {
            // Stored little-endian: blue, green, red, padding.
            out.extend_from_slice(&[px[2], px[1], px[0], 0xff]);
        }
    }
    out
}

impl State {
    fn begin_frame(
        &mut self,
        frame: New<ZwlrScreencopyFrameV1>,
        output: &smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
        rectangle: Option<(i32, i32, i32, i32)>,
        overlay: bool,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let output = Output::from_resource(output);
        let size = output
            .as_ref()
            .and_then(Output::current_mode)
            .map(|mode| (mode.size.w, mode.size.h));
        let part = match (&output, size, rectangle) {
            (Some(_), Some(size), None) => Some((0, 0, size.0, size.1)),
            (Some(output), Some(size), Some(rectangle)) => {
                let scale = output.current_scale().fractional_scale();
                part_in_pixels(size, scale, rectangle)
            }
            _ => None,
        };
        match (output, part) {
            // Not on the login screen, which nobody has logged in to.
            (Some(output), Some(part)) if !self.is_locked() && !self.login_screen => {
                let frame = data_init.init(
                    frame,
                    Some(Frame {
                        output,
                        part,
                        used: std::sync::atomic::AtomicBool::new(false),
                        overlay,
                    }),
                );
                let (width, height) = (part.2 as u32, part.3 as u32);
                frame.buffer(Format::Xrgb8888, width, height, width * 4);
            }
            // Locked, or nothing there to take a picture of.
            _ => data_init.init(frame, None).failed(),
        }
    }

    fn copy_frame(&mut self, wanted: &Frame, buffer: &WlBuffer) -> bool {
        // Locked, or on the login screen, since the frame was asked for.
        if self.is_locked() || self.login_screen {
            return false;
        }
        let Some(mut capture) = self.capture.take() else {
            return false;
        };
        // The pointer is in the picture only if the client asked for it.
        let drawn = std::mem::replace(&mut self.draw_cursor, wanted.overlay);
        let picture = capture(self, &wanted.output);
        self.draw_cursor = drawn;
        self.capture = Some(capture);
        let Some((width, height, rgb)) = picture else {
            return false;
        };
        let (left, top, w, h) = wanted.part;
        // The output may have changed size in between.
        if left + w > width || top + h > height || rgb.len() < (width * height * 3) as usize {
            return false;
        }
        let pixels = to_xrgb(&rgb, width, wanted.part);
        let written = with_buffer_contents_mut(buffer, |pool, length, data| {
            let fits = matches!(data.format, Format::Xrgb8888 | Format::Argb8888)
                && data.width == w
                && data.height == h
                && data.stride == w * 4
                && data.offset >= 0
                && data.offset as usize + pixels.len() <= length;
            if fits {
                // SAFETY: the range was checked to lie inside the pool,
                // which stays mapped for the length of this call. The
                // client may write there too; nothing here reads it.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        pixels.as_ptr(),
                        pool.add(data.offset as usize),
                        pixels.len(),
                    );
                }
            }
            fits
        });
        matches!(written, Ok(true))
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _: &()) -> bool {
        crate::security::trusted(&client)
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => {
                state.begin_frame(frame, &output, None, overlay_cursor != 0, data_init);
            }
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                output,
                overlay_cursor,
                x,
                y,
                width,
                height,
            } => {
                state.begin_frame(
                    frame,
                    &output,
                    Some((x, y, width, height)),
                    overlay_cursor != 0,
                    data_init,
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Option<Frame>> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        wanted: &Option<Frame>,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_screencopy_frame_v1::Request::Copy { buffer } = request {
            let Some(wanted) = wanted else {
                frame.failed();
                return;
            };
            if wanted.used.swap(true, std::sync::atomic::Ordering::SeqCst) {
                frame.post_error(
                    zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                    "this frame has been copied already",
                );
                return;
            }
            if !state.copy_frame(wanted, &buffer) {
                frame.failed();
                return;
            }
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: the kernel fills in the one struct given.
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
            let seconds = now.tv_sec as u64;
            frame.flags(Flags::empty());
            frame.ready((seconds >> 32) as u32, seconds as u32, now.tv_nsec as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_is_cut_to_the_picture_and_scaled_to_pixels() {
        assert_eq!(
            part_in_pixels((1280, 800), 1.0, (10, 20, 100, 50)),
            Some((10, 20, 100, 50))
        );
        // On a screen drawn twice as large, the same rectangle is twice
        // the pixels.
        assert_eq!(
            part_in_pixels((2560, 1600), 2.0, (10, 20, 100, 50)),
            Some((20, 40, 200, 100))
        );
        // Hanging over the edge, it is cut; beyond it, there is nothing.
        assert_eq!(
            part_in_pixels((1280, 800), 1.0, (1200, 700, 500, 500)),
            Some((1200, 700, 80, 100))
        );
        assert_eq!(part_in_pixels((1280, 800), 1.0, (2000, 0, 10, 10)), None);
        assert_eq!(part_in_pixels((1280, 800), 1.0, (0, 0, 0, 10)), None);
        assert_eq!(
            part_in_pixels((1280, 800), 1.0, (-5, -5, 10, i32::MAX)),
            Some((0, 0, 5, 800))
        );
    }

    #[test]
    fn pixels_come_out_as_xrgb_rows_of_the_part_asked_for() {
        // Two by two: red, green; blue, white.
        let rgb = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        assert_eq!(
            to_xrgb(&rgb, 2, (0, 0, 2, 2)),
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255]
        );
        // The right-hand column alone.
        assert_eq!(
            to_xrgb(&rgb, 2, (1, 0, 1, 2)),
            [0, 255, 0, 255, 255, 255, 255, 255]
        );
    }
}
