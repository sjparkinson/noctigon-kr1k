//! The firmware's loop. On the host it builds to a stub, so the workspace
//! builds and tests there.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]
#![cfg_attr(target_arch = "avr", feature(abi_avr_interrupt))]

#[cfg(target_arch = "avr")]
mod avr {
    pub mod hw;
    mod rt;
}
#[cfg(target_arch = "avr")]
use avr::hw;
#[cfg(target_arch = "avr")]
use kr1k::regs;

#[cfg(target_arch = "avr")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    use kr1k::aw2016::Aw2016;
    use kr1k::selftest::{self, Faults};
    use kr1k::torch::{Changes, Torch};
    use kr1k::watchdog::{End, Pacer};

    // First, since PC1 floats from reset until it's an output.
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
    let mut pacer = Pacer::new();
    let mut end = End::start(torch.resting());

    loop {
        // Most likely a stopped 32 kHz oscillator with the light on.
        let Some(wake) = hw::wait_for_tick(end.sleep) else {
            hw::halt();
        };

        // A watchdog setting goes in now, to sync while the pass runs.
        let begin = pacer.begin(wake);
        if let Some(setting) = begin.watchdog {
            hw::set_watchdog(setting);
        }
        torch.rest_for(begin.rested);

        // VDDIO2 is read on every pass, since PC1 lets go of the driver's
        // enable without it.
        torch.tick(
            hw::take_switch(),
            hw::adc_result(),
            hw::vddio2_ok(),
            pacer.lit(),
        );

        if let Some(convert) = torch.convert() {
            hw::adc_start(convert);
        }

        // The aux before the output, so they go dark before the light comes
        // on. A step that fails is never retried.
        if let Some(update) = aux.step(torch.aux())
            && !hw::set_aux(update)
        {
            aux.failed();
        }

        if let Some(output) = output_changes.update(torch.output()) {
            hw::set_output(output);
        }

        // The only feed, after the setting written at the start has synced
        // and before the PIT changes period.
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

// `halt` rather than a spin, which would drain the cell if the 32 kHz
// oscillator were dead and no watchdog reset came.
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
