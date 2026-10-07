#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![reexport_test_harness_main = "test_main"]
#![test_runner(l_os::test_runner)]

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    test_main();

    loop{}
}

fn test_runner(tests: &[&dyn Fn()]) {
    unimplemented!();
}



#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}


use l_os::println;

#[test_case]
fn test_println() {
    println!("test_println output");
}