//! A refusal shows one alarm rings for one mistake, not for every mistake of
//! that kind.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::{TempDir, at, cargo, checked, firmware_build, guards_checked, root};

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

const SKIPPED: [&str; 2] = ["target", ".git"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Build,
    Layout,
    Simulator,
}

struct Mistake {
    name: &'static str,
    file: &'static str,
    find: &'static str,
    replace: &'static str,
    stage: Stage,
    /// A substring of the refusal.
    expect: &'static str,
}

const HW: &str = "src/avr/hw.rs";
const MAIN: &str = "src/main.rs";
const AW2016: &str = "src/aw2016.rs";
const LIMITS: &str = "src/limits.rs";
const BATTERY: &str = "src/battery.rs";
const SENSORS: &str = "src/sensors.rs";
const TORCH: &str = "src/torch.rs";

const MISTAKES: &[Mistake] = &[
    // DAC0 above 0 with PC1 low is refused first, as close as a mistake gets.
    Mistake {
        name: "dac-live-before-pc1",
        file: HW,
        find: "    just_before();\n    out!(reg::VPORTC_OUT, 1 << ENABLE);\n",
        replace: "    just_before();\n    write16(reg::DAC0_DATA, 1 << DAC_SHIFT);\n    \
                  out!(reg::VPORTC_OUT, 1 << ENABLE);\n",
        stage: Stage::Simulator,
        expect: "sets DAC0 to 1 with PC1 low",
    },
    Mistake {
        name: "pc1-down-with-dac-live",
        file: HW,
        find: "pub fn output_off() {\n    write16(reg::DAC0_DATA, 0);\n",
        replace: "pub fn output_off() {\n",
        stage: Stage::Simulator,
        expect: "drops PC1 with DAC0 still at",
    },
    Mistake {
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
    Mistake {
        name: "path-change-with-dac-live",
        file: HW,
        find: "        write16(reg::DAC0_DATA, 0);\n        spin(DAC_FALLS);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "with PC1 high and DAC0 at",
    },
    // How the paths combine is unknown, so they wait for DAC0 coming down too.
    Mistake {
        name: "paths-off-before-dac",
        file: HW,
        find: "pub fn output_off() {\n    write16(reg::DAC0_DATA, 0);\n",
        replace: "pub fn output_off() {\n    out!(reg::VPORTA_OUT, 0);\n    \
                  write16(reg::DAC0_DATA, 0);\n",
        stage: Stage::Simulator,
        expect: "off with PC1 high and DAC0 at",
    },
    Mistake {
        name: "dac-fall-not-waited",
        file: HW,
        find: "        spin(DAC_FALLS);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "table 38-23",
    },
    Mistake {
        name: "pc1-pull-up",
        file: HW,
        find: "    pin_ctrl(reg::PORTC_PIN0CTRL, ENABLE, 0);\n",
        replace: "    pin_ctrl(reg::PORTC_PIN0CTRL, ENABLE, regs::PORT_PULLUPEN);\n",
        stage: Stage::Simulator,
        expect: "gives PC1 a pull-up",
    },
    Mistake {
        name: "switch-pull-up",
        file: HW,
        find: "    pin_ctrl(reg::PORTD_PIN0CTRL, SWITCH, regs::PORT_ISC_BOTHEDGES);\n",
        replace: "    pin_ctrl(\n        reg::PORTD_PIN0CTRL,\n        SWITCH,\n        \
                  regs::PORT_ISC_BOTHEDGES | regs::PORT_PULLUPEN,\n    );\n",
        stage: Stage::Simulator,
        expect: "gives PD4 a pull-up",
    },
    Mistake {
        name: "pd6-input-left-on",
        file: HW,
        find: "    pin_ctrl(reg::PORTD_PIN0CTRL, DAC, regs::PORT_ISC_INPUT_DISABLE);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "PD6's input buffer still on",
    },
    Mistake {
        name: "twi-line-driven",
        file: HW,
        find: "const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW;\n\
               const _: () = assert!(PORTA_OUTPUTS == 0xF2);\n",
        replace: "const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW | 1 << 2;\n",
        stage: Stage::Simulator,
        expect: "PA2 and PA3 are the AW2016's TWI lines",
    },
    // PA1 goes to something unknown. This torch's stock build holds it at 0.
    Mistake {
        name: "pa1-pull-up",
        file: HW,
        find: "    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, 0);\n",
        replace: "    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, regs::PORT_PULLUPEN);\n",
        stage: Stage::Simulator,
        expect: "gives PA1 a pull-up",
    },
    Mistake {
        name: "pa1-driven-high",
        file: HW,
        find: "    out!(reg::VPORTA_OUT, 0);\n    out!(reg::VPORTA_DIR, PORTA_OUTPUTS);\n",
        replace: "    out!(reg::VPORTA_OUT, 1 << HELD_LOW);\n    \
                  out!(reg::VPORTA_DIR, PORTA_OUTPUTS);\n",
        stage: Stage::Simulator,
        expect: "setting PA1",
    },
    // Errata 2.2.3. A plain `write` to a VPORT doesn't show it, since LLVM
    // emits `out` itself for any constant address under 64.
    Mistake {
        name: "sts-to-a-vport",
        file: HW,
        find: "                \"out {addr}, {value}\",\n",
        replace: "                \"sts {addr}, {value}\",\n",
        stage: Stage::Simulator,
        expect: "straight after a store to 64 or over",
    },
    // A store at 64 or over stands in for what could precede it with no `nop`.
    Mistake {
        name: "sleep-mode-without-nop",
        file: HW,
        find: "            \"nop\",\n            \"sts {addr}, {value}\",\n",
        replace: "            \"sts {addr}, {value}\",\n            \"sts {addr}, {value}\",\n",
        stage: Stage::Simulator,
        expect: "SLPCTRL.CTRLA straight after a store to 64 or over",
    },
    Mistake {
        name: "adc-on-in-power-down",
        file: HW,
        find: "        write(reg::ADC0_CTRLA, 0);\n        regs::SLPCTRL_SMODE_PDOWN\n",
        replace: "        regs::SLPCTRL_SMODE_PDOWN\n",
        stage: Stage::Simulator,
        expect: "powers down with ADC0 still enabled",
    },
    Mistake {
        name: "tcb0-left-running",
        file: HW,
        find: "        write(reg::TCB0_CTRLA, 0);\n        write(reg::TCB0_INTCTRL, 0);\n",
        replace: "        write(reg::TCB0_INTCTRL, 0);\n",
        stage: Stage::Simulator,
        expect: "powers down with TCB0 running",
    },
    Mistake {
        name: "no-tick-deadline",
        file: HW,
        find: "                if periods == 0 {\n                    break None;\n",
        replace: "                if periods == 0 {\n                    \
                  periods = TICK_DEADLINE_PERIODS;\n",
        stage: Stage::Simulator,
        expect: "past the deadline",
    },
    // The datasheet doesn't say `cli` or turning every interrupt off is enough alone.
    Mistake {
        name: "halt-without-cli",
        file: HW,
        find: "    unsafe { asm!(\"cli\", options(nostack)) };\n    set_output(Output::OFF);\n",
        replace: "    set_output(Output::OFF);\n",
        stage: Stage::Simulator,
        expect: "stops for good with SREG.I still set",
    },
    // TCB0's interrupt still goes off, so the chip would take the `sleep`.
    Mistake {
        name: "halt-leaves-the-light-on",
        file: HW,
        find: "    unsafe { asm!(\"cli\", options(nostack)) };\n    set_output(Output::OFF);\n",
        replace: "    unsafe { asm!(\"cli\", options(nostack)) };\n    \
                  write(reg::TCB0_INTCTRL, 0);\n",
        stage: Stage::Simulator,
        expect: "sleeps with TCB0 running and PC1 high",
    },
    Mistake {
        name: "dir-before-out",
        file: HW,
        find: "    out!(reg::VPORTC_OUT, 0);\n    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);\n}\n",
        replace: "    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);\n    out!(reg::VPORTC_OUT, 0);\n}\n",
        stage: Stage::Simulator,
        expect: "starting with PC1 high: the first register it writes leaves PC1 set",
    },
    Mistake {
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
    // The first pass still finds it missing, so only that pass's delay is refused.
    Mistake {
        name: "vddio2-ignored",
        file: MAIN,
        find: "        supply: !hw::vddio2_ok(),\n",
        replace: "        supply: !hw::vddio2_ok() && false,\n",
        stage: Stage::Simulator,
        expect: "with VDDIO2 missing: tick",
    },
    Mistake {
        name: "vddio2-not-watched",
        file: MAIN,
        find: "            hw::vddio2_ok(),\n",
        replace: "            true,\n",
        stage: Stage::Simulator,
        expect: "began with MVIO.STATUS.VDDIO2S clear",
    },
    Mistake {
        name: "vddio2-loss-fades-out",
        file: LIMITS,
        find: "        let fault = self.fault && (cut || self.level <= 1);\n",
        replace: "        let fault = self.fault && self.level <= 1;\n",
        stage: Stage::Simulator,
        expect: "began with MVIO.STATUS.VDDIO2S clear",
    },
    Mistake {
        name: "watchdog-never-fed",
        file: MAIN,
        find: "            hw::feed_watchdog();\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "without wdr",
    },
    Mistake {
        name: "watchdog-fed-every-tick",
        file: MAIN,
        find: "        if end.feed {\n",
        replace: "        if end.feed || true {\n",
        stage: Stage::Simulator,
        expect: "closed window",
    },
    Mistake {
        name: "lit-watchdog-never-restored",
        file: MAIN,
        find: "            hw::set_watchdog(setting);\n",
        replace: "            if setting == kr1k::watchdog::Setting::Rest {\n                \
                  hw::set_watchdog(setting);\n            }\n",
        stage: Stage::Simulator,
        expect: "raises PC1 with WDT.CTRLA at 0x0b",
    },
    // The guard alone changes nothing, so this also lights while the switch is down.
    Mistake {
        name: "unguarded-light-at-rest",
        file: TORCH,
        find: "            let asked = if lit_window {\n                \
               self.ui.output_level()\n            } else {\n                0\n            };\n",
        replace: "            let _ = lit_window;\n            \
                  let asked = self.ui.output_level().max(u8::from(pressed));\n",
        stage: Stage::Simulator,
        expect: "the light only comes on with the lit window in force and a wdr since it was written",
    },
    Mistake {
        name: "watchdog-write-not-waited-for",
        file: MAIN,
        find: "        if end.synced {\n            hw::watchdog_synced();\n        }\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "powers down with WDT.STATUS.SYNCBUSY still set",
    },
    // The pass that changes the PIT's period can sleep in power-down.
    Mistake {
        name: "fed-as-the-pit-changes",
        file: MAIN,
        find: "        if end.feed {\n",
        replace: "        if end.feed || end.pit.is_some() {\n",
        stage: Stage::Simulator,
        expect: "the library feeds the watchdog 0 times on this tick, but the firmware 1",
    },
    Mistake {
        name: "pit-left-slow",
        file: MAIN,
        find: "        if let Some(pit) = end.pit {\n",
        replace: "        if let Some(pit @ kr1k::watchdog::Pit::Slow) = end.pit {\n",
        stage: Stage::Simulator,
        expect: "the library has the PIT Fast, but the firmware Slow",
    },
    Mistake {
        name: "switch-wake-dropped",
        file: HW,
        find: "            if switch_wakes {\n",
        replace: "            if switch_wakes && false {\n",
        stage: Stage::Simulator,
        expect: "the firmware ran 0 passes on this tick, but the library 1",
    },
    Mistake {
        name: "stale-switch-edge",
        file: HW,
        find: "    out!(reg::GPR_GPR2, 0);\n    let pressed = switch_pressed();\n",
        replace: "    let pressed = switch_pressed();\n",
        stage: Stage::Simulator,
        expect: "resting, with an edge on every slow tick: tick",
    },
    Mistake {
        name: "high-byte-first",
        file: HW,
        find: "    write(addr, lo);\n    write(addr + 1, hi);\n",
        replace: "    write(addr + 1, hi);\n    write(addr, lo);\n",
        stage: Stage::Simulator,
        expect: "writes the high byte of DAC0.DATA without its low byte",
    },
    Mistake {
        name: "unlock-apart-from-its-write",
        file: HW,
        find: "            \"out {ccp}, {key}\",\n",
        replace: "            \"out {ccp}, {key}\",\n            \"nop\",\n",
        stage: Stage::Layout,
        expect: "isn't followed by `st Z`",
    },
    Mistake {
        name: "sleep-apart-from-sei",
        file: HW,
        find: "asm!(\"sei\", \"sleep\", options(nostack))",
        replace: "asm!(\"sei\", \"nop\", \"sleep\", options(nostack))",
        stage: Stage::Layout,
        expect: "doesn't follow `sei`",
    },
    Mistake {
        name: "handler-races-temp",
        file: HW,
        find: "pub extern \"avr-interrupt\" fn portd_port() {\n",
        replace: "pub extern \"avr-interrupt\" fn portd_port() {\n    write16(reg::DAC0_DATA, 0);\n",
        stage: Stage::Simulator,
        expect: "share its TEMP byte",
    },
    Mistake {
        name: "handler-clobbers-a-register",
        file: HW,
        find: "    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);\n    out!(reg::GPR_GPR0, TICK);\n",
        replace: "    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);\n    \
                  unsafe { asm!(\"inc r2\", options(nostack)) };\n    \
                  out!(reg::GPR_GPR0, TICK);\n",
        stage: Stage::Simulator,
        expect: "the handler for RTC_PIT (vector 6): returns with r2",
    },
    // The check goes by symbol names, so see this one fire on every new nightly.
    Mistake {
        name: "bounds-check",
        file: MAIN,
        find: "        torch.tick(\n            hw::take_switch(),\n            hw::adc_result(),\n",
        replace: "        let reading = hw::adc_result();\n        \
                  let pressed = [hw::take_switch(); 4][usize::from(reading.unwrap_or(0))];\n        \
                  torch.tick(\n            pressed,\n            reading,\n",
        stage: Stage::Layout,
        expect: "panic_bounds_check",
    },
    // Dividing a `u16` by ten turns into a 32-bit multiply.
    Mistake {
        name: "wide-multiply",
        file: HW,
        find: "    Some(u16::from_le_bytes([lo, hi]))\n",
        replace: "    Some(u16::from_le_bytes([lo, hi]) / 10)\n",
        stage: Stage::Build,
        expect: "undefined symbol: __mulsi3",
    },
    Mistake {
        name: "twi-unbounded-wait",
        file: HW,
        find: "        match polls.checked_sub(1) {\n            Some(left) => *polls = left,\n            \
               None => return Sent::TimedOut,\n        }\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "with SCL held low: tick",
    },
    Mistake {
        name: "aux-lit-while-lit",
        file: MAIN,
        find: "aux.step(torch.aux())",
        replace: "aux.step(if torch.output().enable { kr1k::ui::Aux::Blue } else { torch.aux() })",
        stage: Stage::Simulator,
        expect: "raises PC1 with the aux lit",
    },
    Mistake {
        name: "aux-off-not-standby",
        file: AW2016,
        find: "        data: &[CHGDIS],\n",
        replace: "        data: &[CHGDIS | CHIPEN],\n",
        stage: Stage::Simulator,
        expect: "isn't in standby",
    },
    Mistake {
        name: "aux-retried-on-the-next-tick",
        file: MAIN,
        find: "            aux.failed();\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "tries the AW2016 again",
    },
    // Four readings in a row can't put the light out within 12 ticks.
    Mistake {
        name: "collapse-waits",
        file: BATTERY,
        find: "pub const COLLAPSE_READINGS: u8 = 2;\n",
        replace: "pub const COLLAPSE_READINGS: u8 = 4;\n",
        stage: Stage::Simulator,
        expect: "with a collapsing cell: the cell falls on tick",
    },
    Mistake {
        name: "no-quick-readings",
        file: SENSORS,
        find: "        if channel == Channel::Voltage && step >= READING_STEP && lit {\n",
        replace: "        if channel == Channel::Voltage && step >= READING_STEP && lit && false {\n",
        stage: Stage::Simulator,
        expect: "A cell that collapses: expected Off",
    },
    Mistake {
        name: "ground-ignored",
        file: SENSORS,
        find: "        if raw <= GROUND_MAX {\n",
        replace: "        if raw <= GROUND_MAX || true {\n",
        stage: Stage::Simulator,
        expect: "with the ADC stuck on one reading",
    },
    Mistake {
        name: "ground-reads-the-cell",
        file: HW,
        find: "        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_GND),\n",
        replace: "        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_VDDDIV10),\n",
        stage: Stage::Simulator,
        expect: "but the library says",
    },
    Mistake {
        name: "aux-retried-after-a-nack",
        file: HW,
        find: "        if !twi_transfer(transfer, &mut polls) {\n",
        replace: "        while !twi_transfer(transfer, &mut polls) {}\n        if false {\n",
        stage: Stage::Simulator,
        expect: "addresses the bus again after a NACK",
    },
];

/// The firmware never asks for these, so only examples/guards.rs reaches them,
/// and only its checks are run on them.
const GUARDS_MISTAKES: &[Mistake] = &[Mistake {
    name: "power-down-with-the-light-on",
    file: HW,
    find: "    let mode = if rest && !lit() {\n",
    replace: "    let mode = if rest {\n",
    stage: Stage::Simulator,
    expect: "powers down with PC1 high",
}];

type Check = fn(&Path) -> Result<(), String>;

fn hex_checked(copy: &Path) -> Result<(), String> {
    checked(copy).map(drop)
}

fn own_hex(copy: &Path) -> Result<(), String> {
    let hex = Command::new(cargo())
        .current_dir(copy)
        .args(["xtask", "hex"])
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .map_err(|e| e.to_string())?;
    if hex.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&hex.stderr).into_owned())
}

/// The simulator is built from the library, so a mistake there only shows in
/// a simulator built from the copy.
fn in_library(file: &str) -> bool {
    file != MAIN && file != HW
}

fn mistakes() -> Vec<(&'static Mistake, Check)> {
    let whole = MISTAKES
        .iter()
        .map(|mistake| match in_library(mistake.file) {
            true => (mistake, own_hex as Check),
            false => (mistake, hex_checked as Check),
        });
    let guards = GUARDS_MISTAKES
        .iter()
        .map(|mistake| (mistake, guards_checked as Check));
    whole.chain(guards).collect()
}

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

fn stage(refusal: &str) -> Stage {
    if refusal.contains(".elf: simulator: ") {
        Stage::Simulator
    } else if refusal.contains(".elf: ") {
        Stage::Layout
    } else {
        Stage::Build
    }
}

/// Cargo's own refusal is kept, since a link error is one of the reasons.
fn refused(copy: &Path, check: Check) -> Result<(), (Stage, String)> {
    let built = firmware_build(copy)
        // The copy builds into its own `target`, where `check` looks.
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .map_err(|e| (Stage::Build, e.to_string()))?;
    if !built.status.success() {
        let stderr = String::from_utf8_lossy(&built.stderr).into_owned();
        return Err((Stage::Build, stderr));
    }
    check(copy).map_err(|e| (stage(&e), e))
}

#[test]
fn a_substitution_wants_its_text_there_exactly_once_and_has_to_change_it() {
    assert_eq!(substitute("a b c", "b", "x").unwrap(), "a x c");
    assert_eq!(substitute("a b c", " b", "").unwrap(), "a c");
    for (source, find, replace, why) in [
        ("a b c", "d", "x", "there 0 times"),
        ("a b b", "b", "x", "there 2 times"),
        ("a b c", "b", "b", "changes nothing"),
        ("a b c", "", "x", "changes nothing"),
    ] {
        let err = substitute(source, find, replace).unwrap_err();
        assert!(err.contains(why), "{err}");
    }
}

#[test]
fn every_mistake_has_its_own_name_and_finds_its_text_exactly_once() {
    let mistakes = mistakes();
    for (i, (mistake, _)) in mistakes.iter().enumerate() {
        let twice = mistakes[..i].iter().any(|(m, _)| m.name == mistake.name);
        assert!(!twice, "two called {}", mistake.name);
        assert!(!mistake.expect.is_empty(), "{}", mistake.name);
        let copied = COPIED
            .iter()
            .any(|entry| Path::new(mistake.file).starts_with(entry));
        assert!(copied, "{} isn't copied", mistake.file);
        let path = root().join(mistake.file);
        let source = fs::read_to_string(&path).unwrap();
        substitute(&source, mistake.find, mistake.replace)
            .unwrap_or_else(|e| panic!("{}: {}: {e}", mistake.name, mistake.file));
    }
}

#[test]
fn every_copied_entry_is_there_to_copy() {
    for entry in COPIED {
        assert!(root().join(entry).exists(), "{entry}");
    }
}

#[test]
fn a_refusal_by_the_checks_is_the_simulators_or_the_layouts() {
    let simulator = "/tmp/copy/target/avr-none/release/kr1k.elf: simulator: start-up order: \
                     0x0a1c: gives PC1 a pull-up";
    assert_eq!(stage(simulator), Stage::Simulator);
    let guards = "/tmp/copy/target/avr-none/release/examples/guards.elf: simulator: 0x01fa: \
                  powers down with PC1 high";
    assert_eq!(stage(guards), Stage::Simulator);
    let layout = "/tmp/copy/target/avr-none/release/kr1k.elf: the CCP unlock at 0x0010 isn't \
                  followed by `st Z`";
    assert_eq!(stage(layout), Stage::Layout);
}

#[test]
fn a_copy_leaves_out_target_and_git_and_goes_when_its_dropped() {
    let from = TempDir::new("mistakes-test").unwrap();
    let from = from.path();
    for dir in ["src/avr", "target", ".git", "src/target"] {
        fs::create_dir_all(from.join(dir)).unwrap();
        fs::write(from.join(dir).join("hw.rs"), "contents").unwrap();
    }
    let copy = TempDir::new("mistakes-test-copy").unwrap();
    let to = copy.path().to_path_buf();
    copy_tree(from, &to).unwrap();
    copy_tree(&from.join("src/avr/hw.rs"), &to.join("docs/hw.rs")).unwrap();
    let copied = fs::read_to_string(to.join("src/avr/hw.rs")).unwrap();
    assert_eq!(copied, "contents");
    assert!(to.join("docs/hw.rs").exists());
    for gone in ["target", ".git", "src/target"] {
        assert!(!to.join(gone).exists(), "{gone}");
    }
    drop(copy);
    assert!(!to.exists());
}

/// Where cargo keeps the `core` it built for the firmware. A copy that starts
/// with it doesn't build its own, and one that can't only takes longer.
const CORE: &str = "target/avr-none/release/build/core";

fn copy_of_the_tree(name: &str) -> TempDir {
    let dir = TempDir::new(name).unwrap();
    for entry in COPIED {
        copy_tree(&root().join(entry), &dir.path().join(entry)).unwrap();
    }
    _ = copy_tree(&root().join(CORE), &dir.path().join(CORE));
    dir
}

/// Gives back what wasn't refused as expected, each with its place in `mistakes`.
fn try_mistakes(
    copy: &Path,
    mistakes: &[(&Mistake, Check)],
    next: &AtomicUsize,
) -> Vec<(usize, String)> {
    let mut missed = Vec::new();
    loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        let Some(&(mistake, check)) = mistakes.get(i) else {
            return missed;
        };
        let path = copy.join(mistake.file);
        let source = fs::read_to_string(&path).unwrap();
        let broken = substitute(&source, mistake.find, mistake.replace).unwrap();
        fs::write(&path, broken).unwrap();
        let refusal = refused(copy, check);
        // Written rather than copied back, since a copy can keep the old
        // modified time, and cargo would then keep the broken build.
        fs::write(&path, source).unwrap();
        let (name, stage, expect) = (mistake.name, mistake.stage, mistake.expect);
        match refusal {
            Err((stopped, e)) if stopped == stage && e.contains(expect) => {}
            Err((stopped, e)) if stopped != stage => missed.push((
                i,
                format!("{name}: refused at {stopped:?}, not {stage:?}:\n{e}"),
            )),
            Err((_, e)) => {
                missed.push((i, format!("{name}: refused, but not with `{expect}`:\n{e}")))
            }
            Ok(()) => missed.push((i, format!("{name}: nothing refused it"))),
        }
    }
}

/// As many as CI's runner has cores.
const COPIES: usize = 4;

#[test]
#[ignore = "builds the firmware once a mistake, which takes about twenty seconds"]
fn the_firmware_is_refused_with_any_one_mistake_in_it() {
    // Leaves `CORE` built, and nothing writes it again while it's copied.
    let built = firmware_build(&root()).output().unwrap();
    let stderr = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "{stderr}");

    let (mistakes, next) = (mistakes(), AtomicUsize::new(0));
    let mut missed: Vec<(usize, String)> = thread::scope(|scope| {
        let copies: Vec<_> = (0..COPIES)
            .map(|n| {
                let (mistakes, next) = (&mistakes, &next);
                scope.spawn(move || {
                    let copy = copy_of_the_tree(&format!("mistakes-{n}"));
                    try_mistakes(copy.path(), mistakes, next)
                })
            })
            .collect();
        copies
            .into_iter()
            .flat_map(|copy| copy.join().unwrap())
            .collect()
    });
    missed.sort_by_key(|(i, _)| *i);
    let mut missed: Vec<String> = missed.into_iter().map(|(_, why)| why).collect();
    if !missed.is_empty()
        && let Err((_, e)) = refused(copy_of_the_tree("mistakes-none").path(), hex_checked)
    {
        missed.insert(
            0,
            format!(
                "a copy with nothing broken is refused too, so no refusal means anything:\n{e}"
            ),
        );
    }
    assert!(missed.is_empty(), "{}", missed.join("\n\n"));
}
