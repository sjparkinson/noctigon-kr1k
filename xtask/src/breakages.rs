//! `cargo xtask breakages` breaks the firmware on purpose, one mistake at a
//! time, and checks `cargo xtask hex` refuses each for the reason expected.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::Instant;

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

struct Breakage {
    name: &'static str,
    file: &'static str,
    find: &'static str,
    replace: &'static str,
    stage: Stage,
    /// A substring of what `hex` prints.
    expect: &'static str,
}

const HW: &str = "src/avr/hw.rs";
const MAIN: &str = "src/main.rs";
const AW2016: &str = "src/aw2016.rs";
const LIMITS: &str = "src/limits.rs";
const BATTERY: &str = "src/battery.rs";
const SENSORS: &str = "src/sensors.rs";
const TORCH: &str = "src/torch.rs";

const BREAKAGES: &[Breakage] = &[
    // DAC0 above 0 with PC1 low is refused first, as close as a breakage gets.
    Breakage {
        name: "dac-live-before-pc1",
        file: HW,
        find: "    just_before();\n    out!(reg::VPORTC_OUT, 1 << ENABLE);\n",
        replace: "    just_before();\n    write16(reg::DAC0_DATA, 1 << DAC_SHIFT);\n    \
                  out!(reg::VPORTC_OUT, 1 << ENABLE);\n",
        stage: Stage::Simulator,
        expect: "sets DAC0 to 1 with PC1 low",
    },
    Breakage {
        name: "pc1-down-with-dac-live",
        file: HW,
        find: "pub fn output_off() {\n    write16(reg::DAC0_DATA, 0);\n",
        replace: "pub fn output_off() {\n",
        stage: Stage::Simulator,
        expect: "drops PC1 with DAC0 still at",
    },
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
    Breakage {
        name: "path-change-with-dac-live",
        file: HW,
        find: "        write16(reg::DAC0_DATA, 0);\n        spin(DAC_FALLS);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "with PC1 high and DAC0 at",
    },
    // How the paths combine is unknown, so they wait for DAC0 coming down too.
    Breakage {
        name: "paths-off-before-dac",
        file: HW,
        find: "pub fn output_off() {\n    write16(reg::DAC0_DATA, 0);\n",
        replace: "pub fn output_off() {\n    out!(reg::VPORTA_OUT, 0);\n    \
                  write16(reg::DAC0_DATA, 0);\n",
        stage: Stage::Simulator,
        expect: "off with PC1 high and DAC0 at",
    },
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
    Breakage {
        name: "pd6-input-left-on",
        file: HW,
        find: "    pin_ctrl(reg::PORTD_PIN0CTRL, DAC, regs::PORT_ISC_INPUT_DISABLE);\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "PD6's input buffer still on",
    },
    Breakage {
        name: "twi-line-driven",
        file: HW,
        find: "const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW;\n\
               const _: () = assert!(PORTA_OUTPUTS == 0xF2);\n",
        replace: "const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW | 1 << 2;\n",
        stage: Stage::Simulator,
        expect: "PA2 and PA3 are the AW2016's TWI lines",
    },
    // PA1 goes to something unknown. This torch's stock build holds it at 0.
    Breakage {
        name: "pa1-pull-up",
        file: HW,
        find: "    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, 0);\n",
        replace: "    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, regs::PORT_PULLUPEN);\n",
        stage: Stage::Simulator,
        expect: "gives PA1 a pull-up",
    },
    Breakage {
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
    Breakage {
        name: "sts-to-a-vport",
        file: HW,
        find: "                \"out {addr}, {value}\",\n",
        replace: "                \"sts {addr}, {value}\",\n",
        stage: Stage::Simulator,
        expect: "straight after a store to 64 or over",
    },
    // A store at 64 or over stands in for what could precede it with no `nop`.
    Breakage {
        name: "sleep-mode-without-nop",
        file: HW,
        find: "            \"nop\",\n            \"sts {addr}, {value}\",\n",
        replace: "            \"sts {addr}, {value}\",\n            \"sts {addr}, {value}\",\n",
        stage: Stage::Simulator,
        expect: "SLPCTRL.CTRLA straight after a store to 64 or over",
    },
    // The firmware never asks for it, so examples/guards.rs does.
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
    Breakage {
        name: "tcb0-left-running",
        file: HW,
        find: "        write(reg::TCB0_CTRLA, 0);\n        write(reg::TCB0_INTCTRL, 0);\n",
        replace: "        write(reg::TCB0_INTCTRL, 0);\n",
        stage: Stage::Simulator,
        expect: "powers down with TCB0 running",
    },
    Breakage {
        name: "no-tick-deadline",
        file: HW,
        find: "                if periods == 0 {\n                    break None;\n",
        replace: "                if periods == 0 {\n                    \
                  periods = TICK_DEADLINE_PERIODS;\n",
        stage: Stage::Simulator,
        expect: "past the deadline",
    },
    // The datasheet doesn't say `cli` or turning every interrupt off is enough alone.
    Breakage {
        name: "halt-without-cli",
        file: HW,
        find: "    unsafe { asm!(\"cli\", options(nostack)) };\n    set_output(Output::OFF);\n",
        replace: "    set_output(Output::OFF);\n",
        stage: Stage::Simulator,
        expect: "stops for good with SREG.I still set",
    },
    // TCB0's interrupt still goes off, so the chip would take the `sleep`.
    Breakage {
        name: "halt-leaves-the-light-on",
        file: HW,
        find: "    unsafe { asm!(\"cli\", options(nostack)) };\n    set_output(Output::OFF);\n",
        replace: "    unsafe { asm!(\"cli\", options(nostack)) };\n    \
                  write(reg::TCB0_INTCTRL, 0);\n",
        stage: Stage::Simulator,
        expect: "sleeps with TCB0 running and PC1 high",
    },
    Breakage {
        name: "dir-before-out",
        file: HW,
        find: "    out!(reg::VPORTC_OUT, 0);\n    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);\n}\n",
        replace: "    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);\n    out!(reg::VPORTC_OUT, 0);\n}\n",
        stage: Stage::Simulator,
        expect: "starting with PC1 high: the first register it writes leaves PC1 set",
    },
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
    // The first pass still finds it missing, so only that pass's delay is refused.
    Breakage {
        name: "vddio2-ignored",
        file: MAIN,
        find: "        supply: !hw::vddio2_ok(),\n",
        replace: "        supply: !hw::vddio2_ok() && false,\n",
        stage: Stage::Simulator,
        expect: "with VDDIO2 missing: tick",
    },
    Breakage {
        name: "vddio2-not-watched",
        file: MAIN,
        find: "            hw::vddio2_ok(),\n",
        replace: "            true,\n",
        stage: Stage::Simulator,
        expect: "began with MVIO.STATUS.VDDIO2S clear",
    },
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
    Breakage {
        name: "lit-watchdog-never-restored",
        file: MAIN,
        find: "            hw::set_watchdog(setting);\n",
        replace: "            if setting == kr1k::watchdog::Setting::Rest {\n                \
                  hw::set_watchdog(setting);\n            }\n",
        stage: Stage::Simulator,
        expect: "raises PC1 with WDT.CTRLA at 0x0b",
    },
    // The guard alone changes nothing, so this also lights while the switch is down.
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
    Breakage {
        name: "watchdog-write-not-waited-for",
        file: MAIN,
        find: "        if end.synced {\n            hw::watchdog_synced();\n        }\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "powers down with WDT.STATUS.SYNCBUSY still set",
    },
    // The pass that changes the PIT's period can sleep in power-down.
    Breakage {
        name: "fed-as-the-pit-changes",
        file: MAIN,
        find: "        if end.feed {\n",
        replace: "        if end.feed || end.pit.is_some() {\n",
        stage: Stage::Simulator,
        expect: "the library feeds the watchdog 0 times on this tick, but the firmware 1",
    },
    Breakage {
        name: "pit-left-slow",
        file: MAIN,
        find: "        if let Some(pit) = end.pit {\n",
        replace: "        if let Some(pit @ kr1k::watchdog::Pit::Slow) = end.pit {\n",
        stage: Stage::Simulator,
        expect: "the library has the PIT Fast, but the firmware Slow",
    },
    Breakage {
        name: "switch-wake-dropped",
        file: HW,
        find: "            if switch_wakes {\n",
        replace: "            if switch_wakes && false {\n",
        stage: Stage::Simulator,
        expect: "the firmware ran 0 passes on this tick, but the library 1",
    },
    Breakage {
        name: "stale-switch-edge",
        file: HW,
        find: "    out!(reg::GPR_GPR2, 0);\n    let pressed = switch_pressed();\n",
        replace: "    let pressed = switch_pressed();\n",
        stage: Stage::Simulator,
        expect: "resting, with an edge on every slow tick: tick",
    },
    Breakage {
        name: "high-byte-first",
        file: HW,
        find: "    write(addr, lo);\n    write(addr + 1, hi);\n",
        replace: "    write(addr + 1, hi);\n    write(addr, lo);\n",
        stage: Stage::Simulator,
        expect: "writes the high byte of DAC0.DATA without its low byte",
    },
    Breakage {
        name: "unlock-apart-from-its-write",
        file: HW,
        find: "            \"out {ccp}, {key}\",\n",
        replace: "            \"out {ccp}, {key}\",\n            \"nop\",\n",
        stage: Stage::Layout,
        expect: "isn't followed by `st Z`",
    },
    Breakage {
        name: "handler-races-temp",
        file: HW,
        find: "pub extern \"avr-interrupt\" fn portd_port() {\n",
        replace: "pub extern \"avr-interrupt\" fn portd_port() {\n    write16(reg::DAC0_DATA, 0);\n",
        stage: Stage::Simulator,
        expect: "share its TEMP byte",
    },
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
    // The check goes by symbol names, so see this one fire on every new nightly.
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
    // Dividing a `u16` by ten turns into a 32-bit multiply.
    Breakage {
        name: "wide-multiply",
        file: HW,
        find: "    Some(u16::from_le_bytes([lo, hi]))\n",
        replace: "    Some(u16::from_le_bytes([lo, hi]) / 10)\n",
        stage: Stage::Build,
        expect: "undefined symbol: __mulsi3",
    },
    Breakage {
        name: "twi-unbounded-wait",
        file: HW,
        find: "        match polls.checked_sub(1) {\n            Some(left) => *polls = left,\n            \
               None => return Sent::TimedOut,\n        }\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "with SCL held low: tick",
    },
    Breakage {
        name: "aux-lit-while-lit",
        file: MAIN,
        find: "aux.step(torch.aux())",
        replace: "aux.step(if torch.output().enable { kr1k::ui::Aux::Blue } else { torch.aux() })",
        stage: Stage::Simulator,
        expect: "raises PC1 with the aux lit",
    },
    Breakage {
        name: "aux-off-not-standby",
        file: AW2016,
        find: "        data: &[CHGDIS],\n",
        replace: "        data: &[CHGDIS | CHIPEN],\n",
        stage: Stage::Simulator,
        expect: "isn't in standby",
    },
    Breakage {
        name: "aux-retried-on-the-next-tick",
        file: MAIN,
        find: "            aux.failed();\n",
        replace: "",
        stage: Stage::Simulator,
        expect: "tries the AW2016 again",
    },
    // Four readings in a row can't put the light out within 12 ticks.
    Breakage {
        name: "collapse-waits",
        file: BATTERY,
        find: "pub const COLLAPSE_READINGS: u8 = 2;\n",
        replace: "pub const COLLAPSE_READINGS: u8 = 4;\n",
        stage: Stage::Simulator,
        expect: "with a collapsing cell: the cell falls on tick",
    },
    Breakage {
        name: "no-quick-readings",
        file: SENSORS,
        find: "        if channel == Channel::Voltage && step >= READING_STEP && lit {\n",
        replace: "        if channel == Channel::Voltage && step >= READING_STEP && lit && false {\n",
        stage: Stage::Simulator,
        expect: "A cell that collapses: expected Off",
    },
    Breakage {
        name: "ground-ignored",
        file: SENSORS,
        find: "        if raw <= GROUND_MAX {\n",
        replace: "        if raw <= GROUND_MAX || true {\n",
        stage: Stage::Simulator,
        expect: "with the ADC stuck on one reading",
    },
    Breakage {
        name: "ground-reads-the-cell",
        file: HW,
        find: "        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_GND),\n",
        replace: "        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_VDDDIV10),\n",
        stage: Stage::Simulator,
        expect: "but the library says",
    },
    Breakage {
        name: "aux-retried-after-a-nack",
        file: HW,
        find: "        if !twi_transfer(transfer, &mut polls) {\n",
        replace: "        while !twi_transfer(transfer, &mut polls) {}\n        if false {\n",
        stage: Stage::Simulator,
        expect: "addresses the bus again after a NACK",
    },
];

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

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
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

    fn refuses(&self, root: &Path, breakage: &Breakage) -> Result<String, String> {
        // Written afresh, not copied, and only if it differs. A copy can keep
        // the old modification time (on macOS it does), so cargo won't rebuild.
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
        _ = fs::remove_dir_all(&self.dir);
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

fn at<E: std::fmt::Display>(path: &Path) -> impl Fn(E) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let err = refused(SIMULATOR, Stage::Simulator, "without wdr").unwrap_err();
        assert!(err.contains("not with `without wdr`"), "{err}");
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
