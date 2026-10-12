//! The framebuffer the bootloader set up (UEFI GOP), taken over at boot.
//!
//! Its geometry is whatever the firmware chose: width, height, pixel format,
//! stride (in pixels) and bytes per pixel must always be read from
//! [`FrameBufferInfo`], never assumed. The memory itself goes to the console
//! ([`crate::fb_console`]), which is its only user; this module keeps the
//! description.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use l_core::render::{self, FbInfo};
use spin::Mutex;

/// Layout of the framebuffer, if the bootloader provided one (set once at boot).
static INFO: Mutex<Option<FrameBufferInfo>> = Mutex::new(None);

/// Record the bootloader's framebuffer and hand out its memory (once, to the
/// console). Called once from [`crate::init_memory`].
pub fn init(framebuffer: Option<FrameBuffer>) -> Option<&'static mut [u8]> {
    let framebuffer = framebuffer?;
    *INFO.lock() = Some(framebuffer.info());
    Some(framebuffer.into_buffer())
}

/// Layout of the framebuffer, or `None` if there is none.
pub fn info() -> Option<FrameBufferInfo> {
    *INFO.lock()
}

/// The layout in the renderer's terms, or `None` for pixel formats the
/// renderer cannot draw (8-bit grey, firmware-specific bit positions).
pub fn render_info(info: &FrameBufferInfo) -> Option<FbInfo> {
    let format = match info.pixel_format {
        PixelFormat::Rgb => render::PixelFormat::Rgb,
        PixelFormat::Bgr => render::PixelFormat::Bgr,
        _ => return None,
    };
    Some(FbInfo {
        width: info.width,
        height: info.height,
        stride_px: info.stride,
        bytes_per_px: info.bytes_per_pixel,
        format,
    })
}
