//! The framebuffer the bootloader set up (UEFI GOP), taken over at boot.
//!
//! Nothing is drawn on it yet (the framebuffer console comes later). Its
//! geometry is whatever the firmware chose: width, height, pixel format,
//! stride (in pixels) and bytes per pixel must always be read from
//! [`FrameBufferInfo`], never assumed.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo};
use spin::Mutex;

/// The framebuffer, if the bootloader provided one (set once during boot).
pub static FRAMEBUFFER: Mutex<Option<FrameBuffer>> = Mutex::new(None);

/// Store the bootloader's framebuffer. Called once from [`crate::init_memory`].
pub fn init(framebuffer: Option<FrameBuffer>) {
    *FRAMEBUFFER.lock() = framebuffer;
}

/// Layout of the framebuffer, or `None` if there is none.
pub fn info() -> Option<FrameBufferInfo> {
    FRAMEBUFFER.lock().as_ref().map(FrameBuffer::info)
}
