//! A never-flashed program that asks src/avr/hw.rs for power-down with PC1
//! high, which the firmware never does, so `sim::guards` can check it idles.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]
#![cfg_attr(target_arch = "avr", feature(abi_avr_interrupt))]

#[cfg(target_arch = "avr")]
use kr1k::regs;

#[cfg(target_arch = "avr")]
#[allow(dead_code)]
#[path = "../src/avr/hw.rs"]
mod hw;
#[cfg(target_arch = "avr")]
#[path = "../src/avr/rt.rs"]
mod rt;

#[cfg(target_arch = "avr")]
mod guards {
    use crate::hw;
    use kr1k::ramp::{self, Output};
    use kr1k::watchdog::Sleep;

    /// `sim::guards` counts on this.
    const LIT_TICKS: u8 = 4;

    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    extern "C" fn main() -> ! {
        hw::hold_enable_low();
        hw::init();
        hw::set_output(Output::STARTING);
        if hw::wait_for_tick(Sleep::Idle).is_none() {
            hw::halt();
        }
        hw::set_output(ramp::output(ramp::CEILING));
        for _ in 0..LIT_TICKS {
            if hw::wait_for_tick(Sleep::PowerDown).is_none() {
                hw::halt();
            }
        }
        hw::set_output(Output::OFF);
        if hw::wait_for_tick(Sleep::PowerDown).is_none() {
            hw::halt();
        }
        guards_done()
    }

    #[allow(unsafe_code, clippy::empty_loop)]
    #[unsafe(no_mangle)]
    #[inline(never)]
    #[expect(
        clippy::missing_const_for_fn,
        reason = "a named stopping point the simulator looks for, never evaluated at compile time"
    )]
    extern "C" fn guards_done() -> ! {
        loop {}
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        hw::halt()
    }
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!("this is a check of hw.rs for the AVR32DD20; `cargo xtask hex` runs it");
    std::process::exit(1);
}
