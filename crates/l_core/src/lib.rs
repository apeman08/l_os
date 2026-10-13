#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod acpi;
pub mod anim;
pub mod cell;
pub mod dns;
pub mod elf;
pub mod font;
mod font_data;
pub mod frames;
pub mod http;
pub mod handles;
pub mod hotkey;
pub mod ipc;
pub mod memrange;
pub mod policy;
pub mod render;
pub mod shell;
pub mod syscall;
pub mod url;
pub mod usb_desc;
pub mod wipe;
pub mod wipetime;
pub mod xhci_ctx;
pub mod xhci_ring;
pub mod zeroize;
pub mod widgets;
pub mod theme;
pub mod time;
