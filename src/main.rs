//! The firmware. Sample the switch, step the UI, set the output, 64 times a
//! second, or once a second at rest (`kr1k::watchdog::Pacer`).
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
    use kr1k::watchdog::{End, Pacer};

    // PC1 floats from reset until it's an output.
    hw::hold_enable_low();
    // The supply readings for `cargo xtask peek` sit outside .bss, so
    // startup leaves them as SRAM had them. Nothing's been read yet.
    hw::record_supplies([0; 4]);

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
    let mut pacer = Pacer::new();
    let mut end = End::start(torch.resting());

    loop {
        // Power-down or idle, and whether a switch edge ends it, as the last
        // pass's end says, which the simulator holds this loop to. `None`
        // means the light's on and no tick came in about 103 ms, so the
        // 32 kHz oscillator has most likely stopped, and the watchdog with
        // it. The light goes off and stays off until a reset.
        let Some(wake) = hw::wait_for_tick(end.power_down, end.switch_wakes) else {
            hw::halt();
        };

        // A slow tick stands for a second of resting ticks, and a watchdog
        // setting to change goes in now, to sync while the pass runs.
        let begin = pacer.begin(wake);
        if let Some(setting) = begin.watchdog {
            hw::set_watchdog(setting);
        }
        torch.rest_for(begin.rested);

        // VDDIO2 is read again on every pass, since PC1 is on it and lets go
        // of the driver's enable without it. The library puts the light out
        // on the tick it reads missing.
        torch.tick(hw::switch_pressed(), hw::adc_result(), hw::vddio2_ok());

        if let Some(convert) = torch.convert() {
            hw::adc_start(convert);
        }

        // The cell and VDDIO2 as last read with the light off, kept for
        // `cargo xtask peek`. Nothing reads them back.
        hw::record_supplies(torch.supplies().to_bytes());

        // The aux before the output, so they go dark before the light comes
        // on. They only light a tick after it goes out, since the AW2016
        // wakes a tick before it takes a colour. A step that fails is given
        // up on, never tried again until the colour changes.
        if let Some(update) = aux.step(torch.aux())
            && !hw::set_aux(update)
        {
            aux.failed();
        }

        // Off unless the watchdog's lit window is in force, whatever the
        // torch asks for.
        if let Some(output) = output_changes.update(pacer.guard(torch.output())) {
            hw::set_output(output);
        }

        // The only place the running torch feeds the watchdog, after the
        // setting written at the start has synced, and before the PIT
        // changes period.
        end = pacer.end(
            torch.resting(),
            torch.can_slow() && aux.settled(torch.aux()),
        );
        if end.synced {
            hw::watchdog_synced();
        }
        if end.feed {
            hw::feed_watchdog();
        }
        if let Some(pit) = end.pit {
            hw::set_pit(pit);
        }
    }
}

// Turns the light off and powers down for good (`hw::halt`). The watchdog
// isn't fed there, so it resets the chip, within 64 WDT clocks of the last
// feed under the lit setting, about 63 ms, or within 8,192 under the rest
// setting, about 8 s, and the next boot comes up as a fault, dark until the
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
