//! The KR1K's first-light image, the first thing that goes on the torch.
//! Not the firmware. It turns the light on and off at one low level through
//! PC1, DAC0, its reference and the paths, with src/avr/hw.rs's own output
//! code, and nothing else. No watchdog, no sleep, no timer and no interrupt,
//! so there's very little in it to hang. A light that works shows the pads,
//! the programmer, the enable, the DAC and the low paths do what this repo
//! thinks. docs/breakout.md's stages 2 and 3 are for it.
//!
//! From reset it drives PC1 low (`hw::hold_enable_low`), then sets the
//! clock, the pins and DAC0 (`hw::init_pins`), which leaves the driver off,
//! with PC1 low, DAC0 on at 0 against 1.024 V, PD6's input buffer off and the
//! paths low. No blink at boot.
//!
//! Then it polls PD4, a millisecond at a time, by busy-waiting
//! (`hw::spin_ms`). It does nothing until the switch has read released for
//! [`ARMING_MS`] without a break, so a press while the cell goes in and the
//! head's done up can't light it, and nor can a switch held from reset. After
//! that each press, held for [`DEBOUNCE_MS`], toggles the light once, on
//! then off, and the switch has to read released for as long again before
//! the next press counts. A switch held down changes nothing more.
//!
//! On is the full firmware's order, PC1 up with DAC0 at 0 against 1.024 V and
//! the paths low (`hw::output_start`), a wait of at least [`BOOST_MS`], the
//! stock firmware's 8 ms, then [`LEVEL`] (`hw::output_level`). Off is the
//! full firmware's off (`hw::output_off`).
//!
//! The waits are counted the simulator's way, an instruction a clock, and
//! come out about a quarter longer on the chip, since `hw::spin_ms`'s
//! millisecond is about 1.25 ms there.
//!
//! The image is the code alone, short of the full 32 KB, with no CRC and no
//! stamp, since nothing here checks them. `cargo xtask hex` builds it, runs
//! it on the simulator (`sim::first_light`) and writes
//! `target/noctigon-kr1k-first-light.hex`. It has only ever run on the
//! simulator.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]
#![cfg_attr(target_arch = "avr", feature(abi_avr_interrupt))]

// hw.rs and rt.rs reach the library's registers as `crate::regs`.
#[cfg(target_arch = "avr")]
use kr1k::regs;

// Most of hw.rs goes unused here. rt.rs's vector table still links its
// handlers and `halt`, which nothing here enables or reaches.
#[cfg(target_arch = "avr")]
#[allow(dead_code)]
#[path = "../src/avr/hw.rs"]
mod hw;
#[cfg(target_arch = "avr")]
#[path = "../src/avr/rt.rs"]
mod rt;

/// The level it lights at, the firmware's floor.
///
/// That's ramp level 13, the low paths (PA7 and PA6) at code 92 against
/// 1.024 V. Dim, and well clear of the bottom of the ramp, so it can't be
/// mistaken for not lighting. `sim::first_light` lights it the same way.
pub const LEVEL: u8 = kr1k::ui::FLOOR;

/// How long PD4 has to read released, without a break, before a press
/// counts, from reset.
pub const ARMING_MS: u16 = 500;

/// How long a press or a release has to last, without a break, to count.
pub const DEBOUNCE_MS: u16 = 30;

/// How long the driver gets after PC1 rises before DAC0 leaves 0, the stock
/// firmware's 8 ms.
pub const BOOST_MS: u16 = 8;

#[cfg(target_arch = "avr")]
mod first_light {
    use crate::hw;
    use crate::{ARMING_MS, BOOST_MS, DEBOUNCE_MS, LEVEL};
    use kr1k::ramp::{self, Output};

    // src/avr/rt.rs calls `main` by name once memory is set up.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    extern "C" fn main() -> ! {
        hw::hold_enable_low();
        hw::init_pins();

        steady(false, ARMING_MS);
        let mut lit = false;
        loop {
            steady(true, DEBOUNCE_MS);
            if lit {
                hw::output_off();
            } else {
                hw::output_start(|| {});
                hw::spin_ms(BOOST_MS);
                hw::output_level(on());
            }
            lit = !lit;
            steady(false, DEBOUNCE_MS);
        }
    }

    /// The output for [`LEVEL`], which is never off.
    fn on() -> Output {
        ramp::output(LEVEL)
    }

    /// Polls PD4 a millisecond at a time until it's read `pressed` for `ms`
    /// polls in a row.
    fn steady(pressed: bool, ms: u16) {
        let mut held: u16 = 0;
        while held < ms {
            held = if hw::switch_pressed() == pressed {
                held.saturating_add(1)
            } else {
                0
            };
            hw::spin_ms(1);
        }
    }

    // Nothing here can panic, which `cargo xtask hex` checks as it does for
    // the firmware. Rust wants a handler all the same.
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        hw::halt()
    }
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!("this is the first-light image for the AVR32DD20; `cargo xtask hex` builds it");
    std::process::exit(1);
}
