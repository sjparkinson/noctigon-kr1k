//! `cargo xtask breakages` breaks the firmware on purpose, one mistake at a
//! time, and checks `cargo xtask hex` refuses each for the reason expected.
//!
//! Firmware that's right trips none of the simulator's alarms, nor
//! `layout.rs`'s, so a `hex` that passes says nothing about whether they can
//! still ring. sim.rs's tests ring the ones a test can reach by driving the
//! fake chip. The rest only go off with the real firmware running, and
//! `BREAKAGES` is a mistake for each kind of those.
//!
//! The checkout is never edited. The source is copied to a directory under
//! the system's temporary one, and `cargo xtask hex` runs there as a child
//! process, so it's the copy's own xtask judging the copy's firmware, built
//! into the copy's own `target`. First the copy as it is, which has to pass,
//! or a refusal further on would mean nothing. Then each breakage in turn,
//! in that one copy, so xtask and `core` are only built once. Each puts back
//! the file the last one broke, makes its one substitution, and wants `hex`
//! to fail at the stage it names with the text it names. The copy goes when
//! the run ends, pass or fail, though not if the run's killed.
//!
//! They run one after another. `hex` already runs its simulations side by
//! side, and a second copy would have to build xtask and `core` again, which
//! takes longer than every breakage put together. Most are refused in under
//! a second, before the simulator gets to its long runs.
//!
//! Like the simulator, this finds mistakes and proves nothing. A breakage
//! that's refused shows one alarm rings for one mistake. It doesn't show the
//! alarm rings for every mistake of that kind, and a mistake nobody thought
//! to list here is as unchecked as it was.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::Instant;

/// What gets copied, from the workspace root. It's what `cargo xtask hex`
/// needs to build xtask and the firmware, and nothing else.
const COPIED: [&str; 10] = [
    ".cargo",
    "Cargo.lock",
    "Cargo.toml",
    "build.rs",
    "examples",
    "link.x",
    "rust-toolchain.toml",
    "src",
    "vendor",
    "xtask",
];

/// Directories that are never copied, wherever they turn up.
const SKIPPED: [&str; 2] = ["target", ".git"];

/// How far `cargo xtask hex` got before it refused the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// The firmware didn't build. For the breakages here, that's the linker.
    Build,
    /// `layout.rs` refused the linked image.
    Layout,
    /// `sim.rs` refused it, running.
    Simulator,
}

/// One deliberate mistake.
struct Breakage {
    name: &'static str,
    /// The file to break, from the workspace root.
    file: &'static str,
    /// Text that's in `file` exactly once.
    find: &'static str,
    /// What goes in its place.
    replace: &'static str,
    /// Where `hex` has to stop.
    stage: Stage,
    /// Text `hex`'s output has to have in it.
    expect: &'static str,
}

const HW: &str = "src/avr/hw.rs";
const MAIN: &str = "src/main.rs";
const AW2016: &str = "src/aw2016.rs";
const LIMITS: &str = "src/limits.rs";
const BATTERY: &str = "src/battery.rs";
const SENSORS: &str = "src/sensors.rs";
const TORCH: &str = "src/torch.rs";

/// The mistakes, each one a different alarm. Every one of them has been run
/// and seen refused for the reason it gives, and `cargo xtask breakages`
/// sees to it that stays true.
const BREAKAGES: &[Breakage] = &[
    // PC1 only rises with DAC0 at 0. DAC0 above 0 with PC1 low is refused
    // first, which is as close as a breakage gets, since the DAC can't be
    // live before PC1 rises without that going off.
    Breakage {
        name: "dac-live-before-pc1",
        file: HW,
        find: "    just_before();\n    out!(reg::VPORTC_OUT, 1 << ENABLE);\n",
        replace: "    just_before();\n    write16(reg::DAC0_DATA, 1 << DAC_SHIFT);\n    \
                  out!(reg::VPORTC_OUT, 1 << ENABLE);\n",
        stage: Stage::Simulator,
        expect: "sets DAC0 to 1 with PC1 low",
    },
    // Turning off: DAC0 to 0 before PC1 drops.
    Breakage {
        name: "pc1-down-with-dac-live",
        file: HW,
        find: "pub fn output_off() {\n    write16(reg::DAC0_DATA, 0);\n",
        replace: "pub fn output_off() {\n",
        stage: Stage::Simulator,
        expect: "drops PC1 with DAC0 still at",
    },
    // The level comes a tick after PC1 rises. This sets it in the same pass,
    // by asking `set_output` twice with the level, never `Output::STARTING`.
    Breakage {
        name: "dac-before-the-boost-starts",
        file: MAIN,
        find: "        if let Some(output) = output_changes.update(torch.output()) {\n            \
               hw::set_output(output);\n",
        replace: "        if let Some(output) = \
                  output_changes.update(kr1k::ramp::output(torch.level())) {\n            \
                  hw::set_output(output);\n            hw::set_output(output);\n",
        stage: Stage::Simulator,
        expect: "the stock firmware gives the boost converter",
    },
    // A reference or a path only goes up with DAC0 at 0.
    Breakage {
        name: "path-change-with-dac-live",
        file: HW,
        find: "        write16(reg::DAC0_DATA, 0);\n        spin(DAC_FALLS);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "with PC1 high and DAC0 at",
    },
    // Coming down too, since how the paths combine is unknown. This drops
    // the paths first at turn-off, with DAC0 still at the level's code.
    Breakage {
        name: "paths-off-before-dac",
        file: HW,
        find: "pub fn output_off() {\n    write16(reg::DAC0_DATA, 0);\n",
        replace: "pub fn output_off() {\n    out!(reg::VPORTA_OUT, 0);\n    \
                  write16(reg::DAC0_DATA, 0);\n",
        stage: Stage::Simulator,
        expect: "off with PC1 high and DAC0 at",
    },
    // ... and with DAC0's output given time to get there.
    Breakage {
        name: "dac-fall-not-waited",
        file: HW,
        find: "        spin(DAC_FALLS);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "table 38-23",
    },
    Breakage {
        name: "pc1-pull-up",
        file: HW,
        find: "    pin_ctrl(reg::PORTC_PIN0CTRL, ENABLE, 0);\n",
        replace: "    pin_ctrl(reg::PORTC_PIN0CTRL, ENABLE, regs::PORT_PULLUPEN);\n",
        stage: Stage::Simulator,
        expect: "gives PC1 a pull-up",
    },
    Breakage {
        name: "switch-pull-up",
        file: HW,
        find: "    pin_ctrl(reg::PORTD_PIN0CTRL, SWITCH, regs::PORT_ISC_BOTHEDGES);\n",
        replace: "    pin_ctrl(\n        reg::PORTD_PIN0CTRL,\n        SWITCH,\n        \
                  regs::PORT_ISC_BOTHEDGES | regs::PORT_PULLUPEN,\n    );\n",
        stage: Stage::Simulator,
        expect: "gives PD4 a pull-up",
    },
    // PD6's input buffer goes off before DAC0 drives it.
    Breakage {
        name: "pd6-input-left-on",
        file: HW,
        find: "    pin_ctrl(reg::PORTD_PIN0CTRL, DAC, regs::PORT_ISC_INPUT_DISABLE);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "PD6's input buffer still on",
    },
    // PA2 and PA3 are the AW2016's, and never driven.
    Breakage {
        name: "twi-line-driven",
        file: HW,
        find: "const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW;\n\
               const _: () = assert!(PORTA_OUTPUTS == 0xF2);\n",
        replace: "const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW | 1 << 2;\n",
        stage: Stage::Simulator,
        expect: "PA2 and PA3 are the AW2016's TWI lines",
    },
    // PA1 goes to something unknown, and the stock build on this torch holds
    // it as an output at 0, so it never gets the stock source's pull-up back.
    Breakage {
        name: "pa1-pull-up",
        file: HW,
        find: "    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, 0);\n",
        replace: "    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, regs::PORT_PULLUPEN);\n",
        stage: Stage::Simulator,
        expect: "gives PA1 a pull-up",
    },
    // ... and is never driven high.
    Breakage {
        name: "pa1-driven-high",
        file: HW,
        find: "    out!(reg::VPORTA_OUT, 0);\n    out!(reg::VPORTA_DIR, PORTA_OUTPUTS);\n",
        replace: "    out!(reg::VPORTA_OUT, 1 << HELD_LOW);\n    \
                  out!(reg::VPORTA_DIR, PORTA_OUTPUTS);\n",
        stage: Stage::Simulator,
        expect: "setting PA1",
    },
    // Errata 2.2.3: a store under 64 straight after one at 64 or over is
    // lost, so the VPORTs and GPRs take `out`. This is every `out!` as the
    // ATtiny1616 firmware would have written it, an `sts`. A plain `write`
    // to a VPORT doesn't show it, since LLVM emits `out` for any constant
    // address under 64 itself.
    Breakage {
        name: "sts-to-a-vport",
        file: HW,
        find: "                \"out {addr}, {value}\",\n",
        replace: "                \"sts {addr}, {value}\",\n",
        stage: Stage::Simulator,
        expect: "straight after a store to 64 or over",
    },
    // ... and SLPCTRL.CTRLA gets a `nop` before it. This puts a store at 64
    // or over there instead, as a store before `sleep_mode` would be if the
    // `nop` went and the compiler put nothing between.
    Breakage {
        name: "sleep-mode-without-nop",
        file: HW,
        find: "            \"nop\",\n            \"sts {addr}, {value}\",\n",
        replace: "            \"sts {addr}, {value}\",\n            \"sts {addr}, {value}\",\n",
        stage: Stage::Simulator,
        expect: "SLPCTRL.CTRLA straight after a store to 64 or over",
    },
    // `prepare_sleep` idles rather than power down with PC1 high, whatever
    // it's asked. The firmware never asks, so examples/guards.rs does.
    Breakage {
        name: "power-down-with-the-light-on",
        file: HW,
        find: "    let mode = if rest && !lit() {\n",
        replace: "    let mode = if rest {\n",
        stage: Stage::Simulator,
        expect: "powers down with PC1 high",
    },
    Breakage {
        name: "adc-on-in-power-down",
        file: HW,
        find: "        write(reg::ADC0_CTRLA, 0);\n        regs::SLPCTRL_SMODE_PDOWN\n",
        replace: "        regs::SLPCTRL_SMODE_PDOWN\n",
        stage: Stage::Simulator,
        expect: "powers down with ADC0 still enabled",
    },
    // TCB0, the deadline's timer, stops with the light.
    Breakage {
        name: "tcb0-left-running",
        file: HW,
        find: "        write(reg::TCB0_CTRLA, 0);\n        write(reg::TCB0_INTCTRL, 0);\n",
        replace: "        write(reg::TCB0_INTCTRL, 0);\n",
        stage: Stage::Simulator,
        expect: "powers down with TCB0 running",
    },
    // With the light on and no tick, the wait gives up and `main` halts.
    // This count never runs out.
    Breakage {
        name: "no-tick-deadline",
        file: HW,
        find: "                if periods == 0 {\n                    break None;\n",
        replace: "                if periods == 0 {\n                    \
                  periods = TICK_DEADLINE_PERIODS;\n",
        stage: Stage::Simulator,
        expect: "past the deadline",
    },
    // `hw::halt` clears SREG.I as well as turning every interrupt off, since
    // the datasheet doesn't say either alone keeps the chip down.
    Breakage {
        name: "halt-without-cli",
        file: HW,
        find: "    unsafe { asm!(\"cli\", options(nostack)) };\n    set_output(Output::OFF);\n",
        replace: "    set_output(Output::OFF);\n",
        stage: Stage::Simulator,
        expect: "stops for good with SREG.I still set",
    },
    // `hw::halt` is where the firmware ends up when it can't go on, and it
    // has to leave the light off. This one still turns TCB0's interrupt off,
    // so the `sleep` is one the chip would take, with PC1 high, and it's in
    // idle, since `prepare_sleep` won't power down with the light on.
    Breakage {
        name: "halt-leaves-the-light-on",
        file: HW,
        find: "    unsafe { asm!(\"cli\", options(nostack)) };\n    set_output(Output::OFF);\n",
        replace: "    unsafe { asm!(\"cli\", options(nostack)) };\n    \
                  write(reg::TCB0_INTCTRL, 0);\n",
        stage: Stage::Simulator,
        expect: "sleeps with TCB0 running and PC1 high",
    },
    // PC1 is cleared in OUT before it's made an output.
    Breakage {
        name: "dir-before-out",
        file: HW,
        find: "    out!(reg::VPORTC_OUT, 0);\n    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);\n}\n",
        replace: "    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);\n    out!(reg::VPORTC_OUT, 0);\n}\n",
        stage: Stage::Simulator,
        expect: "starting with PC1 high: the first register it writes leaves PC1 set",
    },
    // The reset flags are read and cleared before anything can reset the
    // chip again.
    Breakage {
        name: "watchdog-before-reset-flags",
        file: MAIN,
        find: "    let faults = Faults {\n        \
               hang: selftest::hung(hw::take_reset_flags()),\n        \
               flash: !hw::flash_ok(),\n        \
               fuses: !hw::fuses().ok(),\n        \
               supply: !hw::vddio2_ok(),\n    };\n\n    \
               hw::init();\n    hw::start_watchdog();\n",
        replace: "    hw::init();\n    hw::start_watchdog();\n\n    \
                  let faults = Faults {\n        \
                  hang: selftest::hung(hw::take_reset_flags()),\n        \
                  flash: !hw::flash_ok(),\n        \
                  fuses: !hw::fuses().ok(),\n        \
                  supply: !hw::vddio2_ok(),\n    };\n",
        stage: Stage::Simulator,
        expect: "start-up order: turns the watchdog on before clearing the reset flags",
    },
    // Without VDDIO2, PC1 is tri-stated, so the light stays off as a fault.
    // This reads MVIO.STATUS at power-on and ignores it. The first pass of
    // the loop still finds it missing, so nothing lights, but it's a pass
    // late, and the torch reads its cell as if nothing were wrong where one
    // that failed the check rests. That's what's refused.
    Breakage {
        name: "vddio2-ignored",
        file: MAIN,
        find: "        supply: !hw::vddio2_ok(),\n",
        replace: "        supply: !hw::vddio2_ok() && false,\n",
        stage: Stage::Simulator,
        expect: "with VDDIO2 missing: tick",
    },
    // VDDIO2 is read again on every pass, since it can go with the light on.
    // This reads it at power-on and never again, as the firmware used to.
    Breakage {
        name: "vddio2-not-watched",
        file: MAIN,
        find: "            hw::vddio2_ok(),\n",
        replace: "            true,\n",
        stage: Stage::Simulator,
        expect: "began with MVIO.STATUS.VDDIO2S clear",
    },
    // ... and the light's out in the pass that finds it gone, with no fade,
    // since PC1 isn't holding the driver through one. This fades it out like
    // any other fault. The library and the firmware agree on that, being the
    // same code, so it's the chip's own rule that objects.
    Breakage {
        name: "vddio2-loss-fades-out",
        file: LIMITS,
        find: "        let fault = self.fault && (cut || self.level <= 1);\n",
        replace: "        let fault = self.fault && self.level <= 1;\n",
        stage: Stage::Simulator,
        expect: "began with MVIO.STATUS.VDDIO2S clear",
    },
    Breakage {
        name: "watchdog-never-fed",
        file: MAIN,
        find: "            hw::feed_watchdog();\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "without wdr",
    },
    Breakage {
        name: "watchdog-fed-every-tick",
        file: MAIN,
        find: "        if end.feed {\n",
        replace: "        if end.feed || true {\n",
        stage: Stage::Simulator,
        expect: "closed window",
    },
    // The lit setting goes back before the light can come on. This never
    // writes it after a rest, so the light comes on under the rest setting.
    Breakage {
        name: "lit-watchdog-never-restored",
        file: MAIN,
        find: "            hw::set_watchdog(setting);\n",
        replace: "            if setting == kr1k::watchdog::Setting::Rest {\n                \
                  hw::set_watchdog(setting);\n            }\n",
        stage: Stage::Simulator,
        expect: "raises PC1 with WDT.CTRLA at 0x0b",
    },
    // `Torch::tick` keeps the light at level 0 outside the lit window. Taking
    // that out alone changes nothing today, since only a click or a hold
    // lights the torch. This takes it out and adds the mistake it's there
    // for, a way to light that doesn't wait for a click, here level 1 for as
    // long as the switch is down. A press at rest asks for it on the pass
    // that wakes on it, under the rest setting, and a switch held from
    // power-on on the first pass, before the lit setting's first `wdr`.
    Breakage {
        name: "unguarded-light-at-rest",
        file: TORCH,
        find: "            let asked = if lit_window {\n                \
               self.ui.output_level()\n            } else {\n                0\n            };\n",
        replace: "            let _ = lit_window;\n            \
                  let asked = self.ui.output_level().max(u8::from(pressed));\n",
        stage: Stage::Simulator,
        expect: "the light only comes on with the lit window in force and a wdr since it was written",
    },
    // A write to WDT.CTRLA syncs before the sleep or the `wdr` that follows
    // it. The pass that goes to rest powers down after its write, and that's
    // refused first.
    Breakage {
        name: "watchdog-write-not-waited-for",
        file: MAIN,
        find: "        if end.synced {\n            hw::watchdog_synced();\n        }\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "powers down with WDT.STATUS.SYNCBUSY still set",
    },
    // A feed at rest is a trip back to the fast tick, never on the pass that
    // changes the PIT's period, whose sleep can be power-down.
    Breakage {
        name: "fed-as-the-pit-changes",
        file: MAIN,
        find: "        if end.feed {\n",
        replace: "        if end.feed || end.pit.is_some() {\n",
        stage: Stage::Simulator,
        expect: "the library feeds the watchdog 0 times on this tick, but the firmware 1",
    },
    // The PIT goes back to 64 Hz whenever the torch isn't at rest. This
    // never puts it back.
    Breakage {
        name: "pit-left-slow",
        file: MAIN,
        find: "        if let Some(pit) = end.pit {\n",
        replace: "        if let Some(pit @ kr1k::watchdog::Pit::Slow) = end.pit {\n",
        stage: Stage::Simulator,
        expect: "the library has the PIT Fast, but the firmware Slow",
    },
    // With the PIT slow, a switch edge runs a pass on the press. This sleeps
    // through it until the next slow tick.
    Breakage {
        name: "switch-wake-dropped",
        file: HW,
        find: "            if switch_wakes {\n",
        replace: "            if switch_wakes && false {\n",
        stage: Stage::Simulator,
        expect: "the firmware ran 0 passes on this tick, but the library 1",
    },
    // A pass forgets the switch edges it reads the switch after. This leaves
    // them up, so an edge just after a slow tick ends the slow sleep after
    // that tick's pass at once, for nothing the pass hadn't seen.
    Breakage {
        name: "stale-switch-edge",
        file: HW,
        find: "    out!(reg::GPR_GPR2, 0);\n    let pressed = switch_pressed();\n",
        replace: "    let pressed = switch_pressed();\n",
        stage: Stage::Simulator,
        expect: "resting, with an edge on every slow tick: tick",
    },
    // 16-bit registers go low byte first.
    Breakage {
        name: "high-byte-first",
        file: HW,
        find: "    write(addr, lo);\n    write(addr + 1, hi);\n",
        replace: "    write(addr + 1, hi);\n    write(addr, lo);\n",
        stage: Stage::Simulator,
        expect: "writes the high byte of DAC0.DATA without its low byte",
    },
    // A protected register's write comes straight after its unlock.
    Breakage {
        name: "unlock-apart-from-its-write",
        file: HW,
        find: "            \"out {ccp}, {key}\",\n",
        replace: "            \"out {ccp}, {key}\",\n            \"nop\",\n",
        stage: Stage::Layout,
        expect: "isn't followed by `st Z`",
    },
    // A handler never touches a 16-bit register of a peripheral the main
    // loop might be halfway through. This one zeroes DAC0 from the switch's
    // handler, which a bounce lands between `set_output`'s two bytes.
    Breakage {
        name: "handler-races-temp",
        file: HW,
        find: "pub extern \"avr-interrupt\" fn portd_port() {\n",
        replace: "pub extern \"avr-interrupt\" fn portd_port() {\n    write16(reg::DAC0_DATA, 0);\n",
        stage: Stage::Simulator,
        expect: "share its TEMP byte",
    },
    // A handler gives back every register as it found it.
    Breakage {
        name: "handler-clobbers-a-register",
        file: HW,
        find: "    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);\n    out!(reg::GPR_GPR0, TICK);\n",
        replace: "    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);\n    \
                  unsafe { asm!(\"inc r2\", options(nostack)) };\n    \
                  out!(reg::GPR_GPR0, TICK);\n",
        stage: Stage::Simulator,
        expect: "the handler for RTC_PIT (vector 6): returns with r2",
    },
    // Nothing in the firmware can panic. This is the check CONTRIBUTING.md
    // wants shown to fire on every new nightly, since it goes by the names
    // of symbols.
    Breakage {
        name: "bounds-check",
        file: MAIN,
        find: "        torch.tick(\n            hw::take_switch(),\n            hw::adc_result(),\n",
        replace: "        let reading = hw::adc_result();\n        \
                  let pressed = [hw::take_switch(); 4][usize::from(reading.unwrap_or(0))];\n        \
                  torch.tick(\n            pressed,\n            reading,\n",
        stage: Stage::Layout,
        expect: "panic_bounds_check",
    },
    // No 32-bit multiply, which dividing a `u16` by ten turns into.
    Breakage {
        name: "wide-multiply",
        file: HW,
        find: "    Some(u16::from_le_bytes([lo, hi]))\n",
        replace: "    Some(u16::from_le_bytes([lo, hi]) / 10)\n",
        stage: Stage::Build,
        expect: "undefined symbol: __mulsi3",
    },
    // Every wait on TWI0 counts down the update's budget. This one waits for
    // WIF for ever, which a held-down SCL never raises.
    Breakage {
        name: "twi-unbounded-wait",
        file: HW,
        find: "        match polls.checked_sub(1) {\n            Some(left) => *polls = left,\n            \
               None => return Sent::TimedOut,\n        }\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "with SCL held low: tick",
    },
    // The aux go dark before the light comes on. This keeps off's blue
    // wanted while the light's on, so PC1 rises with it lit.
    Breakage {
        name: "aux-lit-while-lit",
        file: MAIN,
        find: "aux.step(torch.aux())",
        replace: "aux.step(if torch.output().enable { kr1k::ui::Aux::Blue } else { torch.aux() })",
        stage: Stage::Simulator,
        expect: "raises PC1 with the aux lit",
    },
    // Off is standby. This leaves CHIPEN set, the AW2016 awake at 100 µA.
    Breakage {
        name: "aux-off-not-standby",
        file: AW2016,
        find: "        data: &[CHGDIS],\n",
        replace: "        data: &[CHGDIS | CHIPEN],\n",
        stage: Stage::Simulator,
        expect: "isn't in standby",
    },
    // A failed update is given up until the colour changes. Without this,
    // the next tick carries on as if it had gone through.
    Breakage {
        name: "aux-retried-on-the-next-tick",
        file: MAIN,
        find: "            aux.failed();\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "tries the AW2016 again",
    },
    // A cell under the hard floor puts the light out within 12 ticks. This
    // wants four readings in a row, which the worst tick of the round can't
    // get inside that.
    Breakage {
        name: "collapse-waits",
        file: BATTERY,
        find: "pub const COLLAPSE_READINGS: u8 = 2;\n",
        replace: "pub const COLLAPSE_READINGS: u8 = 4;\n",
        stage: Stage::Simulator,
        expect: "with a collapsing cell: the cell falls on tick",
    },
    // ... and reads the cell between the four-a-second samples to do it.
    // This drops the quick readings, so it's down to those four.
    Breakage {
        name: "no-quick-readings",
        file: SENSORS,
        find: "            && lit\n            && self.set_up == Some(Channel::Voltage)\n",
        replace: "            && lit\n            && false\n",
        stage: Stage::Simulator,
        expect: "A cell that collapses: expected Off",
    },
    // An ADC that reads ground as anything but near zero is a fault. This
    // lets any ground reading through, so an ADC stuck on a good cell's
    // reading hides a flat cell for good.
    Breakage {
        name: "ground-ignored",
        file: SENSORS,
        find: "        if raw <= GROUND_MAX {\n",
        replace: "        if raw <= GROUND_MAX || true {\n",
        stage: Stage::Simulator,
        expect: "with the ADC stuck on one reading",
    },
    // ... and ground is the GND input. This reads the cell for it, which
    // the simulator's ADC answers as the cell, so every ground reading is
    // well off zero and a good torch faults.
    Breakage {
        name: "ground-reads-the-cell",
        file: HW,
        find: "        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_GND),\n",
        replace: "        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_VDDDIV10),\n",
        stage: Stage::Simulator,
        expect: "but the library says",
    },
    // The cell's and VDDIO2's readings are kept where `peek` reads them.
    // This writes zeros there, so `peek` would say nothing's been read.
    Breakage {
        name: "supplies-not-kept",
        file: MAIN,
        find: "        hw::record_supplies(torch.supplies().to_bytes());\n",
        replace: "        hw::record_supplies([0; 4]);\n",
        stage: Stage::Simulator,
        expect: "keeping the supply readings",
    },
    // ... and within an update, a NACK ends it. This tries each transfer
    // until it goes through.
    Breakage {
        name: "aux-retried-after-a-nack",
        file: HW,
        find: "        if !twi_transfer(transfer, &mut polls) {\n",
        replace: "        while !twi_transfer(transfer, &mut polls) {}\n        if false {\n",
        stage: Stage::Simulator,
        expect: "addresses the bus again after a NACK",
    },
];

/// Runs the breakages named, or all of them with none named.
pub fn run(root: &Path, names: &[&str]) -> Result<(), String> {
    let chosen = choose(names)?;
    let copy = Scratch::of(root)?;
    let started = Instant::now();

    eprintln!("building an unbroken copy in {}", copy.dir.display());
    let (passed, output) = copy.hex()?;
    if !passed {
        return Err(format!(
            "the copy doesn't pass `cargo xtask hex` with nothing broken, so no refusal would \
             mean anything:\n{output}"
        ));
    }
    eprintln!("it passes, in {} s", started.elapsed().as_secs());

    let mut failed = Vec::new();
    for breakage in &chosen {
        let began = Instant::now();
        match copy.refuses(root, breakage) {
            Ok(verdict) => eprintln!(
                "refused {} in {:.1} s: {verdict}",
                breakage.name,
                began.elapsed().as_secs_f32()
            ),
            Err(e) => {
                eprintln!("NOT REFUSED {}: {e}", breakage.name);
                failed.push(breakage.name);
            }
        }
    }
    if !failed.is_empty() {
        return Err(format!(
            "{} of {} breakages got past `cargo xtask hex`, or were refused for some other \
             reason: {}",
            failed.len(),
            chosen.len(),
            failed.join(", ")
        ));
    }
    println!(
        "{} breakages, each refused for its own reason, in {} s",
        chosen.len(),
        started.elapsed().as_secs()
    );
    Ok(())
}

/// The breakages called `names`, in the table's order, or all of them.
fn choose(names: &[&str]) -> Result<Vec<&'static Breakage>, String> {
    if let Some(unknown) = names
        .iter()
        .find(|name| BREAKAGES.iter().all(|b| b.name != **name))
    {
        let known: Vec<&str> = BREAKAGES.iter().map(|b| b.name).collect();
        return Err(format!(
            "no breakage called `{unknown}`; there's {}",
            known.join(", ")
        ));
    }
    Ok(BREAKAGES
        .iter()
        .filter(|b| names.is_empty() || names.contains(&b.name))
        .collect())
}

/// `source` with `find` swapped for `replace`. `find` has to be there exactly
/// once, so a refactor that moves or rewords the line fails here, loudly, and
/// can't turn a breakage into one that changes nothing.
fn substitute(source: &str, find: &str, replace: &str) -> Result<String, String> {
    if find.is_empty() || find == replace {
        return Err("the substitution changes nothing".into());
    }
    match source.matches(find).count() {
        1 => Ok(source.replacen(find, replace, 1)),
        n => Err(format!(
            "the text to break is there {n} times, not once:\n{find}"
        )),
    }
}

/// Where `hex` stopped, and the line it stopped with, from what it printed.
/// xtask's last word is the last line starting `error: `. If that names an
/// ELF, the build worked and the image was refused, by the simulator if it
/// says so and by `layout.rs` if not. If it doesn't, it's cargo having
/// failed, and what the compiler or the linker said is further up.
fn verdict(output: &str) -> Option<(Stage, &str)> {
    let line = output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("error: "))?;
    let stage = if line.contains(".elf: simulator: ") {
        Stage::Simulator
    } else if line.contains(".elf: ") {
        Stage::Layout
    } else {
        Stage::Build
    };
    Some((stage, line))
}

/// Whether `output` is `hex` refusing the firmware at `stage` with `expect`
/// in what it said. Gives back the line that has `expect` in it, less the
/// path of the ELF.
fn refused<'a>(output: &'a str, stage: Stage, expect: &str) -> Result<&'a str, String> {
    let Some((stopped, _)) = verdict(output) else {
        return Err(format!("it failed without saying why:\n{output}"));
    };
    if stopped != stage {
        return Err(format!(
            "it stopped at {stopped:?}, not {stage:?}:\n{output}"
        ));
    }
    let Some(line) = output.lines().find(|line| line.contains(expect)) else {
        return Err(format!(
            "it was refused, but not with `{expect}`:\n{output}"
        ));
    };
    let said = line.split_once(".elf: ").map_or(line, |(_, said)| said);
    Ok(said.trim())
}

/// The copy of the source, which goes when this does.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// Copies `COPIED` from `root` to a fresh directory under the system's
    /// temporary one, named for this process.
    fn of(root: &Path) -> Result<Self, String> {
        let dir = env::temp_dir().join(format!("kr1k-breakages-{}", process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(at(&dir))?;
        }
        // Made before anything's copied, so a failed copy is cleared up too.
        let copy = Self { dir };
        for entry in COPIED {
            copy_tree(&root.join(entry), &copy.dir.join(entry))?;
        }
        Ok(copy)
    }

    /// Runs `cargo xtask hex` in the copy. Gives back whether it passed, and
    /// everything it printed.
    fn hex(&self) -> Result<(bool, String), String> {
        let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let output = Command::new(&cargo)
            .current_dir(&self.dir)
            .args(["xtask", "hex"])
            // The copy's xtask looks in the copy's own `target`.
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .map_err(|e| format!("couldn't run {cargo}: {e}"))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), text))
    }

    /// Breaks the copy as `breakage` says, having put back whatever the last
    /// one broke, and checks `hex` refuses it for the reason it gives.
    fn refuses(&self, root: &Path, breakage: &Breakage) -> Result<String, String> {
        // Written afresh rather than copied, and only if it differs, so it's
        // newer than the last build. A copy can keep the file's old
        // modification time (it does on macOS), which leaves cargo sure the
        // last breakage's build of a library file is still fresh.
        for other in BREAKAGES {
            let file = Path::new(other.file);
            let original = fs::read(root.join(file)).map_err(at(file))?;
            let copy = self.dir.join(file);
            if fs::read(&copy).map_err(at(&copy))? != original {
                fs::write(&copy, original).map_err(at(&copy))?;
            }
        }
        let path = self.dir.join(breakage.file);
        let source = fs::read_to_string(&path).map_err(at(&path))?;
        let broken = substitute(&source, breakage.find, breakage.replace)
            .map_err(|e| format!("{}: {e}", breakage.file))?;
        fs::write(&path, broken).map_err(at(&path))?;

        let (passed, output) = self.hex()?;
        if passed {
            return Err("`cargo xtask hex` passed it".into());
        }
        refused(&output, breakage.stage, breakage.expect).map(str::to_string)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Nothing to be done if it won't go, and the run's verdict stands.
        _ = fs::remove_dir_all(&self.dir);
    }
}

/// Copies a file, or a directory and all that's in it but `SKIPPED`.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(at(parent))?;
        }
        return fs::copy(from, to).map(drop).map_err(at(from));
    }
    fs::create_dir_all(to).map_err(at(to))?;
    for entry in fs::read_dir(from).map_err(at(from))? {
        let name = entry.map_err(at(from))?.file_name();
        if SKIPPED.iter().any(|skipped| name == *skipped) {
            continue;
        }
        copy_tree(&from.join(&name), &to.join(&name))?;
    }
    Ok(())
}

/// Prefixes an error with the file it came from.
fn at<E: std::fmt::Display>(path: &Path) -> impl Fn(E) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checkout these tests were built from.
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn a_substitution_wants_its_text_there_exactly_once() {
        assert_eq!(substitute("a b c", "b", "x").unwrap(), "a x c");
        assert_eq!(substitute("a b c", " b", "").unwrap(), "a c");
        let err = substitute("a b c", "d", "x").unwrap_err();
        assert!(err.contains("there 0 times"), "{err}");
        let err = substitute("a b b", "b", "x").unwrap_err();
        assert!(err.contains("there 2 times"), "{err}");
    }

    #[test]
    fn a_substitution_has_to_change_something() {
        assert!(substitute("a b c", "b", "b").is_err());
        assert!(substitute("a b c", "", "x").is_err());
    }

    /// The slow part isn't run here, so this is what catches a table gone
    /// stale: a file that's moved, or a line a refactor reworded.
    #[test]
    fn every_breakage_finds_its_text_exactly_once_in_the_source_as_it_is() {
        for breakage in BREAKAGES {
            let path = root().join(breakage.file);
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {}: {e}", breakage.name, breakage.file));
            let broken = substitute(&source, breakage.find, breakage.replace)
                .unwrap_or_else(|e| panic!("{}: {}: {e}", breakage.name, breakage.file));
            assert_ne!(broken, source, "{}", breakage.name);
            assert!(!breakage.expect.is_empty(), "{}", breakage.name);
        }
    }

    #[test]
    fn every_breakage_has_its_own_name_and_breaks_a_file_thats_copied() {
        for (i, breakage) in BREAKAGES.iter().enumerate() {
            let twice = BREAKAGES[..i].iter().any(|b| b.name == breakage.name);
            assert!(!twice, "two called {}", breakage.name);
            let copied = COPIED
                .iter()
                .any(|entry| Path::new(breakage.file).starts_with(entry));
            assert!(copied, "{} isn't copied", breakage.file);
        }
    }

    #[test]
    fn every_copied_entry_is_there_to_copy() {
        for entry in COPIED {
            assert!(root().join(entry).exists(), "{entry}");
        }
    }

    #[test]
    fn breakages_are_chosen_by_name_and_an_unknown_name_is_refused() {
        assert_eq!(choose(&[]).unwrap().len(), BREAKAGES.len());
        let chosen = choose(&["wide-multiply", "pc1-pull-up"]).unwrap();
        let names: Vec<&str> = chosen.iter().map(|b| b.name).collect();
        assert_eq!(names, ["pc1-pull-up", "wide-multiply"]);
        let err = choose(&["pc1-pull-up", "pc2-pull-up"]).err().unwrap();
        assert!(err.contains("no breakage called `pc2-pull-up`"), "{err}");
    }

    const SIMULATOR: &str = "\
   Compiling kr1k v0.0.0 (/tmp/copy)
    Finished `release` profile [optimized + debuginfo] target(s) in 4.05s
error: /tmp/copy/target/avr-none/release/kr1k.elf: simulator: startup: 0x0a1c: gives PC1 a pull-up
";
    const LAYOUT: &str = "\
    Finished `release` profile [optimized + debuginfo] target(s) in 4.05s
error: /tmp/copy/target/avr-none/release/kr1k.elf: the CCP unlock at 0x0010 isn't followed by `st Z`
";
    const LINKER: &str = "\
error: linking with `rust-lld` failed: exit status: 1
  = note: rust-lld: error: undefined symbol: __mulsi3
error: could not compile `kr1k` (bin \"kr1k\") due to 1 previous error
error: cargo failed (exit status: 101)
";
    const COMPILER: &str = "\
error[E0425]: cannot find value `x` in this scope
error: could not compile `kr1k` (bin \"kr1k\") due to 1 previous error
error: cargo failed (exit status: 101)
";

    #[test]
    fn the_verdict_is_xtasks_last_error_and_says_how_far_hex_got() {
        let stage = |output| verdict(output).map(|(stage, _)| stage);
        assert_eq!(stage(SIMULATOR), Some(Stage::Simulator));
        assert_eq!(stage(LAYOUT), Some(Stage::Layout));
        assert_eq!(stage(LINKER), Some(Stage::Build));
        assert_eq!(stage(COMPILER), Some(Stage::Build));
        assert_eq!(stage("wrote target/noctigon-kr1k.hex\n"), None);
        let (_, line) = verdict(LINKER).unwrap();
        assert_eq!(line, "cargo failed (exit status: 101)");
    }

    #[test]
    fn a_refusal_counts_only_at_the_stage_and_with_the_text_expected() {
        let line = refused(SIMULATOR, Stage::Simulator, "gives PC1 a pull-up").unwrap();
        assert_eq!(line, "simulator: startup: 0x0a1c: gives PC1 a pull-up");
        let line = refused(LAYOUT, Stage::Layout, "isn't followed by `st Z`").unwrap();
        assert_eq!(line, "the CCP unlock at 0x0010 isn't followed by `st Z`");
        let line = refused(LINKER, Stage::Build, "undefined symbol: __mulsi3").unwrap();
        assert_eq!(line, "= note: rust-lld: error: undefined symbol: __mulsi3");

        // Refused, but for something else.
        let err = refused(SIMULATOR, Stage::Simulator, "without wdr").unwrap_err();
        assert!(err.contains("not with `without wdr`"), "{err}");
        // A breakage that doesn't compile hasn't been refused by anything.
        let err = refused(COMPILER, Stage::Build, "undefined symbol: __mulsi3").unwrap_err();
        assert!(err.contains("not with"), "{err}");
        let err = refused(COMPILER, Stage::Simulator, "x").unwrap_err();
        assert!(err.contains("stopped at Build, not Simulator"), "{err}");
        let err = refused(LAYOUT, Stage::Simulator, "st Z").unwrap_err();
        assert!(err.contains("stopped at Layout, not Simulator"), "{err}");
        let err = refused("", Stage::Build, "x").unwrap_err();
        assert!(err.contains("without saying why"), "{err}");
    }

    #[test]
    fn a_copy_leaves_out_target_and_git_and_goes_when_its_dropped() {
        let from = env::temp_dir().join(format!("kr1k-breakages-test-{}", process::id()));
        let to = from.with_extension("copy");
        for dir in ["src/avr", "target", ".git", "src/target"] {
            fs::create_dir_all(from.join(dir)).unwrap();
            fs::write(from.join(dir).join("hw.rs"), "contents").unwrap();
        }
        let copy = Scratch { dir: to.clone() };
        copy_tree(&from, &to).unwrap();
        copy_tree(&from.join("src/avr/hw.rs"), &to.join("docs/hw.rs")).unwrap();
        let copied = fs::read_to_string(to.join("src/avr/hw.rs")).unwrap();
        assert_eq!(copied, "contents");
        assert!(to.join("docs/hw.rs").exists());
        for gone in ["target", ".git", "src/target"] {
            assert!(!to.join(gone).exists(), "{gone}");
        }
        drop(copy);
        assert!(!to.exists());
        fs::remove_dir_all(&from).unwrap();
    }
}
