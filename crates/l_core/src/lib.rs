#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod anim;
pub mod cell;
pub mod dns;
pub mod elf;
pub mod font;
mod font_data;
pub mod frames;
pub mod http;
pub mod handles;
pub mod ipc;
pub mod policy;
pub mod render;
pub mod shell;
pub mod syscall;
pub mod url;
pub mod widgets;
pub mod theme;
pub mod time;
