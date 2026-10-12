#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod dns;
pub mod elf;
pub mod frames;
pub mod handles;
pub mod ipc;
pub mod policy;
pub mod shell;
pub mod syscall;
pub mod time;
