//! The firmware. Sample the switch, step the UI, set the output, 64 times a
//! second.
//!
//! On the host this builds to a stub, so `cargo test` and `cargo clippy`
//! work across the workspace. `cargo xtask hex` builds the real thing.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]
#![cfg_attr(target_arch = "avr", feature(abi_avr_interrupt))]
// The same restrictions as the library's, for the loop, the hardware layer
// and the startup code. Only the AVR clippy sees them. The xtask test
// `lints::restriction_lint_lists_match` keeps this list in step with the
// library's.
#![cfg_attr(
    target_arch = "avr",
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::as_conversions,
    )
)]

// The hardware layer and the startup code, which only the AVR build has.
#[cfg(target_arch = "avr")]
mod avr {
    pub mod hw;
    mod rt;
}
#[cfg(target_arch = "avr")]
use avr::hw;
// The library owns src/regs.rs, since its self-tests check fuse fields with
// the same masks; hw.rs and rt.rs reach it as `crate::regs`.
#[cfg(target_arch = "avr")]
use kr1k::regs;

// src/avr/rt.rs calls `main` by name once memory is set up.
#[cfg(target_arch = "avr")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    use kr1k::aw2016::Aw2016;
    use kr1k::selftest::{self, Faults};
    use kr1k::torch::{Changes, Torch};
    use kr1k::watchdog::Feeder;

    // PC1 floats from reset until it's an output.
    hw::hold_enable_low();

    let faults = Faults {
        hang: selftest::hung(hw::take_reset_flags()),
        flash: !hw::flash_ok(),
        fuses: !hw::fuses().ok(),
        supply: !hw::vddio2_ok(),
    };

    hw::init();
    hw::start_watchdog();

    let mut torch = Torch::new(hw::temperature_calibration(), faults);
    let mut output_changes = Changes::new();
    let mut aux = Aw2016::new();
    let mut feeder = Feeder::new();

    loop {
        // Power-down or idle, by `Feeder::power_down`'s rule, which the
        // simulator holds this loop to. False means the light's on and no
        // tick came in about 103 ms, so the 32 kHz oscillator has most likely
        // stopped, and the watchdog with it. The light goes off and stays off
        // until a reset.
        if !hw::wait_for_tick(feeder.power_down(torch.resting())) {
            hw::halt();
        }

        // VDDIO2 is read again on every pass, since PC1 is on it and lets go
        // of the driver's enable without it. The library puts the light out
        // on the tick it reads missing.
        torch.tick(hw::switch_pressed(), hw::adc_result(), hw::vddio2_ok());

        if let Some(convert) = torch.convert() {
            hw::adc_start(convert);
        }

        // The aux before the output, so they go dark before the light comes
        // on. They only light a tick after it goes out, since the AW2016
        // wakes a tick before it takes a colour. A step that fails is given
        // up on, never tried again until the colour changes.
        if let Some(update) = aux.step(torch.aux())
            && !hw::set_aux(update)
        {
            aux.failed();
        }

        if let Some(output) = output_changes.update(torch.output()) {
            hw::set_output(output);
        }

        // The only place the running torch feeds the watchdog.
        if feeder.tick() {
            hw::feed_watchdog();
        }
    }
}

// Turns the light off and powers down for good (`hw::halt`). The watchdog
// isn't fed there, so it resets the chip within 64 WDT clocks of the last
// feed, about 63 ms, and the next boot comes up as a fault, dark until the
// cell comes out.
//
// Spinning here would end the same way while the watchdog runs. `halt` is for
// when it doesn't, with the 32 kHz oscillator dead. No reset comes then, and
// a spin would keep the CPU running off the cell for good where `halt`
// sleeps. It's here because Rust requires a handler. `cargo xtask hex`
// refuses any image that could reach it (xtask/src/layout.rs), so nothing
// should ever run this.
#[cfg(target_arch = "avr")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    hw::halt()
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!("this is firmware for the AVR32DD20; build it with `cargo xtask hex`");
    std::process::exit(1);
}
