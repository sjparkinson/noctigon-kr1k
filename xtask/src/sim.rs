//! A fake AVR32DD20 around `avr.rs`'s CPU, wired up like the KR1K's Lume
//! X1-C driver, and the checks that run the real firmware on it.
//!
//! This finds mistakes and proves nothing. It is one reading of the datasheet
//! (DS40002413B, errata DS80001026D), run against code written from that same
//! reading, and has never been compared with a real chip. The board's rules
//! have no section number, since they come from what the stock firmware does.
//!
//! Only the registers the firmware touches are modelled, and touching any
//! other is an error. Chip resets aren't modelled, so a watchdog bite is a
//! failed run, and nothing analogue is either. Time counts instructions, each
//! taken as one clock, except the budgets, which take each as four.

use std::iter;
use std::ops::Range;
use std::thread;

use kr1k::TICKS_PER_SECOND;
use kr1k::battery::COLLAPSE_BELOW;
use kr1k::board;
use kr1k::ramp::{CEILING, Output, Vref};
use kr1k::sensors::{CYCLE_TICKS, Channel};
use kr1k::testing::{self, Action, EXAMPLES, Example, Paced, PitPhase, Rig, Sample, Scene};
use kr1k::ui::Aux;
use kr1k::watchdog::{
    CLOCK_HZ, CLOSED_CLOCKS, End, FEED_TICKS, OPEN_CLOCKS, Pit, SYNC_CLOCKS, TICK_DEADLINE_PERIODS,
    TIMER_PERIOD_CLOCKS,
};

use crate::avr::{Bus, Cpu, I, IO_LOW_END, VECTOR_WORDS, is_call, is_two_words, ldi, ldi_operands};
use crate::crc;
use crate::elf::{DATA_SPACE, Elf, SRAM};
use crate::layout::{self, jmp_target, word};
use crate::stamp;
use crate::switching::RandomSwitching;
use crate::twi::{Fault, Twi};
use crate::updi::map;
use crate::world::{CALIBRATION, FUSE_NAMES, World};

// Only what the ATDF names goes here, since anything hand-written in
// src/regs.rs is the firmware's choice.
use kr1k::regs::{
    self, ADC_ENABLE, ADC_INITDLY, ADC_MUXPOS_GND, ADC_MUXPOS_TEMPSENSE, ADC_MUXPOS_VDDDIV10,
    ADC_PRESC, ADC_RESRDY, ADC_RESSEL_12BIT, ADC_SAMPDLY, ADC_SAMPLEN, ADC_SAMPNUM,
    ADC_SAMPNUM_ACC4, ADC_STCONV, CRCSCAN_BUSY, CRCSCAN_ENABLE, CRCSCAN_OK, CRCSCAN_SRC_FLASH,
    DAC_ENABLE, DAC_OUTEN, MVIO_VDDIO2S, PORT_INVEN, PORT_ISC, PORT_PULLUPEN, RSTCTRL_PORF,
    RTC_CTRLBUSY, RTC_PI, RTC_PITEN, SLPCTRL_SEN, TCB_CAPT, TCB_ENABLE, VREF_REFSEL_1V024,
    VREF_REFSEL_2V048, VREF_REFSEL_2V500, WDT_LOCK, WDT_PERIOD, WDT_SYNCBUSY, WDT_WINDOW,
    addr as reg,
};

const CCP_IOREG: u8 = regs::CPU_CCP_IOREG;
/// The reset value (§12.5.7).
const OSCHF_4MHZ: u8 = regs::CLKCTRL_FRQSEL_4M;
const ISC_INTDISABLE: u8 = regs::PORT_ISC_INTDISABLE;
const ISC_BOTHEDGES: u8 = regs::PORT_ISC_BOTHEDGES;
const ISC_RISING: u8 = regs::PORT_ISC_RISING;
const ISC_FALLING: u8 = regs::PORT_ISC_FALLING;
const ISC_INPUT_DISABLE: u8 = regs::PORT_ISC_INPUT_DISABLE;
const ISC_LEVEL: u8 = regs::PORT_ISC_LEVEL;
const SLEEP_IDLE: u8 = regs::SLPCTRL_SMODE_IDLE;
const SLEEP_POWER_DOWN: u8 = regs::SLPCTRL_SMODE_PDOWN;
const TCB_RUNNING: u8 = regs::TCB_CLKSEL_DIV1 | TCB_ENABLE;
const PIT_FAST: u8 = regs::RTC_PERIOD_CYC512 | RTC_PITEN;
const PIT_SLOW: u8 = regs::RTC_PERIOD_CYC32768 | RTC_PITEN;
const ADC_ON: u8 = ADC_ENABLE | ADC_RESSEL_12BIT | regs::ADC_CONVMODE_SINGLEENDED;
const DAC_DRIVING: u8 = DAC_ENABLE | DAC_OUTEN;
/// How far DAC0's 10-bit code sits up DATA (§34.5.2).
const DAC_SHIFT: u32 = regs::DAC_DATA.trailing_zeros();

const VECTOR_RTC_PIT: u8 = regs::VECTOR_RTC_PIT as u8;
const VECTOR_TCB0: u8 = regs::VECTOR_TCB0_INT as u8;
const VECTOR_PORTA: u8 = regs::VECTOR_PORTA_PORT as u8;
const VECTOR_PORTC: u8 = regs::VECTOR_PORTC_PORT as u8;
const VECTOR_PORTD: u8 = regs::VECTOR_PORTD_PORT as u8;
const VECTORS: usize = regs::VECTORS;
const _: () = assert!(VECTORS <= 64);

/// Anything over is reserved.
const WDT_PERIOD_MAX: u8 = regs::WDT_PERIOD_8KCLK;

const CLK_PER: u32 = 4_000_000;
const _: () = assert!(CLOCK_HZ == CLK_PER);
/// Main clocks in TCB0's period.
const TIMER_PERIOD: u64 = TIMER_PERIOD_CLOCKS as u64;
/// WDT clocks in a 64 Hz tick (§22.3.2).
const WDT_CLOCKS_PER_TICK: u32 = 16;
/// The scan starts three cycles after ENABLE (§30.3.1).
const CRC_START_INSTRUCTIONS: u64 = 3;
/// Reads of STATUS that show BUSY. A real chip may never show it, since the
/// scan stalls the CPU (§30.3.2).
const CRC_BUSY_READS: u8 = 2;
/// What the first conversion after a change of input reads. The datasheet
/// doesn't promise it's wrong (table 38-25), so this is the worst case.
const UNSETTLED: u16 = 0;

/// Where flash appears in data space.
const MAPPED: u16 = map::FLASH;
const FLASH_SIZE: usize = map::FLASH_SIZE;
const _: () = assert!(MAPPED as usize + FLASH_SIZE == 0x1_0000);

/// SRAM's contents before startup.
const JUNK: u8 = 0xA5;

const ENABLE: u8 = 1 << board::ENABLE;
const PATHS: u8 = board::PATHS;
const TWI: u8 = 1 << board::TWI_SDA | 1 << board::TWI_SCL;
/// PA1, which goes to something unknown and is only ever held low.
const HELD_LOW: u8 = 1 << board::HELD_LOW;
const HELD_LOW_CTRL: usize = reg::PORTA_PIN0CTRL + board::HELD_LOW as usize;
/// PD4, active low.
const SWITCH: u8 = 1 << board::SWITCH;
const SWITCH_PIN: usize = board::SWITCH as usize;
const DAC_PIN: u8 = 1 << board::DAC;
const DAC_PIN_CTRL: usize = reg::PORTD_PIN0CTRL + board::DAC as usize;
const SWITCH_CTRL: usize = reg::PORTD_PIN0CTRL + SWITCH_PIN;

const BOOT_BUDGET: u64 = 100_000;
const TICK_CYCLES: u64 = CLK_PER as u64 / TICKS_PER_SECOND as u64;
const _: () = assert!(TICK_CYCLES * TICKS_PER_SECOND as u64 == CLK_PER as u64);
/// A load from mapped flash can take more than four clocks (instruction set
/// manual, table 5-4 note 2) and how many isn't stated, so this is likely.
pub const TICK_BUDGET: u64 = TICK_CYCLES / 4;
/// §15.3.2.3, table 15-1.
const INTERRUPT_CYCLES: u64 = 10;
pub const HANDLER_BUDGET: u64 = (TIMER_PERIOD - INTERRUPT_CYCLES) / 4;
/// 10 µs (table 38-23). A floor, since how fast PD6 falls on this board is
/// unknown.
const DAC_SETTLE_CYCLES: u64 = CLK_PER as u64 / 100_000;
/// 2 µs, table 38-18's tINTREF.
const VREF_SETTLE_CYCLES: u64 = CLK_PER as u64 / 500_000;
/// 8 ms, the stock firmware's wait for the boost converter.
const BOOST_START_CYCLES: u64 = CLK_PER as u64 / 125;

/// The real chip is busy for two 32 kHz clocks (§26.13.12).
const PIT_SYNC_READS: u8 = 2;

#[derive(Clone, Copy, Default)]
pub struct Report {
    /// Bytes.
    pub stack: u16,
    pub loop_instructions: u64,
    pub off_instructions: u64,
    pub handler_instructions: u64,
    pub landed: u32,
    last_new: u32,
}

impl Report {
    fn merge(&mut self, run: &Self) {
        self.stack = self.stack.max(run.stack);
        self.loop_instructions = self.loop_instructions.max(run.loop_instructions);
        self.off_instructions = self.off_instructions.max(run.off_instructions);
        self.handler_instructions = self.handler_instructions.max(run.handler_instructions);
        self.landed += run.landed;
    }

    fn take(&mut self, sim: &Sim) {
        self.stack = self.stack.max(sim.stack());
        self.handler_instructions = self.handler_instructions.max(sim.longest_handler);
        self.landed += sim.landed;
        self.last_new = self.last_new.max(sim.last_new);
    }
}

/// `main` is a word address.
struct Linked {
    main: u32,
    ends: Ends,
    layout: Layout,
}

/// Where interrupts that never return end up, as byte addresses.
#[derive(Clone, Copy, Default)]
struct Ends {
    unhandled: u32,
    halt: u32,
}

impl Linked {
    fn of(elf: &Elf) -> Result<Self, String> {
        let symbol = |name| elf.require(name);
        let sram = |name| -> Result<u16, String> { Ok((symbol(name)? - DATA_SPACE) as u16) };
        Ok(Self {
            main: symbol("main")? / 2,
            ends: Ends {
                unhandled: symbol("__unhandled_interrupt")?,
                halt: layout::halt(elf.symbols(), |name| elf.symbol(name))?,
            },
            layout: Layout {
                data: sram("__data_start")?..sram("__data_end")?,
                bss: sram("__bss_start")?..sram("__bss_end")?,
                load: symbol("__data_load")? as u16,
            },
        })
    }

    fn boot(&self, image: Vec<u8>) -> Result<Sim, String> {
        Sim::new(image, self.layout.bss.end, self.ends)
    }
}

pub fn check(elf: &Elf, examples: bool) -> Result<Report, String> {
    let linked = Linked::of(elf)?;
    let image = elf.flash_image();
    let signed = crc::sign(&image, &stamp::TEST)?;
    let reset = elf.require("__reset")?;

    let power_on = || -> Result<Sim, String> {
        let mut sim = linked.boot(signed.clone())?;
        sim.run_to(linked.main, BOOT_BUDGET)
            .map_err(|e| format!("startup: {e}"))?;
        Ok(sim)
    };
    let sim = power_on()?;
    check_startup(&sim.chip, &linked.layout).map_err(|e| format!("startup: {e}"))?;
    startup_with_statics(&image, reset, &linked)
        .map_err(|e| format!("startup with statics: {e}"))?;
    found_lit(linked.boot(signed.clone())?).map_err(|e| format!("starting with PC1 high: {e}"))?;
    boot_order(linked.boot(signed.clone())?).map_err(|e| format!("start-up order: {e}"))?;
    unhandled_interrupt(power_on()?)
        .map_err(|e| format!("with an interrupt that has no handler: {e}"))?;
    let examples = if examples { EXAMPLES } else { &[] };
    // Errors come back in this order. The late ones would report many a
    // mistake as a wrong output before the run that names it.
    let faults: [(FaultCheck, &str); 6] = [
        (slow_ticks, "with the slowest ticks"),
        (oscillator_stops, "with the 32 kHz oscillator stopped"),
        (adc_never_answers, "with a dead ADC"),
        (adc_stuck, "with the ADC stuck on one reading"),
        (switch_stuck, "with the switch stuck down"),
        (vddio2_missing, "with VDDIO2 missing"),
    ];
    let late_faults: [(FaultCheck, &str); 9] = [
        (cell_collapses, "with a collapsing cell"),
        (
            |sim, report| bad_bus(sim, Fault::Absent, report),
            "with the AW2016 never answering",
        ),
        (
            |sim, report| bad_bus(sim, Fault::SdaLow, report),
            "with SDA held low",
        ),
        (
            |sim, report| bad_bus(sim, Fault::SclLow, report),
            "with SCL held low",
        ),
        (vddio2_drops, "with VDDIO2 going while lit"),
        (
            |sim, report| resting(sim, PitPhase::Aligned, report),
            "resting, the PIT aligned",
        ),
        (
            |sim, report| resting(sim, PitPhase::Early, report),
            "resting, the PIT's first tick early",
        ),
        (
            |sim, report| resting(sim, PitPhase::Late, report),
            "resting, the PIT's first tick late",
        ),
        (stale_edges, "resting, with an edge on every slow tick"),
    ];
    let power_on = &power_on;
    let reports: Vec<Result<Report, String>> = thread::scope(|scope| {
        let spawn = |(check, name): (FaultCheck, &'static str)| {
            scope.spawn(move || {
                let mut report = Report::default();
                check(power_on()?, &mut report).map_err(|e| format!("{name}: {e}"))?;
                Ok(report)
            })
        };
        let faults = faults.map(spawn);
        // Spawning every run before joining any is what runs them in parallel.
        let runs: Vec<_> = examples
            .iter()
            .map(Some)
            .chain([None])
            .map(|example| scope.spawn(move || both_ways(power_on, example)))
            .collect();
        let late_faults = late_faults.map(spawn);
        faults
            .into_iter()
            .chain(runs)
            .chain(late_faults)
            .map(|run| run.join().unwrap_or_else(|_| Err("a run panicked".into())))
            .collect()
    });

    let mut report = Report::default();
    for run in reports {
        report.merge(&run?);
    }
    if report.off_instructions == 0 {
        return Err("never powered down".into());
    }
    if report.landed == 0 {
        return Err("no interrupt ever landed mid-pass, so this checked nothing".into());
    }
    Ok(report)
}

type FaultCheck = fn(Sim, &mut Report) -> Result<(), String>;

const LANDING_MARGIN: u32 = 2 * TICKS_PER_SECOND as u32;

fn both_ways(
    power_on: &impl Fn() -> Result<Sim, String>,
    example: Option<&Example>,
) -> Result<Report, String> {
    let mut report = run(power_on, example, Landing::Watch)?;
    let until = report.last_new + LANDING_MARGIN;
    report.merge(&run(power_on, example, Landing::Until(until))?);
    Ok(report)
}

fn run(
    power_on: &impl Fn() -> Result<Sim, String>,
    example: Option<&Example>,
    landing: Landing,
) -> Result<Report, String> {
    let mut sim = power_on()?;
    sim.landing = landing;
    let landing = matches!(landing, Landing::Until(_));
    let mut report = Report::default();
    let result = match example {
        Some(example) => {
            ui_example(sim, example, &mut report).map_err(|e| format!("{}: {e}", example.name))
        }
        None => against_the_library(sim, &mut report).map_err(|e| format!("random switching: {e}")),
    };
    match (result, landing) {
        (Ok(()), _) => Ok(report),
        (Err(e), true) => Err(format!("with interrupts mid-pass: {e}")),
        (Err(e), false) => Err(e),
    }
}

const RANDOM_TICKS: usize = 20_000;

fn ui_example(mut sim: Sim, example: &Example, report: &mut Report) -> Result<(), String> {
    // The run without landing has checked the level.
    let Some(level) = in_step(&mut sim, testing::samples(example.actions()), report)? else {
        return Ok(());
    };
    if example.expect.holds(level) {
        Ok(())
    } else {
        Err(format!(
            "expected {:?}, but it ends at level {level}",
            example.expect
        ))
    }
}

fn against_the_library(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    let samples = RandomSwitching::new(0x5EED)
        .flat_map(|(pressed, ticks)| iter::repeat_n(pressed, ticks.into()))
        .take(RANDOM_TICKS)
        .map(|pressed| Sample {
            pressed,
            scene: Scene::NORMAL,
        });
    in_step(&mut sim, samples, report).map(drop)
}

fn lit(sim: &mut Sim, ticks: u16, report: &mut Report) -> Result<(), String> {
    let script = [Action::Clicks(1), Action::Wait(ticks)];
    in_step(sim, testing::samples(script), report)?;
    if !sim.chip.enabled_high() {
        return Err("a click from off left PC1 low, so there's no light to check".into());
    }
    Ok(())
}

fn ends_off(end: Option<u8>) -> Result<(), String> {
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
    Ok(())
}

/// The slowest tick the oscillators allow, in main clocks, with the 32 kHz
/// one 30% slow and the main one 5% fast (figure 38-2). Table 38-10 gives
/// OSC32K no tolerance at all, so the 30% is a guess.
const SLOWEST_TICK_CYCLES: u64 = TICK_CYCLES * 105 / 70;
const DEADLINE_CYCLES: u64 = TICK_DEADLINE_PERIODS as u64 * TIMER_PERIOD;
const _: () = assert!(4 * SLOWEST_TICK_CYCLES < DEADLINE_CYCLES);

/// The model's watchdog never sees the slow oscillator.
fn slow_ticks(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    sim.chip.tick_cycles = SLOWEST_TICK_CYCLES;
    lit(&mut sim, 2 * TICKS_PER_SECOND, report)
}

fn adc_never_answers(sim: Sim, report: &mut Report) -> Result<(), String> {
    let seconds = |s: usize| s * usize::from(TICKS_PER_SECOND);
    let script = [
        Action::Clicks(1),
        Action::Wait(10 * TICKS_PER_SECOND),
        Action::Clicks(1),
        Action::Wait(2 * TICKS_PER_SECOND),
    ];
    let levels = std::cell::RefCell::new(Vec::new());
    let mut shadow = Rig::default();
    let samples = testing::samples(script).map(|sample| {
        let sample = Sample {
            scene: Scene {
                adc_dead: true,
                ..sample.scene
            },
            ..sample
        };
        shadow.tick_in(sample.pressed, &sample.scene);
        levels.borrow_mut().push(shadow.level());
        sample
    });
    let mut sim = sim;
    let end = in_step(&mut sim, samples, report)?;
    let levels = levels.into_inner();
    ends_off(end)?;
    let lit = levels
        .iter()
        .position(|&level| level > 0)
        .ok_or("the light never came on")?;
    let out = lit + seconds(4);
    if !levels.get(out..).is_some_and(|l| l.iter().all(|&l| l == 0)) {
        return Err(format!(
            "the light isn't out and staying out from 4 s after it came on ({:?} at {out})",
            levels.get(out)
        ));
    }
    Ok(())
}

/// From the cell falling under the floor to PC1 low, both counted. Typed as a
/// number on purpose, not worked out from the library's constants.
const COLLAPSE_TICKS: usize = 12;

fn cell_collapses(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    let rounds = u16::from(CYCLE_TICKS);
    let script = (0..rounds).flat_map(|round| {
        [
            Action::Cell(380),
            Action::Clicks(1),
            Action::Hold {
                clicks: 0,
                ticks: 2 * TICKS_PER_SECOND + round,
            },
            Action::Cell(COLLAPSE_BELOW - 1),
            Action::Wait(TICKS_PER_SECOND),
            Action::Clicks(1),
            Action::Wait(TICKS_PER_SECOND),
        ]
    });
    let ticks = std::cell::RefCell::new(Vec::new());
    let mut shadow = Rig::default();
    let samples = testing::samples(script).inspect(|sample| {
        shadow.tick_in(sample.pressed, &sample.scene);
        ticks
            .borrow_mut()
            .push((sample.scene.cell, shadow.level(), shadow.aux()));
    });
    let end = in_step(&mut sim, samples, report)?;
    let ticks = ticks.into_inner();
    ends_off(end)?;
    let good = |tick: &(u16, u8, Aux)| tick.0 >= COLLAPSE_BELOW;
    let mut slowest = 0;
    let mut falls = 0;
    for fell in 1..ticks.len() {
        if !good(&ticks[fell - 1]) || good(&ticks[fell]) {
            continue;
        }
        falls += 1;
        let at = |e: String| format!("the cell falls on tick {fell}: {e}");
        if ticks[fell - 1].1 != CEILING {
            return Err(at("the light isn't at the top of the ramp".into()));
        }
        let down = ticks[fell..].iter().take_while(|tick| !good(tick)).count();
        let lit = ticks[fell..fell + down]
            .iter()
            .take_while(|tick| tick.1 > 0)
            .count();
        if lit >= COLLAPSE_TICKS {
            return Err(at(format!(
                "PC1 is still high {lit} ticks on; it has {COLLAPSE_TICKS}"
            )));
        }
        if let Some(faded) = ticks[fell..fell + lit]
            .iter()
            .find(|tick| tick.1 != CEILING)
        {
            return Err(at(format!(
                "the light went to level {} on its way out, where a collapse is off at once",
                faded.1
            )));
        }
        if ticks[fell + lit].2 != Aux::Yellow {
            return Err(at(format!(
                "the aux show {:?} as it goes out, not a flat cell's yellow",
                ticks[fell + lit].2
            )));
        }
        if let Some(relit) = ticks[fell + lit..fell + down].iter().position(|t| t.1 > 0) {
            return Err(at(format!(
                "lit again {relit} ticks after it went out, with the cell still down"
            )));
        }
        slowest = slowest.max(lit + 1);
    }
    if falls != usize::from(rounds) {
        return Err(format!("the cell fell {falls} times, not {rounds}"));
    }
    if slowest != COLLAPSE_TICKS {
        return Err(format!(
            "the slowest took {slowest} ticks, not the {COLLAPSE_TICKS} the worst tick of the \
             round should, so this didn't reach it"
        ));
    }
    Ok(())
}

fn adc_stuck(sim: Sim, report: &mut Report) -> Result<(), String> {
    let seconds = |s: usize| s * usize::from(TICKS_PER_SECOND);
    let script = [
        Action::Clicks(1),
        Action::Wait(TICKS_PER_SECOND),
        Action::AdcStuck,
        Action::Wait(5 * TICKS_PER_SECOND),
        Action::Clicks(1),
        Action::Wait(2 * TICKS_PER_SECOND),
    ];
    let ticks = std::cell::RefCell::new(Vec::new());
    let mut shadow = Rig::default();
    let samples = testing::samples(script).inspect(|sample| {
        shadow.tick_in(sample.pressed, &sample.scene);
        let fault = shadow.health() == kr1k::limits::Health::Fault;
        ticks
            .borrow_mut()
            .push((sample.scene.adc_stuck.is_some(), shadow.level(), fault));
    });
    let mut sim = sim;
    let end = in_step(&mut sim, samples, report)?;
    let ticks = ticks.into_inner();
    ends_off(end)?;
    let stuck = ticks
        .iter()
        .position(|tick| tick.0)
        .ok_or("the ADC never stuck")?;
    if ticks[stuck - 1].1 == 0 || ticks[..stuck].iter().any(|tick| tick.2) {
        return Err("the light wasn't on and well before the ADC stuck".into());
    }
    let out = stuck + seconds(2);
    if !ticks
        .get(out..)
        .is_some_and(|t| t.iter().all(|t| t.1 == 0 && t.2))
    {
        return Err(format!(
            "the light isn't out as a fault and staying so from 2 s after the ADC stuck \
             ({:?} at {out})",
            ticks.get(out)
        ));
    }
    Ok(())
}

fn switch_stuck(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    let samples = iter::repeat_n(
        Sample {
            pressed: true,
            scene: Scene::NORMAL,
        },
        20_000,
    );
    let end = in_step(&mut sim, samples, report)?;
    if end != Some(kr1k::ui::FLOOR) {
        return Err(format!("ends at level {end:?}, not the floor"));
    }
    Ok(())
}

fn vddio2_missing(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    let script = [
        Action::Vddio2(false),
        Action::Wait(TICKS_PER_SECOND),
        Action::Clicks(1),
        Action::Wait(TICKS_PER_SECOND),
        Action::Hold {
            clicks: 0,
            ticks: TICKS_PER_SECOND,
        },
        Action::Wait(TICKS_PER_SECOND),
    ];
    let end = in_step(&mut sim, testing::samples(script), report)?;
    if sim.chip.vddio2_ok() {
        return Err("the fuses have single supply, so VDDIO2S reads 1 regardless".into());
    }
    ends_off(end)?;
    if sim.chip.pc1_rises != 0 {
        return Err("raised PC1 with VDDIO2 missing from power-on".into());
    }
    Ok(())
}

/// Dips shorter than a tick aren't run, since a firmware that looks once a
/// pass can miss one whole and `Chip::vddio2_moved` would refuse what follows.
fn vddio2_drops(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    sim.chip.late_vddio2 = true;
    let second = TICKS_PER_SECOND;
    let script = [
        Action::Clicks(1),
        Action::Hold {
            clicks: 0,
            ticks: 3 * second,
        },
        Action::Vddio2(false),
        Action::Wait(second),
        Action::Clicks(1),
        Action::Hold {
            clicks: 0,
            ticks: second,
        },
        Action::Vddio2(true),
        Action::Wait(second),
        Action::Clicks(1),
        Action::Wait(second),
    ];
    let end = in_step(&mut sim, testing::samples(script), report)?;
    if !sim.chip.lost_lit {
        return Err("VDDIO2 never went with the light on, so this checked nothing".into());
    }
    ends_off(end)?;
    if sim.chip.pc1_rises != 1 {
        return Err(format!(
            "PC1 rose {} times, not just the once before VDDIO2 went",
            sim.chip.pc1_rises
        ));
    }
    Ok(())
}

fn resting(mut sim: Sim, phase: PitPhase, report: &mut Report) -> Result<(), String> {
    sim.chip.pit_phase = phase;
    let second = TICKS_PER_SECOND;
    let script = [
        Action::Wait(20 * second),
        Action::Clicks(1),
        Action::Wait(3 * second),
        Action::Clicks(1),
        Action::Wait(12 * second + 17),
        Action::Clicks(3),
        Action::Wait(14 * second),
        Action::Clicks(3),
        Action::Wait(9 * second + 40),
        Action::Hold {
            clicks: 1,
            ticks: 2 * second,
        },
        Action::Wait(10 * second + 5),
        Action::Clicks(1),
        Action::Wait(6 * second),
        Action::Clicks(1),
        Action::Wait(second),
    ];
    let end = in_step(&mut sim, testing::samples(script), report)?;
    ends_off(end)?;
    if sim.chip.slow_ticks < 40 {
        return Err(format!(
            "only {} slow ticks in over a minute of rests, so this checked little",
            sim.chip.slow_ticks
        ));
    }
    if sim.chip.pc1_rises < 2 {
        return Err("the light didn't come on twice, so this checked nothing".into());
    }
    Ok(())
}

fn stale_edges(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    sim.chip.edge_with_slow_tick = true;
    let second = TICKS_PER_SECOND;
    let script = [
        Action::Wait(12 * second),
        Action::Clicks(1),
        Action::Wait(2 * second),
        Action::Clicks(1),
        Action::Wait(12 * second),
    ];
    let end = in_step(&mut sim, testing::samples(script), report)?;
    ends_off(end)?;
    if sim.chip.edges_with_ticks < 10 {
        return Err(format!(
            "only {} edges came with slow ticks, so this checked little",
            sim.chip.edges_with_ticks
        ));
    }
    Ok(())
}

fn bad_bus(mut sim: Sim, fault: Fault, report: &mut Report) -> Result<(), String> {
    sim.chip.twi.fault = fault;
    let second = TICKS_PER_SECOND;
    let script = [
        Action::Clicks(1),
        Action::Wait(second),
        Action::Clicks(1),
        Action::Wait(second),
        Action::Clicks(3),
        Action::Wait(second),
        Action::Clicks(1),
        Action::Wait(6 * second),
        Action::Clicks(3),
        Action::Wait(second),
        Action::Clicks(1),
        Action::Hold {
            clicks: 0,
            ticks: second,
        },
        Action::Wait(second),
        Action::Clicks(1),
        Action::Wait(second),
    ];
    let end = in_step(&mut sim, testing::samples(script), report)?;
    ends_off(end)?;
    if sim.chip.pc1_rises < 2 {
        return Err("the light didn't come on twice, so this checked nothing".into());
    }
    let twi = &sim.chip.twi;
    if twi.failures == 0 || twi.failures != twi.sessions {
        return Err(format!(
            "{} of {} updates failed, but every one should have",
            twi.failures, twi.sessions
        ));
    }
    Ok(())
}

/// Instructions from dropping PC1 to the `sleep`.
const HALT_BUDGET: u64 = 100;
/// PIT periods.
const DOWN_PERIODS: u64 = 4 * TICKS_PER_SECOND as u64;

fn oscillator_stops(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    lit(&mut sim, 0, report)?;
    sim.chip.osc32k_stopped = true;
    let stopped = sim.chip.now;
    while sim.chip.enabled_high() {
        sim.step()?;
        if sim.chip.now - stopped > DEADLINE_CYCLES + TICK_BUDGET {
            return Err(format!(
                "PC1's still high {} TCB0 periods on, past the deadline of \
                 {TICK_DEADLINE_PERIODS}",
                (sim.chip.now - stopped) / TIMER_PERIOD
            ));
        }
    }
    let took = sim.chip.now - stopped;
    // The first period of the wait is a part of one.
    if took < DEADLINE_CYCLES - TIMER_PERIOD {
        return Err(format!(
            "PC1 dropped after {} TCB0 periods, before the deadline of {TICK_DEADLINE_PERIODS}",
            took / TIMER_PERIOD
        ));
    }

    for _ in 0..HALT_BUDGET {
        if sim.chip.down {
            break;
        }
        sim.step()?;
        if sim.chip.enabled_high() {
            return Err("dropped PC1, then raised it again".into());
        }
    }
    if !sim.chip.down {
        return Err(format!(
            "dropped PC1, but didn't stop for good within {HALT_BUDGET} instructions"
        ));
    }
    if !sim.chip.powering_down() {
        return Err("stopped for good in idle, which leaves the main clock running".into());
    }
    let until = sim.chip.now + DOWN_PERIODS * TICK_CYCLES;
    while sim.chip.now < until {
        sim.step()?;
        if !sim.cpu.asleep || sim.chip.enabled_high() {
            return Err("woke up or raised PC1 after stopping for good".into());
        }
    }
    // `hw::halt` is a call deeper than the loop.
    report.stack = report.stack.max(sim.stack());
    Ok(())
}

/// Nothing in the firmware touches TCB1 and the model doesn't have it.
const VECTOR_UNHANDLED: u8 = regs::VECTOR_TCB1_INT as u8;

/// Resets aren't modelled, so the run stops one PIT period short of the bite.
fn unhandled_interrupt(mut sim: Sim) -> Result<(), String> {
    if sim.handled & 1 << VECTOR_UNHANDLED != 0 {
        return Err(format!(
            "{} has a handler now, so pick another vector to force",
            vector_name(VECTOR_UNHANDLED)
        ));
    }
    lit(&mut sim, TICKS_PER_SECOND / 4, &mut Report::default())?;
    let mut fed = false;
    for _ in 0..=FEED_TICKS {
        let before = sim.chip.wdt.feeds;
        sim.tick(false)?;
        if sim.chip.wdt.feeds > before {
            fed = true;
            break;
        }
    }
    if !fed {
        return Err(format!(
            "no wdr within {FEED_TICKS} ticks of the light coming on"
        ));
    }
    sim.chip.force_interrupt(VECTOR_UNHANDLED);
    let mut steps = 0;
    while !sim.chip.down {
        if steps == HALT_BUDGET {
            return Err(format!(
                "didn't stop for good within {HALT_BUDGET} instructions of {} being forced",
                vector_name(VECTOR_UNHANDLED)
            ));
        }
        sim.step()?;
        steps += 1;
    }
    if sim.forced != Some(VECTOR_UNHANDLED) || sim.cpu.handling != sim.forced {
        return Err("stopped for good without taking the forced interrupt".into());
    }
    if !sim.abandoned {
        return Err("stopped for good without going through hw::halt".into());
    }
    sim.chip
        .shows(Output::OFF)
        .map_err(|e| format!("stopped for good, but {e}"))?;
    if !sim.chip.powering_down() {
        return Err("stopped for good in idle, which leaves the main clock running".into());
    }

    let periods = sim.chip.wdt_periods_left();
    if periods == 0 {
        return Err(
            "stopped for good with the watchdog about to bite, so nothing's left to watch".into(),
        );
    }
    let (ticks, until) = (
        sim.chip.ticks,
        sim.chip.next_pit + (periods - 1) * sim.chip.tick_cycles,
    );
    while sim.chip.now < until {
        sim.step()?;
        if !sim.cpu.asleep || sim.cpu.handling != sim.forced || sim.chip.enabled_high() {
            return Err("woke up, took an interrupt or raised PC1 after stopping for good".into());
        }
    }
    if sim.chip.ticks == ticks {
        return Err(
            "no PIT period went by after stopping for good, so this checked nothing".into(),
        );
    }
    Ok(())
}

/// Returns the level it ends at, or `None` if `Landing::Until` cut it short.
fn in_step(
    sim: &mut Sim,
    samples: impl IntoIterator<Item = Sample>,
    report: &mut Report,
) -> Result<Option<u8>, String> {
    // The fuses are read before the first tick.
    let mut samples = samples.into_iter().peekable();
    let power_on = World::from(samples.peek().map_or(Scene::NORMAL, |s| s.scene));
    sim.chip.set_world(power_on)?;
    if power_on.scene.corrupt_flash {
        // The checksum's high byte, which is never code.
        let byte = sim
            .chip
            .flash
            .get_mut(crc::CHECKSUM_AT)
            .ok_or("can't corrupt flash: the image isn't padded to all of flash")?;
        *byte ^= 0x01;
    }
    sim.tick(false).map_err(|e| format!("hw::init: {e}"))?;
    sim.chip
        .clocked_as_modelled()
        .map_err(|e| format!("hw::init: {e}"))?;
    if sim.chip.wdt_open() == 0 {
        return Err("hw::start_watchdog never turned the watchdog on".into());
    }
    let mut rig = Paced::new(power_on.faults(), sim.chip.grid);
    rig.phase = sim.chip.pit_phase;
    rig.bus_fails = sim.chip.twi.fault != Fault::None;
    let mut was_resting = false;
    let mut finished = true;
    let mut aux = AuxWatch::default();
    for (tick, sample) in samples.enumerate() {
        let at = |e: String| format!("tick {tick}: {e}");
        sim.tick_no = tick as u32;
        let landing = match sim.landing {
            Landing::Until(until) if sim.tick_no > until => {
                finished = false;
                break;
            }
            Landing::Until(_) => true,
            Landing::Off | Landing::Watch => false,
        };
        sim.chip.enter(World::from(sample.scene)).map_err(at)?;
        // Not the scene's with single-supply fuses or VDDIO2 going mid-pass.
        let scene = Scene {
            vddio2: sim.chip.vddio2_ok(),
            ..sample.scene
        };
        let (power_downs, reads, feeds, late_edges) = (
            sim.chip.power_downs,
            sim.chip.switch_reads,
            sim.chip.wdt.feeds,
            sim.late_edges,
        );
        let took = sim.tick(sample.pressed).map_err(at)?;
        let mut ends: Vec<End> = rig.sample(sample.pressed, &scene).into_iter().collect();
        let passes = (sim.chip.switch_reads - reads) as usize;
        // A late bounce wakes a slow sleep with nothing to see.
        while passes > ends.len() && sim.late_edges > late_edges {
            match rig.bounce(&scene) {
                Some(end) => ends.push(end),
                None => break,
            }
        }
        if passes != ends.len() {
            return Err(at(format!(
                "the firmware ran {passes} passes on this tick, but the library {}",
                ends.len()
            )));
        }
        let Some(&end) = ends.last() else {
            if sim.chip.wdt.feeds != feeds {
                return Err(at("fed the watchdog with no pass to run".into()));
            }
            sim.chip.shows(rig.output()).map_err(at)?;
            continue;
        };
        if !landing {
            report.loop_instructions = report.loop_instructions.max(took);
        }
        let powered_down = sim.chip.power_downs > power_downs;
        if powered_down && was_resting {
            report.off_instructions = report.off_instructions.max(took);
        }
        sim.chip.shows(rig.output()).map_err(at)?;
        if !sim.chip.aux_unwired {
            aux.tick(&sim.chip.twi, rig.aux()).map_err(at)?;
        }
        was_resting = rig.resting();
        // A firmware that never feeds at all is the bite's to catch.
        let fed = ends.iter().filter(|end| end.feed).count();
        if sim.chip.wdt.feeds > 0 && (sim.chip.wdt.feeds - feeds) as usize != fed {
            return Err(at(format!(
                "the library feeds the watchdog {fed} times on this tick, but the firmware {}",
                sim.chip.wdt.feeds - feeds
            )));
        }
        if sim.chip.pit() != rig.pit() {
            return Err(at(format!(
                "the library has the PIT {:?}, but the firmware {:?}",
                rig.pit(),
                sim.chip.pit()
            )));
        }
        if powered_down != end.power_down {
            return Err(at(format!(
                "the library says {}, but the firmware {}",
                if end.power_down { "power down" } else { "idle" },
                if powered_down {
                    "powered down"
                } else {
                    "idled"
                },
            )));
        }
    }
    report.take(sim);
    Ok(finished.then(|| rig.level()))
}

/// Ticks the aux may stay dark after the torch wants a colour.
const AUX_CATCH_UP_TICKS: u8 = 2;

#[derive(Default)]
struct AuxWatch {
    behind: u8,
    sessions: u32,
    failures: u32,
    gave_up: Option<Aux>,
}

impl AuxWatch {
    fn tick(&mut self, twi: &Twi, want: Aux) -> Result<(), String> {
        let started = twi.sessions - self.sessions;
        let failed = twi.failures > self.failures;
        (self.sessions, self.failures) = (twi.sessions, twi.failures);
        if started > 1 {
            return Err(format!("turns the TWI host on {started} times in one pass"));
        }
        if self.gave_up.is_some_and(|gave_up| gave_up != want) {
            self.gave_up = None;
        }
        if started > 0 && self.gave_up == Some(want) {
            return Err(format!(
                "tries the AW2016 again with the torch still wanting {want:?}, after a failed \
                 update: it gives up until the colour changes"
            ));
        }
        if failed {
            self.gave_up = Some(want);
        }
        if twi.fault != Fault::None {
            return Ok(());
        }
        let shown = twi.aw2016.colour()?;
        if want == Aux::Off {
            self.behind = 0;
            if twi.aw2016.awake() {
                return Err(
                    "the torch wants the aux off, but the AW2016 isn't in standby, so it draws \
                     its active current (H13 in docs/safety.md)"
                        .into(),
                );
            }
            return Ok(());
        }
        let brightness = |bright: bool| if bright { "bright" } else { "dim" };
        match shown {
            Some((colour, bright)) if colour == want && bright == want.bright() => {
                self.behind = 0;
                Ok(())
            }
            Some((colour, bright)) => Err(format!(
                "the aux show {} {colour:?}, but the torch wants {} {want:?}",
                brightness(bright),
                brightness(want.bright())
            )),
            None => {
                self.behind += 1;
                if self.behind > AUX_CATCH_UP_TICKS {
                    return Err(format!(
                        "the aux are still dark {} ticks after the torch wanted {want:?}",
                        self.behind
                    ));
                }
                Ok(())
            }
        }
    }
}

/// The probe's .rodata table, then its .data static's starting values, then
/// a byte it doesn't touch.
const PROBE_EXPECTED: [u8; 7] = [0x12, 0x34, 0x56, 0x9A, 0xBC, 0xDE, 0];

pub fn probe(elf: &Elf) -> Result<(), String> {
    let linked = Linked::of(elf)?;
    let layout = &linked.layout;
    let rodata = elf.section(".rodata").ok_or("no .rodata section")?;
    if rodata.is_empty() || layout.data.is_empty() || layout.bss.is_empty() {
        return Err(".rodata, .data or .bss is empty, so there's nothing to check".into());
    }
    let mut sim = linked.boot(elf.flash_image())?;
    sim.run_to(linked.main, BOOT_BUDGET)
        .and_then(|()| check_startup(&sim.chip, layout))
        .map_err(|e| format!("startup: {e}"))?;
    sim.run_to(elf.require("link_probe_done")? / 2, BOOT_BUDGET)?;

    let copy = (elf.require("LINK_PROBE_COPY")? - DATA_SPACE) as u16;
    let got = sim.chip.sram(&(copy..copy + PROBE_EXPECTED.len() as u16));
    if got != PROBE_EXPECTED {
        return Err(format!(
            "LINK_PROBE_COPY holds {got:02x?}, expected {PROBE_EXPECTED:02x?}"
        ));
    }
    Ok(())
}

pub fn guards(elf: &Elf) -> Result<(), String> {
    let linked = Linked::of(elf)?;
    let mut sim = linked.boot(elf.flash_image())?;
    sim.run_to(
        elf.require("guards_done")? / 2,
        BOOT_BUDGET + 8 * TICK_BUDGET,
    )?;
    match sim.chip.power_downs {
        1 => Ok(()),
        n => Err(format!(
            "powered down {n} times, but only its one wait with the light off should"
        )),
    }
}

/// Data-space addresses.
struct Layout {
    data: Range<u16>,
    bss: Range<u16>,
    load: u16,
}

fn check_startup(chip: &Chip, layout: &Layout) -> Result<(), String> {
    let from = usize::from(layout.load.wrapping_sub(MAPPED));
    let len = usize::from(layout.data.end - layout.data.start);
    let initial = chip
        .flash
        .get(from..from + len)
        .ok_or(".data's initial values are outside flash")?;
    if chip.sram(&layout.data) != initial {
        return Err(".data in SRAM doesn't match its initial values in flash".into());
    }
    if let Some((i, b)) = chip
        .sram(&layout.bss)
        .iter()
        .enumerate()
        .find(|&(_, &b)| b != 0)
    {
        return Err(format!(
            ".bss isn't zeroed: {:#06x} holds {b:#04x}",
            usize::from(layout.bss.start) + i
        ));
    }
    Ok(())
}

/// `reset` is the byte address of `__reset`.
fn startup_with_statics(image: &[u8], reset: u32, linked: &Linked) -> Result<(), String> {
    let values = [0x11, 0x22, 0x33, 0x44, 0x55];
    let mut image = image.to_vec();
    image.resize(image.len().next_multiple_of(2), 0xFF);
    let load = MAPPED + image.len() as u16;
    image.extend(values);

    // An odd start, away from the edges of SRAM, so off-by-ones show.
    let start = SRAM.start + 0x11;
    let moved = Layout {
        data: start..start + 5,
        bss: start + 5..start + 12,
        load,
    };
    patch_startup(&mut image, reset as usize, &linked.layout, &moved)?;

    let mut sim = Sim::new(image, moved.bss.end, linked.ends)?;
    sim.run_to(linked.main, BOOT_BUDGET)?;
    check_startup(&sim.chip, &moved)?;
    for edge in [moved.data.start - 1, moved.bss.end] {
        if sim.chip.sram(&(edge..edge + 1)) != [JUNK] {
            return Err(format!("wrote to {edge:#06x}, outside .data and .bss"));
        }
    }
    Ok(())
}

fn patch_startup(
    image: &mut [u8],
    reset: usize,
    linked: &Layout,
    moved: &Layout,
) -> Result<(), String> {
    // (register, byte) for each `ldi`, in the order rt.rs loads them.
    let loads = |l: &Layout| -> Vec<(u8, u8)> {
        [
            (26, l.data.start),
            (30, l.load),
            (24, l.data.end),
            (26, l.bss.start),
            (24, l.bss.end),
        ]
        .into_iter()
        .flat_map(|(r, v)| {
            let [lo, hi] = v.to_le_bytes();
            [(r, lo), (r + 1, hi)]
        })
        .collect()
    };

    let mut found = Vec::new();
    let mut at = reset;
    loop {
        let w = word(image, at).ok_or("no call to main after __reset")?;
        if is_call(w) {
            break; // call main
        }
        if let Some((r, k)) = ldi_operands(w) {
            found.push((at, r, k));
        }
        at += if is_two_words(w) { 4 } else { 2 };
    }

    let actual: Vec<(u8, u8)> = found.iter().map(|&(_, r, k)| (r, k)).collect();
    if actual != loads(linked) {
        return Err(
            "src/avr/rt.rs's startup doesn't load the registers sim.rs expects; update `patch_startup`"
                .into(),
        );
    }
    for (&(at, r, _), (_, k)) in found.iter().zip(loads(moved)) {
        image[at..at + 2].copy_from_slice(&ldi(r, k).to_le_bytes());
    }
    Ok(())
}

/// Not a model of a stray jump to reset. Only PORTC starts dirty.
fn found_lit(mut sim: Sim) -> Result<(), String> {
    sim.chip.io[reg::VPORTC_DIR] = ENABLE;
    sim.chip.io[reg::VPORTC_OUT] = ENABLE;
    for _ in 0..BOOT_BUDGET {
        if sim.chip.register_writes == 0 {
            sim.step()?;
        } else if sim.chip.io[reg::VPORTC_OUT] & ENABLE == 0 {
            return Ok(());
        } else {
            return Err(
                "the first register it writes leaves PC1 set in VPORTC.OUT, so the boost \
                 converter stays on: clear OUT before DIR, and before anything else"
                    .into(),
            );
        }
    }
    Err(format!(
        "wrote no register within {BOOT_BUDGET} instructions"
    ))
}

fn boot_order(mut sim: Sim) -> Result<(), String> {
    for _ in 0..BOOT_BUDGET {
        if sim.chip.tick_polled {
            return sim.chip.boot.in_order();
        }
        sim.step()?;
    }
    Err(format!(
        "didn't wait for a tick within {BOOT_BUDGET} instructions"
    ))
}

/// `unhandled` is a word address.
fn handled(image: &[u8], unhandled: u32) -> u64 {
    (1..VECTORS).fold(0, |mask, v| {
        let at = v * VECTOR_WORDS as usize * 2;
        if jmp_target(image, at).is_some_and(|target| target / 2 != unhandled) {
            mask | 1 << v
        } else {
            mask
        }
    })
}

struct Landed {
    r: [u8; 32],
    sreg: u8,
    sp: u16,
}

struct Sim {
    cpu: Cpu,
    chip: Chip,
    /// SP below this means a push landed in .bss.
    stack_floor: u16,
    lowest_sp: u16,
    /// Leaves out handlers' instructions while `land` is landing interrupts.
    counted: u64,
    /// Word address.
    unhandled: u32,
    /// Word address.
    halt: u32,
    handled: u64,
    forced: Option<u8>,
    abandoned: bool,
    landed_on: Option<Landed>,
    handler_steps: u64,
    longest_handler: u64,
    landing: Landing,
    /// Indexed by word address.
    reached: Vec<bool>,
    tick_no: u32,
    last_new: u32,
    landed: u32,
    /// Switch edges handled since the main loop last read it, with no tick
    /// since. Only those can end a slow sleep with no library pass to match.
    late_edges: u32,
}

#[derive(Clone, Copy)]
enum Landing {
    Off,
    Watch,
    Until(u32),
}

impl Sim {
    fn new(image: Vec<u8>, bss_end: u16, ends: Ends) -> Result<Self, String> {
        if image.len() > FLASH_SIZE {
            return Err(format!("{} bytes won't fit in flash", image.len()));
        }
        let cpu = Cpu::new(&SRAM);
        let unhandled = ends.unhandled / 2;
        let handled = handled(&image, unhandled);
        let chip = Chip::new(image);
        Ok(Self {
            lowest_sp: cpu.sp,
            cpu,
            handled,
            chip,
            stack_floor: bss_end - 1,
            counted: 0,
            unhandled,
            halt: ends.halt / 2,
            forced: None,
            abandoned: false,
            landed_on: None,
            handler_steps: 0,
            longest_handler: 0,
            landing: Landing::Off,
            reached: vec![false; FLASH_SIZE / 2],
            tick_no: 0,
            last_new: 0,
            landed: 0,
            late_edges: 0,
        })
    }

    /// A landed CAPT counts towards `hw::wait_for_tick`'s deadline, so the
    /// run that checks the deadline leaves landing off.
    fn land(&mut self) {
        if matches!(self.landing, Landing::Off) || self.cpu.handling.is_some() || self.cpu.asleep {
            return;
        }
        let chip = &mut self.chip;
        let bounce =
            chip.enabled & 1 << VECTOR_PORTD != 0 && senses(chip.io[SWITCH_CTRL] & PORT_ISC);
        let period = chip.enabled & 1 << VECTOR_TCB0 != 0;
        if !bounce && !period {
            return;
        }
        if let Some(seen @ false) = self.reached.get_mut(self.cpu.pc as usize) {
            *seen = true;
            self.last_new = self.tick_no;
            if let Landing::Until(_) = self.landing {
                if bounce {
                    chip.portd_flags |= SWITCH;
                }
                if period && !chip.tcb.capt {
                    chip.tcb.capt = true;
                    chip.tcb.capt_up_at = chip.now;
                }
                self.landed += 1;
            }
        }
    }

    /// Bytes.
    const fn stack(&self) -> u16 {
        SRAM.end - 1 - self.lowest_sp
    }

    fn step(&mut self) -> Result<(), String> {
        let pc = self.cpu.pc;
        let awake = !self.cpu.asleep;
        let handling = self.cpu.handling;
        // `hw::halt` gets the main loop's rules, wherever it was reached from.
        self.chip.in_handler = handling.is_some() && !self.abandoned;
        self.cpu.step(&mut self.chip)?;
        // §13.3.1 and figure 15-1. Neither says the other's enough, so both.
        if self.chip.down && self.cpu.flag(I) {
            return Err(format!(
                "{:#06x}: stops for good with SREG.I still set, which leaves the \
                 interrupt enables alone keeping it down",
                pc * 2
            ));
        }
        self.watch_handler(handling)?;
        if self.cpu.handling.is_some() && self.cpu.pc == self.halt {
            self.abandon();
        }
        if awake {
            let landing = matches!(self.landing, Landing::Until(_));
            if !landing || handling.is_none() || self.abandoned {
                self.counted += 1;
            }
            if self.chip.wdt.polling {
                self.chip.wdt_waits += 1;
            }
            self.chip.elapse()?;
        }
        self.lowest_sp = self.lowest_sp.min(self.cpu.sp);
        if self.cpu.sp < self.stack_floor {
            return Err(format!("{:#06x}: the stack ran into .bss", pc * 2));
        }
        // The real chip would `reti` with the flag still up, and take the
        // interrupt again, forever.
        if self.cpu.flag(I) {
            let orphans = self.chip.enabled & !self.handled;
            if orphans != 0 {
                return Err(format!(
                    "{:#06x}: enables {} with SREG.I set, but its vector goes to \
                     __unhandled_interrupt",
                    pc * 2,
                    vector_name(orphans.trailing_zeros() as u8)
                ));
            }
        }
        if self.cpu.pc == self.unhandled
            && let Some(vector) = self.cpu.handling
            && self.forced != Some(vector)
        {
            return Err(format!(
                "takes {}, whose vector goes to __unhandled_interrupt",
                vector_name(vector)
            ));
        }
        self.land();
        Ok(())
    }

    /// `before` is what the CPU was handling before the step just run.
    fn watch_handler(&mut self, before: Option<u8>) -> Result<(), String> {
        match (before, self.cpu.handling) {
            (None, Some(vector)) => {
                if vector == VECTOR_PORTD && self.chip.switch_read_at >= self.chip.tick_taken_at {
                    self.late_edges += 1;
                }
                if self.chip.forced == Some(vector) {
                    self.chip.forced = None;
                    self.forced = Some(vector);
                }
                self.chip.interrupted = self.chip.temps_in_use();
                self.handler_steps = 0;
                self.landed_on = Some(Landed {
                    r: self.cpu.r,
                    sreg: self.cpu.sreg,
                    sp: self.cpu.sp,
                });
            }
            (Some(vector), None) => {
                self.chip.interrupted = 0;
                self.longest_handler = self.longest_handler.max(self.handler_steps);
                if let Some(landed) = self.landed_on.take() {
                    self.gave_back(&landed)
                        .map_err(|e| format!("the handler for {}: {e}", vector_name(vector)))?;
                }
            }
            (Some(_), Some(_)) if self.abandoned => {}
            (Some(vector), Some(_)) => {
                self.handler_steps += 1;
                if self.handler_steps > HANDLER_BUDGET {
                    return Err(format!(
                        "the handler for {} ran past {HANDLER_BUDGET} instructions, \
                         which might not fit in a TCB0 period",
                        vector_name(vector)
                    ));
                }
            }
            (None, None) => {}
        }
        Ok(())
    }

    const fn abandon(&mut self) {
        self.abandoned = true;
        self.chip.interrupted = 0;
        self.chip.tcb.temp = None;
        self.chip.dac.temp = None;
        self.chip.adc.temp = None;
        self.landed_on = None;
    }

    fn gave_back(&self, landed: &Landed) -> Result<(), String> {
        let changed = (0..32).find(|&n| self.cpu.r[n] != landed.r[n]);
        if let Some(n) = changed {
            return Err(format!(
                "returns with r{n} at {:#04x}, not the {:#04x} it found",
                self.cpu.r[n], landed.r[n]
            ));
        }
        if self.cpu.sreg != landed.sreg {
            return Err(format!(
                "returns with SREG at {:#04x}, not the {:#04x} it found",
                self.cpu.sreg, landed.sreg
            ));
        }
        if self.cpu.sp != landed.sp.wrapping_add(2) {
            return Err(format!(
                "returns with SP at {:#06x}, having found it at {:#06x}",
                self.cpu.sp,
                landed.sp.wrapping_add(2)
            ));
        }
        Ok(())
    }

    /// `target` is a word address.
    fn run_to(&mut self, target: u32, budget: u64) -> Result<(), String> {
        for _ in 0..budget {
            if self.cpu.pc == target {
                return Ok(());
            }
            self.step()?;
        }
        Err(format!(
            "didn't reach {:#06x} within {budget} instructions",
            target * 2
        ))
    }

    /// Instructions waiting on WDT.STATUS.SYNCBUSY count a quarter each,
    /// since the wait lasts as long as the sync whatever they take.
    fn tick(&mut self, pressed: bool) -> Result<u64, String> {
        self.chip.set_switch(pressed)?;
        let (start, grid) = ((self.counted, self.chip.wdt_waits), self.chip.grid);
        self.chip.tick_polled = false;
        let mut pass_end = None;
        let ran = |end: (u64, u64)| {
            let waited = end.1 - start.1;
            end.0 - start.0 - waited + waited / 4
        };
        while self.chip.grid == grid {
            self.step()?;
            if self.chip.down {
                return Err(
                    "sleeps with no interrupt enabled, so only a reset wakes it \
                     (section 13.3.1): it's stopped for good"
                        .into(),
                );
            }
            if self.chip.tick_polled {
                pass_end.get_or_insert((self.counted, self.chip.wdt_waits));
            }
            if ran(pass_end.unwrap_or((self.counted, self.chip.wdt_waits))) > TICK_BUDGET {
                return Err(format!(
                    "a pass of the main loop ran past {TICK_BUDGET} instructions, \
                     so it hangs or misses 64 Hz ticks"
                ));
            }
        }
        Ok(ran(pass_end.unwrap_or((self.counted, self.chip.wdt_waits))))
    }
}

/// §18.5.16
const fn senses(isc: u8) -> bool {
    matches!(isc, ISC_BOTHEDGES | ISC_RISING | ISC_FALLING | ISC_LEVEL)
}

/// Timed in WDT clocks (§22.3.2), taking the worst case where the datasheet
/// is silent. A write to CTRLA doesn't restart the count, and the stricter
/// of the old and new timeouts holds while it syncs. A new window only
/// starts with the next `wdr` (§22.3.3.2). Whether SYNCBUSY covers a `wdr`'s
/// sync isn't stated, so CTRLA isn't written within three WDT clocks of one.
#[derive(Default)]
struct Wdt {
    /// When the last `wdr` ran, or the watchdog was turned on.
    since: u64,
    windowed: bool,
    pending: Option<(u8, u64)>,
    fed_at: Option<u64>,
    polling: bool,
    feeds: u32,
}

/// In WDT clocks, as are the next two.
const WDT_SYNC_CLOCKS: u64 = SYNC_CLOCKS as u64;
/// Three less two (§22.3.6).
const WDR_SPREAD: u32 = 1;
const WDR_SYNC_CLOCKS: u64 = 3;
const WDT_LIT: u8 = regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK;
const _: () = {
    assert!(wdt_clocks(WDT_LIT & WDT_PERIOD) == OPEN_CLOCKS as u32);
    assert!(wdt_clocks((WDT_LIT & WDT_WINDOW) >> 4) == CLOSED_CLOCKS as u32);
};

const fn wdt_clocks(setting: u8) -> u32 {
    match setting {
        0 => 0,
        n => 8 << (n - 1),
    }
}

/// Each in `Chip::now`.
#[derive(Default)]
struct Boot {
    flags_cleared: Option<u64>,
    flash_checked: Option<u64>,
    fuses_read: Option<u64>,
    supply_read: Option<u64>,
    set_up: Option<u64>,
    pit_on: Option<u64>,
    watchdog_on: Option<u64>,
}

impl Boot {
    fn write(&mut self, now: u64, a: usize, value: u8) {
        let first = |slot: &mut Option<u64>| _ = slot.get_or_insert(now);
        match a {
            reg::RSTCTRL_RSTFR => first(&mut self.flags_cleared),
            reg::CRCSCAN_CTRLA => first(&mut self.flash_checked),
            reg::CRCSCAN_CTRLB | reg::VPORTC_OUT | reg::VPORTC_DIR => {}
            _ => first(&mut self.set_up),
        }
        if a == reg::RTC_PITCTRLA && value & RTC_PITEN != 0 {
            first(&mut self.pit_on);
        }
        if a == reg::WDT_CTRLA {
            first(&mut self.watchdog_on);
        }
    }

    fn in_order(&self) -> Result<(), String> {
        let at = |when: Option<u64>, what: &str| when.ok_or_else(|| format!("never {what}"));
        let flags = at(self.flags_cleared, "cleared RSTCTRL.RSTFR")?;
        let flash = at(self.flash_checked, "started a CRC scan")?;
        let fuses = at(self.fuses_read, "read the fuses")?;
        let supply = at(self.supply_read, "read MVIO.STATUS")?;
        let set_up = at(self.set_up, "set anything up")?;
        let pit = at(self.pit_on, "turned the PIT on")?;
        let watchdog = at(self.watchdog_on, "turned the watchdog on")?;
        let rules = [
            (
                flags < watchdog,
                "turns the watchdog on before clearing the reset flags, so a bite could \
                 reset the chip before `main` has read why it last reset",
            ),
            (
                flash < set_up,
                "sets the chip up before the CRC scan, so code nobody's checked has \
                 already run",
            ),
            (
                fuses < set_up,
                "sets the chip up before reading the fuses, so it's run on fuses nobody's \
                 checked",
            ),
            (
                supply < set_up,
                "sets the chip up before checking VDDIO2, which PC1 hangs off",
            ),
            (
                pit < watchdog,
                "turns the watchdog on before the PIT, so its first timeout starts before \
                 there's a tick to feed it on",
            ),
        ];
        match rules.into_iter().find(|&(ok, _)| !ok) {
            Some((_, why)) => Err(why.into()),
            None => Ok(()),
        }
    }
}

struct CrcScan {
    started: Option<(u64, u8)>,
    busy_reads: u8,
    /// OK from reset, before any scan (§30.5.3).
    status: u8,
}

#[derive(Default)]
struct Adc {
    last_setup: Option<(u8, u8)>,
    result: u16,
    ready_at: Option<u64>,
    temp: Option<u8>,
}

#[derive(Default)]
struct Dac {
    code: u16,
    temp: Option<u8>,
    /// Meaningless while `code` isn't 0.
    zero_since: u64,
}

#[derive(Default)]
struct Tcb {
    ccmp: u16,
    cnt: u16,
    temp: Option<(usize, u8)>,
    capt: bool,
    capt_up_at: u64,
    next_capt: u64,
}

impl Tcb {
    /// Taken as CCMP + 1, which the datasheet doesn't state (§24.3.3.1.1).
    fn period(&self) -> u64 {
        u64::from(self.ccmp) + 1
    }
}

/// Bits of `Chip::interrupted`.
const TEMP_TCB0: u8 = 1 << 0;
const TEMP_DAC0: u8 = 1 << 1;
const TEMP_ADC0: u8 = 1 << 2;

struct Chip {
    flash: Vec<u8>,
    sram: Vec<u8>,
    io: Vec<u8>,
    /// Counts instructions while awake and jumps to the next event asleep.
    now: u64,
    next_event: u64,
    unlocked_until: Option<u64>,
    /// The key's first store to the bus or `sleep` ends it.
    ccp_hold: bool,
    pressed: bool,
    pit_busy: u8,
    pit_flag: bool,
    portd_flags: u8,
    next_pit: u64,
    /// Main clocks.
    tick_cycles: u64,
    /// Stops the PIT and the WDT (§26.4.1.1, §22.3.2). A sync left waiting
    /// on it isn't modelled.
    osc32k_stopped: bool,
    /// Only a reset wakes the chip (§13.3.1), and resets aren't modelled.
    down: bool,
    ticks: u32,
    slow_ticks: u32,
    /// Edges of the 64 Hz grid, whatever the PIT's period.
    grid: u32,
    pit_next: u32,
    /// The datasheet doesn't state where the first tick after a change of
    /// period lands (§26.5.2.2).
    pit_phase: PitPhase,
    switch_reads: u32,
    switch_read_at: u64,
    edge_with_slow_tick: bool,
    edges_with_ticks: u32,
    wdt_waits: u64,
    tick_polled: bool,
    tcb: Tcb,
    dac: Dac,
    wdt: Wdt,
    crc: CrcScan,
    adc: Adc,
    rstfr: u8,
    world: World,
    vddio2_lost_at: Option<u64>,
    lost_lit: bool,
    tick_taken_at: u64,
    late_vddio2: bool,
    vddio2_going: bool,
    enabled: u64,
    forced: Option<u8>,
    in_handler: bool,
    interrupted: u8,
    pc1_rose_at: u64,
    vref_changed_at: u64,
    power_downs: u32,
    register_writes: u32,
    boot: Boot,
    pc1_rises: u32,
    twi: Twi,
    aux_unwired: bool,
}

impl Chip {
    fn new(flash: Vec<u8>) -> Self {
        let mut io = vec![0; 0x1100];
        io[reg::CLKCTRL_OSCHFCTRLA] = OSCHF_4MHZ;
        Self {
            flash,
            sram: vec![JUNK; usize::from(SRAM.end - SRAM.start)],
            io,
            now: 0,
            next_event: TICK_CYCLES,
            unlocked_until: None,
            ccp_hold: false,
            pressed: false,
            pit_busy: PIT_SYNC_READS,
            pit_flag: false,
            portd_flags: 0,
            next_pit: TICK_CYCLES,
            tick_cycles: TICK_CYCLES,
            osc32k_stopped: false,
            down: false,
            ticks: 0,
            slow_ticks: 0,
            grid: 0,
            pit_next: 0,
            pit_phase: PitPhase::Aligned,
            switch_reads: 0,
            switch_read_at: 0,
            edge_with_slow_tick: false,
            edges_with_ticks: 0,
            wdt_waits: 0,
            tick_polled: false,
            tcb: Tcb::default(),
            dac: Dac::default(),
            wdt: Wdt::default(),
            crc: CrcScan {
                started: None,
                busy_reads: 0,
                status: CRCSCAN_OK,
            },
            adc: Adc::default(),
            rstfr: RSTCTRL_PORF,
            world: World::NORMAL,
            vddio2_lost_at: None,
            lost_lit: false,
            tick_taken_at: 0,
            late_vddio2: false,
            vddio2_going: false,
            enabled: 0,
            forced: None,
            in_handler: false,
            interrupted: 0,
            pc1_rose_at: 0,
            vref_changed_at: 0,
            power_downs: 0,
            register_writes: 0,
            boot: Boot::default(),
            pc1_rises: 0,
            twi: Twi::new(),
            aux_unwired: false,
        }
    }

    /// One-shot, and wakes nothing, where a real stray interrupt stays
    /// enabled and flagged. Whether one blocked by LVL0EX still wakes
    /// `hw::halt`'s `sleep` isn't in the datasheet.
    const fn force_interrupt(&mut self, vector: u8) {
        self.forced = Some(vector);
    }

    fn sram(&self, range: &Range<u16>) -> &[u8] {
        &self.sram[usize::from(range.start - SRAM.start)..usize::from(range.end - SRAM.start)]
    }

    fn enabled_high(&self) -> bool {
        self.io[reg::VPORTC_OUT] & self.io[reg::VPORTC_DIR] & ENABLE != 0
    }

    fn paths(&self) -> u8 {
        self.io[reg::VPORTA_OUT] & self.io[reg::VPORTA_DIR] & PATHS
    }

    fn dac_reference(&self) -> u8 {
        self.io[reg::VREF_DAC0REF]
    }

    /// Reads 1 with single supply (§19.3.1).
    fn vddio2_ok(&self) -> bool {
        self.world.vddio2s()
    }

    fn set_world(&mut self, world: World) -> Result<(), String> {
        let was = self.vddio2_ok();
        self.world = world;
        self.vddio2_moved(was)
    }

    fn set_vddio2(&mut self, there: bool) -> Result<(), String> {
        let was = self.vddio2_ok();
        self.world.scene.vddio2 = there;
        self.vddio2_moved(was)
    }

    fn enter(&mut self, mut world: World) -> Result<(), String> {
        if self.late_vddio2 && self.world.scene.vddio2 && !world.scene.vddio2 {
            world.scene.vddio2 = true;
            self.vddio2_going = true;
        }
        self.set_world(world)
    }

    /// A firmware that looked more than once a pass would see this sooner
    /// than `in_step` tells the library.
    fn vddio2_goes(&mut self) -> Result<(), String> {
        if !self.vddio2_going {
            return Ok(());
        }
        self.vddio2_going = false;
        self.set_vddio2(false)
    }

    /// §19.3.2.1. The datasheet gives no delay either way, so both are taken
    /// as at once. What the driver's enable does while PC1 is tri-stated is
    /// unknown.
    fn vddio2_moved(&mut self, was: bool) -> Result<(), String> {
        let ok = self.vddio2_ok();
        if was && !ok {
            self.vddio2_lost_at = Some(self.now);
            self.lost_lit = self.enabled_high() && self.dac.code != 0;
        }
        if ok && !was {
            self.vddio2_lost_at = None;
            if self.enabled_high() {
                self.pc1_moves(false).map_err(|e| {
                    format!(
                        "VDDIO2 comes back with PC1 still set in PORTC, and the pin takes that \
                         up again (section 19.3.2.1), which {e}"
                    )
                })?;
            }
        }
        Ok(())
    }

    /// The firmware's rule, not the datasheet's.
    fn dark_without_vddio2(&self) -> Result<(), String> {
        let Some(lost) = self.vddio2_lost_at else {
            return Ok(());
        };
        if lost > self.tick_taken_at || self.shows(Output::OFF).is_ok() {
            return Ok(());
        }
        Err(format!(
            "sleeps with PC1 {} in PORTC, DAC0 at {}, VREF.DAC0REF {:#04x} and the paths \
             {:#04x}, at the end of a pass that began with MVIO.STATUS.VDDIO2S clear: PC1 is \
             tri-stated without VDDIO2 (section 19.3.2.1), so the light goes off in that pass, \
             DAC0 to 0 and the paths low",
            if self.enabled_high() { "set" } else { "clear" },
            self.dac.code,
            self.dac_reference(),
            self.paths()
        ))
    }

    /// Writes of anything else are refused already, so this is what catches
    /// a write that never happened.
    fn clocked_as_modelled(&self) -> Result<(), String> {
        if self.io[reg::CLKCTRL_OSCHFCTRLA] != OSCHF_4MHZ || self.io[reg::CLKCTRL_MCLKCTRLB] != 0 {
            return Err("isn't on the internal oscillator at 4 MHz, undivided".into());
        }
        if self.io[reg::TCB0_CTRLB] != regs::TCB_CNTMODE_INT {
            return Err("left TCB0 out of periodic interrupt mode".into());
        }
        if self.tcb.period() != TIMER_PERIOD {
            return Err(format!(
                "TCB0.CCMP is {} after the first tick, so its period isn't the {TIMER_PERIOD} \
                 clocks the tick deadline counts",
                self.tcb.ccmp
            ));
        }
        if self.io[reg::RTC_PITCTRLA] != PIT_FAST {
            return Err("the PIT isn't on at 512 cycles, 64 Hz".into());
        }
        Ok(())
    }

    fn tcb_running(&self) -> bool {
        self.io[reg::TCB0_CTRLA] & TCB_ENABLE != 0
    }

    fn powering_down(&self) -> bool {
        self.io[reg::SLPCTRL_CTRLA] == SLEEP_POWER_DOWN | SLPCTRL_SEN
    }

    fn take_protected_write(&mut self, a: usize) -> Result<(), String> {
        if self.unlocked_until.is_none_or(|until| self.now > until) {
            return Err(format!(
                "writes {} without the CCP key in the four instructions before, \
                 and the chip ignores that write",
                name(a)
            ));
        }
        self.unlocked_until = None;
        Ok(())
    }

    /// The one place simulated time moves.
    fn advance_to(&mut self, to: u64) -> Result<(), String> {
        self.now = to;
        if self.now < self.next_event {
            return Ok(());
        }
        if self.tcb_running() {
            while self.now >= self.tcb.next_capt {
                if !self.tcb.capt {
                    self.tcb.capt_up_at = self.tcb.next_capt;
                }
                self.tcb.capt = true;
                self.tcb.next_capt += self.tcb.period();
            }
        }
        while self.now >= self.next_pit {
            self.next_pit += self.tick_cycles;
            if self.osc32k_stopped {
                // The WDT's count stops with its clock.
                self.wdt.since += self.tick_cycles;
                continue;
            }
            self.grid += 1;
            if self.io[reg::RTC_PITCTRLA] & RTC_PITEN != 0 && self.grid >= self.pit_next {
                self.pit_flag = true;
                self.ticks += 1;
                self.pit_next = match self.pit() {
                    Pit::Fast => self.grid + 1,
                    Pit::Slow => {
                        self.slow_ticks += 1;
                        if self.edge_with_slow_tick && senses(self.io[SWITCH_CTRL] & PORT_ISC) {
                            self.portd_flags |= SWITCH;
                            self.edges_with_ticks += 1;
                        }
                        testing::next_slow(self.grid)
                    }
                };
            }
            self.wdt_check()?;
        }
        if let Some((value, at)) = self.wdt.pending
            && self.now >= at
        {
            self.wdt_check()?;
            self.io[reg::WDT_CTRLA] = value;
            self.wdt.pending = None;
            self.wdt.polling = false;
            self.wdt.windowed = false;
            self.wdt_check()?;
        }
        self.plan_next_event();
        Ok(())
    }

    fn pit(&self) -> Pit {
        if self.io[reg::RTC_PITCTRLA] & regs::RTC_PERIOD == regs::RTC_PERIOD_CYC32768 {
            Pit::Slow
        } else {
            Pit::Fast
        }
    }

    const fn wdt_time(&self, clocks: u64) -> u64 {
        clocks * self.tick_cycles / WDT_CLOCKS_PER_TICK as u64
    }

    fn wdt_open(&self) -> u32 {
        wdt_clocks(self.io[reg::WDT_CTRLA] & WDT_PERIOD)
    }

    /// §22.3.3.2
    fn wdt_closed(&self) -> u32 {
        if self.wdt.windowed {
            wdt_clocks((self.io[reg::WDT_CTRLA] & WDT_WINDOW) >> 4)
        } else {
            0
        }
    }

    fn wdt_on(&self) -> bool {
        self.wdt_open() > 0 || self.wdt.pending.is_some()
    }

    /// While a write syncs, which setting counts isn't stated, so the shorter.
    fn wdt_timeout(&self) -> u32 {
        let now = self.wdt_closed() + self.wdt_open();
        match self.wdt.pending {
            Some((value, _)) => {
                let coming = wdt_clocks(value & WDT_PERIOD);
                if now == 0 { coming } else { now.min(coming) }
            }
            None => now,
        }
    }

    const fn wdt_elapsed(&self) -> u64 {
        (self.now - self.wdt.since) * WDT_CLOCKS_PER_TICK as u64 / self.tick_cycles
    }

    /// A clock early, for a `wdr` that synced fast.
    fn wdt_bite_at(&self) -> Option<u64> {
        let timeout = self.wdt_timeout();
        (timeout > 0)
            .then(|| self.wdt.since + self.wdt_time(u64::from(timeout.saturating_sub(WDR_SPREAD))))
    }

    fn wdt_periods_left(&self) -> u64 {
        match self.wdt_bite_at() {
            None => DOWN_PERIODS,
            Some(bite) if bite <= self.next_pit => 0,
            Some(bite) => ((bite - 1 - self.next_pit) / self.tick_cycles + 1).min(DOWN_PERIODS),
        }
    }

    /// The watchdog runs in every sleep mode (§22.3.4).
    fn wdt_check(&self) -> Result<(), String> {
        if self.osc32k_stopped {
            return Ok(());
        }
        match self.wdt_bite_at() {
            Some(bite) if self.now >= bite => Err(format!(
                "the watchdog went {} WDT clocks without wdr, against a {}-clock timeout, so \
                 the chip would have reset",
                self.wdt_elapsed(),
                self.wdt_timeout()
            )),
            _ => Ok(()),
        }
    }

    const fn crc_status(&mut self) -> u8 {
        match self.crc.started {
            Some((at, before)) if self.now <= at + CRC_START_INSTRUCTIONS => before,
            Some(_) if self.crc.busy_reads > 0 => {
                self.crc.busy_reads -= 1;
                CRCSCAN_BUSY
            }
            _ => self.crc.status,
        }
    }

    /// §30.3.2
    fn start_crc(&mut self) -> Result<(), String> {
        let ctrlb = self.io[reg::CRCSCAN_CTRLB];
        if ctrlb != CRCSCAN_SRC_FLASH {
            return Err(format!(
                "starts a CRC scan with CRCSCAN.CTRLB {ctrlb:#04x}; only the whole-flash \
                 scan puts the checksum where `xtask` does (table 30-1)"
            ));
        }
        let before = self.crc_status();
        let flash = self.flash.iter().copied().chain(iter::repeat(0xFF));
        let sum = crc::crc16(flash.take(FLASH_SIZE));
        self.crc = CrcScan {
            started: Some((self.now, before)),
            busy_reads: CRC_BUSY_READS,
            status: if sum == 0 { CRCSCAN_OK } else { 0 },
        };
        Ok(())
    }

    fn converting(&self) -> bool {
        self.adc.ready_at.is_some_and(|at| self.now < at)
    }

    fn start_conversion(&mut self) -> Result<(), String> {
        if self.converting() {
            return Err("starts a conversion while one's still running".into());
        }
        let io = |r: usize| self.io[r];
        if io(reg::ADC0_CTRLA) != ADC_ON {
            return Err(format!(
                "starts a conversion with ADC0.CTRLA {:#04x}; it wants the ADC enabled, \
                 single-ended, right-adjusted and 12-bit, one conversion a trigger",
                io(reg::ADC0_CTRLA)
            ));
        }
        if io(reg::ADC0_CTRLB) != ADC_SAMPNUM_ACC4 & ADC_SAMPNUM {
            return Err("converts without four samples accumulated".into());
        }
        let divide = adc_prescaler(io(reg::ADC0_CTRLC) & ADC_PRESC).ok_or_else(|| {
            format!(
                "converts with ADC0.CTRLC {:#04x}, which isn't a prescaler the simulator \
                 models",
                io(reg::ADC0_CTRLC)
            )
        })?;
        let clk_adc = CLK_PER / divide;
        if !(125_000..=2_000_000).contains(&clk_adc) {
            return Err(format!(
                "runs the ADC at {clk_adc} Hz, outside 125 kHz to 2 MHz (table 38-25)"
            ));
        }
        let initdly = adc_initdly(io(reg::ADC0_CTRLD) & ADC_INITDLY).ok_or("a reserved INITDLY")?;
        let sampdly = u32::from(io(reg::ADC0_CTRLD) & ADC_SAMPDLY);
        let samplen = u32::from(io(reg::ADC0_SAMPCTRL) & ADC_SAMPLEN);
        let reference = io(reg::VREF_ADC0REF);
        let muxpos = io(reg::ADC0_MUXPOS);
        let channel = match (muxpos, reference) {
            (ADC_MUXPOS_VDDDIV10, VREF_REFSEL_1V024) => Channel::Voltage,
            // §33.5.7 says no more, so the worst offset table 38-24 allows.
            (ADC_MUXPOS_GND, VREF_REFSEL_1V024) => Channel::Ground,
            (ADC_MUXPOS_TEMPSENSE, VREF_REFSEL_2V048) => {
                let need = |us: u32| (clk_adc * us).div_ceil(1_000_000);
                if initdly < need(25) || samplen < need(28) {
                    return Err(format!(
                        "reads the temperature sensor with INITDLY {initdly} and SAMPLEN \
                         {samplen}; it needs at least {} and {} ADC clocks (section 33.3.3.8)",
                        need(25),
                        need(28)
                    ));
                }
                Channel::Temperature
            }
            _ => {
                return Err(format!(
                    "converts MUXPOS {muxpos:#04x} against VREF.ADC0REF {reference:#04x}; the \
                     simulator only models VDD / 10 and ground against 1.024 V \
                     and the temperature sensor against 2.048 V"
                ));
            }
        };
        let setup = Some((muxpos, reference));
        self.adc.result = if self.adc.last_setup == setup {
            self.world.reading(channel)
        } else {
            UNSETTLED
        };
        self.adc.last_setup = setup;
        // §33.3.3.4.4. INITDLY is counted every time, so a result is never
        // early.
        let adc_clocks = initdly + 4 * (2 + sampdly + samplen) + 4 * 27 / 2;
        let clocks = 4 + u64::from(adc_clocks) * u64::from(divide);
        self.adc.ready_at = Some(self.now + clocks);
        Ok(())
    }

    fn plan_next_event(&mut self) {
        self.next_event = if self.tcb_running() {
            self.next_pit.min(self.tcb.next_capt)
        } else {
            self.next_pit
        };
        if let Some((_, at)) = self.wdt.pending {
            self.next_event = self.next_event.min(at);
        }
    }

    fn elapse(&mut self) -> Result<(), String> {
        self.advance_to(self.now + 1)
    }

    fn shows(&self, output: Output) -> Result<(), String> {
        let reference = match output.vref {
            Vref::V1024 => VREF_REFSEL_1V024,
            Vref::V2500 => VREF_REFSEL_2V500,
        };
        let want = (output.enable, output.dac, reference, output.paths.mask());
        let have = (
            self.enabled_high(),
            self.dac.code,
            self.dac_reference(),
            self.paths(),
        );
        if have != want {
            return Err(format!(
                "PC1 is {}, DAC0 {}, VREF.DAC0REF {:#04x} and the paths {:#04x}, but the \
                 library says {output:?}",
                if have.0 { "high" } else { "low" },
                have.1,
                have.2,
                have.3
            ));
        }
        Ok(())
    }

    fn dac_zero(&self, what: &str) -> Result<(), String> {
        if self.dac.code != 0 {
            return Err(format!(
                "{what} with PC1 high and DAC0 at {}, which for a moment asks for a current \
                 neither the old setting nor the new one does: DAC0 goes to 0 first",
                self.dac.code
            ));
        }
        Ok(())
    }

    fn dac_quiet(&self, what: &str) -> Result<(), String> {
        self.dac_zero(what)?;
        let since = self.now - self.dac.zero_since;
        if since < DAC_SETTLE_CYCLES {
            return Err(format!(
                "{what} with PC1 high {since} instructions after DAC0 went to 0, short of \
                 the {DAC_SETTLE_CYCLES} clocks its output takes to settle (table 38-23)"
            ));
        }
        Ok(())
    }

    /// §18.3.3
    fn set_switch(&mut self, pressed: bool) -> Result<(), String> {
        if pressed == self.pressed {
            return Ok(());
        }
        self.pressed = pressed;
        let sense = match self.io[SWITCH_CTRL] & PORT_ISC {
            ISC_INTDISABLE | ISC_INPUT_DISABLE => false,
            ISC_BOTHEDGES => true,
            ISC_RISING => !pressed,
            ISC_FALLING => pressed,
            isc => {
                return Err(format!(
                    "PD4's ISC is {isc:#x}, which the simulator doesn't model"
                ));
            }
        };
        if sense {
            self.portd_flags |= SWITCH;
        }
        Ok(())
    }

    fn interrupts(&self, tcb: bool) -> u64 {
        let port = |base: usize, vector: u8| {
            let on = (0..8).any(|pin| senses(self.io[base + pin] & PORT_ISC));
            u64::from(on) << vector
        };
        let pit = self.io[reg::RTC_PITINTCTRL] & RTC_PI != 0;
        let capt = tcb && self.io[reg::TCB0_INTCTRL] & TCB_CAPT != 0;
        port(reg::PORTA_PIN0CTRL, VECTOR_PORTA)
            | port(reg::PORTC_PIN0CTRL, VECTOR_PORTC)
            | port(reg::PORTD_PIN0CTRL, VECTOR_PORTD)
            | u64::from(pit) << VECTOR_RTC_PIT
            | u64::from(capt) << VECTOR_TCB0
    }

    fn update_enabled(&mut self) {
        self.enabled = self.interrupts(true);
    }

    const fn temps_in_use(&self) -> u8 {
        (if self.tcb.temp.is_some() {
            TEMP_TCB0
        } else {
            0
        }) | (if self.dac.temp.is_some() {
            TEMP_DAC0
        } else {
            0
        }) | (if self.adc.temp.is_some() {
            TEMP_ADC0
        } else {
            0
        })
    }

    fn ready_for_power_down(&self) -> Result<(), String> {
        if self.enabled_high() {
            return Err(
                "powers down with PC1 high, so the light's on with nothing awake to \
                        regulate it or turn it off"
                    .into(),
            );
        }
        if self.tcb_running() {
            return Err(
                "powers down with TCB0 running, which power-down halts (section 24.3.6)".into(),
            );
        }
        if self.converting() {
            return Err(
                "powers down mid-conversion, which resumes on waking with an invalid \
                 result (section 33.3.8)"
                    .into(),
            );
        }
        if self.io[reg::ADC0_CTRLA] & ADC_ENABLE != 0 {
            return Err("powers down with ADC0 still enabled".into());
        }
        if self.pit_busy > 0 {
            return Err(
                "powers down with RTC.PITSTATUS.CTRLBUSY still set, and the datasheet \
                 doesn't say the write finishes syncing with CLK_PER stopped"
                    .into(),
            );
        }
        if self.wdt.pending.is_some() {
            return Err(
                "powers down with WDT.STATUS.SYNCBUSY still set, and the datasheet doesn't \
                 say the write finishes syncing with CLK_PER stopped"
                    .into(),
            );
        }
        Ok(())
    }

    fn write_pin_ctrl(&mut self, a: usize, value: u8) -> Result<(), String> {
        let pull_up = value & PORT_PULLUPEN != 0;
        let pin = |base: usize, n: u8| base + usize::from(n);
        let no_pull_up = [
            (
                pin(reg::PORTC_PIN0CTRL, board::ENABLE),
                "PC1",
                "the driver's enable, which a pull-up could creep on",
            ),
            (SWITCH_CTRL, "PD4", "the switch, which has its own"),
            (DAC_PIN_CTRL, "PD6", "DAC0's output"),
            (
                HELD_LOW_CTRL,
                "PA1",
                "an output held low as the stock build had it, wired to who knows what",
            ),
            (
                pin(reg::PORTA_PIN0CTRL, board::PATH1),
                "PA7",
                "a power path",
            ),
            (
                pin(reg::PORTA_PIN0CTRL, board::PATH2),
                "PA6",
                "a power path",
            ),
            (
                pin(reg::PORTA_PIN0CTRL, board::PATH3),
                "PA5",
                "a power path",
            ),
        ];
        if pull_up && let Some((_, pin, what)) = no_pull_up.iter().find(|&&(at, ..)| at == a) {
            return Err(format!("gives {pin} a pull-up ({what})"));
        }
        if a == pin(reg::PORTA_PIN0CTRL, board::TWI_SDA)
            || a == pin(reg::PORTA_PIN0CTRL, board::TWI_SCL)
        {
            return Err(format!(
                "writes {value:#04x} to {}: PA2 and PA3 are the AW2016's TWI lines, which \
                 the firmware leaves alone",
                name(a)
            ));
        }
        if value & PORT_INVEN != 0 {
            return Err(format!(
                "sets INVEN in {}, which inverts the pin both ways (section 18.3.2.3) and \
                 the simulator doesn't model",
                name(a)
            ));
        }
        if a == DAC_PIN_CTRL
            && value & PORT_ISC != ISC_INPUT_DISABLE
            && self.io[reg::DAC0_CTRLA] & DAC_OUTEN != 0
        {
            return Err(
                "turns PD6's input buffer back on with DAC0 driving it (section 34.3.1)".into(),
            );
        }
        self.io[a] = value;
        self.update_enabled();
        Ok(())
    }

    fn write_porta(&mut self, a: usize, value: u8) -> Result<(), String> {
        if value & TWI != 0 {
            return Err(format!(
                "writes {value:#04x} to {}: PA2 and PA3 are the AW2016's TWI lines, which \
                 the firmware never drives",
                name(a)
            ));
        }
        if a == reg::VPORTA_OUT && value & HELD_LOW != 0 {
            return Err(format!(
                "writes {value:#04x} to VPORTA.OUT, setting PA1, which the stock build on \
                 this torch holds low and which goes to something unknown"
            ));
        }
        let (old, before) = (self.io[a], self.paths());
        self.io[a] = value;
        let after = self.paths();
        let checked = if !self.enabled_high() || after == before {
            Ok(())
        } else if after & !before != 0 {
            self.dac_quiet(&format!("turns power paths {:#04x} on", after & !before))
        } else {
            self.dac_zero(&format!("turns power paths {:#04x} off", before & !after))
        };
        if checked.is_err() {
            // A refused write changes nothing, so a test can carry on.
            self.io[a] = old;
        }
        checked
    }

    fn write_portc(&mut self, a: usize, value: u8) -> Result<(), String> {
        if value & !ENABLE != 0 {
            return Err(format!(
                "writes {value:#04x} to {}: only PC1 is the firmware's to drive",
                name(a)
            ));
        }
        let (old, was) = (self.io[a], self.enabled_high());
        self.io[a] = value;
        let moved = self.pc1_moves(was);
        if moved.is_err() {
            self.io[a] = old;
        }
        moved
    }

    /// `was` is whether PC1 was high before the write.
    fn pc1_moves(&mut self, was: bool) -> Result<(), String> {
        let high = self.enabled_high();
        if high && !was {
            if self.io[reg::DAC0_CTRLA] != DAC_DRIVING {
                return Err(
                    "raises PC1 without DAC0 on and driving PD6, so the amplifier's \
                     setpoint is whatever the pin floats to"
                        .into(),
                );
            }
            if self.dac.code != 0 {
                return Err(format!(
                    "raises PC1 with DAC0 at {}: it's at 0 whenever PC1 rises",
                    self.dac.code
                ));
            }
            if self.dac_reference() != VREF_REFSEL_1V024 {
                return Err(format!(
                    "raises PC1 with VREF.DAC0REF at {:#04x}: it's at 1.024 V whenever PC1 rises",
                    self.dac_reference()
                ));
            }
            if self.paths() != 0 {
                return Err(format!(
                    "raises PC1 with power paths {:#04x} on: they're low whenever PC1 rises",
                    self.paths()
                ));
            }
            if self.wdt_on()
                && (self.io[reg::WDT_CTRLA] != WDT_LIT
                    || self.wdt.pending.is_some()
                    || !self.wdt.windowed)
            {
                return Err(format!(
                    "raises PC1 with WDT.CTRLA at {:#04x}{}: the light only comes on with the \
                     lit window in force and a wdr since it was written",
                    self.io[reg::WDT_CTRLA],
                    if self.wdt.pending.is_some() {
                        " and a write still syncing"
                    } else {
                        ""
                    }
                ));
            }
            if self.pit() == Pit::Slow {
                return Err(
                    "raises PC1 with the PIT slow, so the light's protection would run once \
                     a second"
                        .into(),
                );
            }
            self.twi.catch_up(self.now, false)?;
            if self.twi.aw2016.lit() {
                return Err(
                    "raises PC1 with the aux lit: they go out before the main LED comes on".into(),
                );
            }
            self.pc1_rose_at = self.now;
            self.pc1_rises += 1;
        }
        if was && !high {
            if self.dac.code != 0 {
                return Err(format!(
                    "drops PC1 with DAC0 still at {}: the off order is DAC0 to 0, its \
                     reference to 1.024 V, then PC1",
                    self.dac.code
                ));
            }
            if self.dac_reference() != VREF_REFSEL_1V024 {
                return Err(
                    "drops PC1 with VREF.DAC0REF still above 1.024 V: the off order is DAC0 \
                     to 0, its reference to 1.024 V, then PC1"
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn write_portd(&mut self, a: usize, value: u8) -> Result<(), String> {
        let refused = match a {
            reg::VPORTD_DIR => value & !DAC_PIN != 0,
            _ => value != 0,
        };
        if refused {
            return Err(format!(
                "writes {value:#04x} to {}: only PD6 is an output, held low under DAC0, and \
                 PD4's the switch",
                name(a)
            ));
        }
        self.io[a] = value;
        Ok(())
    }

    fn write_dac_reference(&mut self, value: u8) -> Result<(), String> {
        if value != VREF_REFSEL_1V024 && value != VREF_REFSEL_2V500 {
            return Err(only_models(
                reg::VREF_DAC0REF,
                value,
                "1.024 V and 2.5 V, with ALWAYSON clear",
            ));
        }
        if value != self.dac_reference() {
            if self.enabled_high() && value == VREF_REFSEL_2V500 {
                self.dac_quiet("raises VREF.DAC0REF to 2.5 V")?;
            } else if self.enabled_high() {
                self.dac_zero("lowers VREF.DAC0REF to 1.024 V")?;
            }
            self.vref_changed_at = self.now;
        }
        self.io[reg::VREF_DAC0REF] = value;
        Ok(())
    }

    fn set_dac(&mut self, code: u16) -> Result<(), String> {
        if code != 0 {
            if !self.enabled_high() {
                return Err(format!(
                    "sets DAC0 to {code} with PC1 low: it's only ever above 0 with the light on"
                ));
            }
            let since = self.now - self.pc1_rose_at;
            if since < BOOST_START_CYCLES {
                return Err(format!(
                    "sets DAC0 to {code} {since} instructions after PC1 rose, inside the \
                     {BOOST_START_CYCLES} clocks (8 ms) the stock firmware gives the boost \
                     converter"
                ));
            }
            let since = self.now - self.vref_changed_at;
            if since < VREF_SETTLE_CYCLES {
                return Err(format!(
                    "sets DAC0 to {code} {since} instructions after its reference changed, \
                     inside the {VREF_SETTLE_CYCLES} clocks table 38-18 gives a change"
                ));
            }
        }
        if code == 0 && self.dac.code != 0 {
            self.dac.zero_since = self.now;
        }
        self.dac.code = code;
        Ok(())
    }

    fn write_tcb_ctrla(&mut self, value: u8) -> Result<(), String> {
        let running = self.tcb_running();
        match value {
            0 if running => {
                // Where it stopped, for a start that doesn't write CNT.
                let left = self.tcb.next_capt.saturating_sub(self.now);
                self.tcb.cnt = (self.tcb.period().saturating_sub(left)) as u16;
            }
            TCB_RUNNING if !running => {
                if self.tcb.cnt > self.tcb.ccmp {
                    return Err("starts TCB0 with CNT over CCMP, so it counts to MAX first \
                         (section 24.3.2), which the simulator doesn't model"
                        .into());
                }
                self.tcb.next_capt = self.now + u64::from(self.tcb.ccmp - self.tcb.cnt) + 1;
            }
            0 | TCB_RUNNING => {}
            _ => {
                return Err(only_models(
                    reg::TCB0_CTRLA,
                    value,
                    "it off, or on and undivided",
                ));
            }
        }
        self.io[reg::TCB0_CTRLA] = value;
        self.plan_next_event();
        Ok(())
    }

    fn tcb_written(&mut self, low: usize, value: u16) -> Result<(), String> {
        match low {
            reg::TCB0_CCMP if self.tcb_running() => Err(
                "writes TCB0.CCMP with TCB0 running, which the simulator doesn't model \
                 (section 24.3.2)"
                    .into(),
            ),
            reg::TCB0_CCMP => {
                self.tcb.ccmp = value;
                Ok(())
            }
            _ => {
                if self.tcb_running() {
                    if value > self.tcb.ccmp {
                        return Err("writes TCB0.CNT over CCMP with TCB0 running, which the \
                             simulator doesn't model"
                            .into());
                    }
                    self.tcb.next_capt = self.now + u64::from(self.tcb.ccmp - value) + 1;
                    self.plan_next_event();
                }
                self.tcb.cnt = value;
                Ok(())
            }
        }
    }

    fn temp_undisturbed(&self, a: usize) -> Result<(), String> {
        let wide = [
            (TEMP_TCB0, reg::TCB0_CNT..reg::TCB0_CCMP + 2, "TCB0"),
            (TEMP_DAC0, reg::DAC0_DATA..reg::DAC0_DATA + 2, "DAC0"),
            (TEMP_ADC0, reg::ADC0_RES..reg::ADC0_RES + 2, "ADC0"),
        ];
        for (bit, range, peripheral) in wide {
            if self.interrupted & bit != 0 && range.contains(&a) {
                return Err(format!(
                    "a handler touches {} with the main loop halfway through one of \
                     {peripheral}'s 16-bit registers, and they share its TEMP byte, so one \
                     corrupts the other (section 8.10.3)",
                    name(a & !1)
                ));
            }
        }
        Ok(())
    }
}

/// CLK_PER's divisor for ADC0.CTRLC.PRESC (§33.5.3).
const fn adc_prescaler(presc: u8) -> Option<u32> {
    Some(match presc {
        regs::ADC_PRESC_DIV2 => 2,
        regs::ADC_PRESC_DIV4 => 4,
        regs::ADC_PRESC_DIV8 => 8,
        regs::ADC_PRESC_DIV12 => 12,
        regs::ADC_PRESC_DIV16 => 16,
        regs::ADC_PRESC_DIV20 => 20,
        regs::ADC_PRESC_DIV24 => 24,
        regs::ADC_PRESC_DIV28 => 28,
        regs::ADC_PRESC_DIV32 => 32,
        regs::ADC_PRESC_DIV48 => 48,
        regs::ADC_PRESC_DIV64 => 64,
        regs::ADC_PRESC_DIV96 => 96,
        regs::ADC_PRESC_DIV128 => 128,
        regs::ADC_PRESC_DIV256 => 256,
        _ => return None,
    })
}

/// ADC clocks for ADC0.CTRLD.INITDLY (§33.5.4), `None` for a reserved value.
const fn adc_initdly(initdly: u8) -> Option<u32> {
    Some(match initdly {
        regs::ADC_INITDLY_DLY0 => 0,
        regs::ADC_INITDLY_DLY16 => 16,
        regs::ADC_INITDLY_DLY32 => 32,
        regs::ADC_INITDLY_DLY64 => 64,
        regs::ADC_INITDLY_DLY128 => 128,
        regs::ADC_INITDLY_DLY256 => 256,
        _ => return None,
    })
}

fn plain(addr: usize) -> bool {
    (reg::GPR_GPR0..=reg::GPR_GPR3).contains(&addr)
        || matches!(
            addr,
            reg::SLPCTRL_CTRLA
                | reg::CRCSCAN_CTRLB
                | reg::ADC0_CTRLB
                | reg::ADC0_CTRLC
                | reg::ADC0_CTRLD
                | reg::ADC0_SAMPCTRL
                | reg::ADC0_MUXPOS
        )
}

fn twi(addr: usize) -> bool {
    (reg::TWI0_CTRLA..reg::TWI0_CTRLA + 0x10).contains(&addr)
}

fn pin_ctrl(addr: usize) -> bool {
    [
        reg::PORTA_PIN0CTRL,
        reg::PORTC_PIN0CTRL,
        reg::PORTD_PIN0CTRL,
    ]
    .iter()
    .any(|&base| (base..base + 8).contains(&addr))
}

fn portf(addr: usize) -> bool {
    (reg::PORTF_DIR..=reg::PORTF_PIN7CTRL).contains(&addr)
        || (reg::VPORTF_DIR..=reg::VPORTF_INTFLAGS).contains(&addr)
}

fn vector_name(vector: u8) -> String {
    match vector {
        VECTOR_RTC_PIT => format!("RTC_PIT (vector {vector})"),
        VECTOR_TCB0 => format!("TCB0_INT (vector {vector})"),
        VECTOR_PORTA => format!("PORTA_PORT (vector {vector})"),
        VECTOR_PORTC => format!("PORTC_PORT (vector {vector})"),
        VECTOR_PORTD => format!("PORTD_PORT (vector {vector})"),
        VECTOR_UNHANDLED => format!("TCB1_INT (vector {vector})"),
        v => format!("vector {v}"),
    }
}

/// Changing one mid-conversion gives unpredictable results (§33.3.2).
const ADC_SETUP: [usize; 7] = [
    reg::VREF_ADC0REF,
    reg::ADC0_CTRLA,
    reg::ADC0_CTRLB,
    reg::ADC0_CTRLC,
    reg::ADC0_CTRLD,
    reg::ADC0_SAMPCTRL,
    reg::ADC0_MUXPOS,
];

fn fuse_or_lock(addr: usize) -> bool {
    let fuses = usize::from(map::FUSES);
    let lock = usize::from(map::LOCK);
    (fuses..fuses + map::FUSES_SIZE).contains(&addr)
        || (lock..lock + map::LOCK_SIZE).contains(&addr)
}

fn name(addr: usize) -> String {
    if let Some(fuse) = addr
        .checked_sub(usize::from(map::FUSES))
        .and_then(|offset| FUSE_NAMES.get(offset))
        .filter(|name| !name.is_empty())
    {
        return format!("FUSE.{fuse}");
    }
    let pins = [
        (reg::PORTA_PIN0CTRL, "PORTA"),
        (reg::PORTC_PIN0CTRL, "PORTC"),
        (reg::PORTD_PIN0CTRL, "PORTD"),
    ];
    if let Some((base, port)) = pins
        .iter()
        .find(|&&(base, _)| (base..base + 8).contains(&addr))
    {
        return format!("{port}.PIN{}CTRL", addr - base);
    }
    let known = [
        (reg::RSTCTRL_RSTFR, "RSTCTRL.RSTFR"),
        (reg::WDT_CTRLA, "WDT.CTRLA"),
        (reg::WDT_STATUS, "WDT.STATUS"),
        (reg::VREF_ADC0REF, "VREF.ADC0REF"),
        (reg::VREF_DAC0REF, "VREF.DAC0REF"),
        (reg::CRCSCAN_CTRLA, "CRCSCAN.CTRLA"),
        (reg::CRCSCAN_CTRLB, "CRCSCAN.CTRLB"),
        (reg::CRCSCAN_STATUS, "CRCSCAN.STATUS"),
        (reg::ADC0_CTRLA, "ADC0.CTRLA"),
        (reg::ADC0_CTRLB, "ADC0.CTRLB"),
        (reg::ADC0_CTRLC, "ADC0.CTRLC"),
        (reg::ADC0_CTRLD, "ADC0.CTRLD"),
        (reg::ADC0_SAMPCTRL, "ADC0.SAMPCTRL"),
        (reg::ADC0_MUXPOS, "ADC0.MUXPOS"),
        (reg::ADC0_COMMAND, "ADC0.COMMAND"),
        (reg::ADC0_INTFLAGS, "ADC0.INTFLAGS"),
        (reg::ADC0_RES, "ADC0.RESL"),
        (reg::ADC0_RES + 1, "ADC0.RESH"),
        (reg::DAC0_CTRLA, "DAC0.CTRLA"),
        (reg::DAC0_DATA, "DAC0.DATAL"),
        (reg::DAC0_DATA + 1, "DAC0.DATAH"),
        (reg::TCB0_CTRLA, "TCB0.CTRLA"),
        (reg::TCB0_CTRLB, "TCB0.CTRLB"),
        (reg::TCB0_INTCTRL, "TCB0.INTCTRL"),
        (reg::TCB0_INTFLAGS, "TCB0.INTFLAGS"),
        (reg::TCB0_CNT, "TCB0.CNT"),
        (reg::TCB0_CCMP, "TCB0.CCMP"),
        (reg::SIGROW_TEMPSENSE0, "SIGROW.TEMPSENSE0"),
        (reg::SIGROW_TEMPSENSE1, "SIGROW.TEMPSENSE1"),
        (reg::CPU_CCP, "CPU.CCP"),
        (reg::GPR_GPR0, "GPR.GPR0"),
        (reg::GPR_GPR1, "GPR.GPR1"),
        (reg::VPORTA_DIR, "VPORTA.DIR"),
        (reg::VPORTA_OUT, "VPORTA.OUT"),
        (reg::VPORTA_IN, "VPORTA.IN"),
        (reg::VPORTC_DIR, "VPORTC.DIR"),
        (reg::VPORTC_OUT, "VPORTC.OUT"),
        (reg::VPORTD_DIR, "VPORTD.DIR"),
        (reg::VPORTD_OUT, "VPORTD.OUT"),
        (reg::VPORTD_IN, "VPORTD.IN"),
        (reg::VPORTD_INTFLAGS, "VPORTD.INTFLAGS"),
        (reg::MVIO_STATUS, "MVIO.STATUS"),
        (reg::SLPCTRL_CTRLA, "SLPCTRL.CTRLA"),
        (reg::CLKCTRL_OSCHFCTRLA, "CLKCTRL.OSCHFCTRLA"),
        (reg::CLKCTRL_MCLKCTRLB, "CLKCTRL.MCLKCTRLB"),
        (reg::RTC_PITCTRLA, "RTC.PITCTRLA"),
        (reg::RTC_PITSTATUS, "RTC.PITSTATUS"),
        (reg::RTC_PITINTCTRL, "RTC.PITINTCTRL"),
        (reg::RTC_PITINTFLAGS, "RTC.PITINTFLAGS"),
        (reg::TWI0_CTRLA, "TWI0.CTRLA"),
        (reg::TWI0_MCTRLA, "TWI0.MCTRLA"),
        (reg::TWI0_MCTRLB, "TWI0.MCTRLB"),
        (reg::TWI0_MSTATUS, "TWI0.MSTATUS"),
        (reg::TWI0_MBAUD, "TWI0.MBAUD"),
        (reg::TWI0_MADDR, "TWI0.MADDR"),
        (reg::TWI0_MDATA, "TWI0.MDATA"),
    ];
    known
        .iter()
        .find(|&&(a, _)| a == addr)
        .map_or_else(|| format!("{addr:#06x}"), |(_, n)| (*n).to_string())
}

fn only_models(a: usize, value: u8, what: &str) -> String {
    format!(
        "writes {value:#04x} to {}; the simulator only models {what}",
        name(a)
    )
}

impl Bus for Chip {
    fn fetch(&self, pc: u32) -> Option<u16> {
        word(&self.flash, pc as usize * 2)
    }

    fn wdr(&mut self) -> Result<(), String> {
        if self.wdt.pending.is_some() {
            return Err("wdr before the WDT.CTRLA write has synced".into());
        }
        if self.wdt_open() == 0 {
            return Ok(());
        }
        self.wdt_check()?;
        let closed = self.wdt_closed();
        let edge = self.wdt.since + self.wdt_time(u64::from(closed + WDR_SPREAD));
        if closed > 0 && self.now <= edge {
            return Err(format!(
                "wdr {} WDT clocks after the last, in or within a clock of the {closed}-clock \
                 closed window, so the chip would have reset",
                self.wdt_elapsed()
            ));
        }
        self.wdt.since = self.now;
        self.wdt.fed_at = Some(self.now);
        self.wdt.feeds += 1;
        self.wdt.windowed = self.io[reg::WDT_CTRLA] & WDT_WINDOW != 0;
        Ok(())
    }

    /// Lowest vector first, the default static priority (§15.3.2.4.3).
    fn interrupt(&self) -> Option<u8> {
        let sensing = (0..8).fold(0u8, |bits, pin| {
            let on = senses(self.io[reg::PORTD_PIN0CTRL + pin] & PORT_ISC);
            bits | u8::from(on) << pin
        });
        let pending = u64::from(self.portd_flags & sensing != 0) << VECTOR_PORTD
            | u64::from(self.pit_flag) << VECTOR_RTC_PIT
            | u64::from(self.tcb.capt) << VECTOR_TCB0;
        let forced = self.forced.map_or(0, |vector| 1 << vector);
        let requests = pending & self.enabled | forced;
        (requests != 0).then(|| requests.trailing_zeros() as u8)
    }

    /// §7.6.1. That the store or `sleep` ending the protected change ends
    /// this too is likely, not stated.
    fn holds_interrupts(&self) -> bool {
        self.ccp_hold && self.unlocked_until.is_some_and(|until| self.now <= until)
    }

    /// With no interrupt enabled, only a reset wakes it (§13.3.1).
    fn sleep(&mut self) -> Result<bool, String> {
        self.dark_without_vddio2()?;
        self.vddio2_goes()?;
        self.ccp_hold = false;
        self.tick_polled = true;
        let ctrla = self.io[reg::SLPCTRL_CTRLA];
        if ctrla & SLPCTRL_SEN == 0 {
            return Err(
                "sleeps with SLPCTRL.SEN clear, which the chip treats as a nop, so the \
                 firmware would spin instead"
                    .into(),
            );
        }
        if self.powering_down() {
            self.ready_for_power_down()?;
        }
        if self.twi.on() {
            return Err(
                "sleeps with the TWI host on, which stops in every sleep mode (section 29.3.5), \
                 so a transfer or a STOP could be left with SCL low"
                    .into(),
            );
        }
        if self.tcb_running() != self.enabled_high()
            || self.enabled_high() && self.io[reg::TCB0_INTCTRL] & TCB_CAPT == 0
        {
            return Err(format!(
                "sleeps with TCB0 {} and PC1 {}: the tick deadline's timer and its interrupt \
                 run exactly while the light's on",
                if self.tcb_running() {
                    "running"
                } else {
                    "stopped"
                },
                if self.enabled_high() { "high" } else { "low" }
            ));
        }
        let mut wakes = if self.powering_down() {
            self.power_downs += 1;
            // §18.3.3.1, table 13-4.
            self.interrupts(false)
        } else if ctrla == SLEEP_IDLE | SLPCTRL_SEN {
            self.enabled
        } else {
            return Err(format!(
                "sleeps with SLPCTRL.CTRLA {ctrla:#04x}, but the simulator only models \
                 idle and power-down"
            ));
        };
        if self.io[reg::RTC_PITCTRLA] & RTC_PITEN == 0 {
            wakes &= !(1 << VECTOR_RTC_PIT);
        }
        if wakes == 0 && self.enabled != 0 {
            return Err("sleeps with no interrupt enabled that could wake it".into());
        }
        self.down = wakes == 0;
        Ok(wakes != 0)
    }

    /// TCB0 halts in power-down (§24.3.6).
    fn idle(&mut self) -> Result<(), String> {
        if self.powering_down() {
            self.tcb.next_capt += self.next_pit - self.now;
            self.plan_next_event();
            return self.advance_to(self.next_pit);
        }
        self.advance_to(self.next_event)
    }

    fn consecutive_store(&mut self, addr: u16) -> Result<(), String> {
        let a = usize::from(addr);
        if addr < IO_LOW_END || a == reg::SLPCTRL_CTRLA {
            return Err(format!(
                "stores to {} straight after a store to 64 or over, and errata 2.2.3 loses \
                 that write: use `out`, or put a `nop` between",
                name(a)
            ));
        }
        Ok(())
    }

    fn read(&mut self, addr: u16) -> Result<u8, String> {
        let a = usize::from(addr);
        self.temp_undisturbed(a)?;
        if SRAM.contains(&addr) {
            return Ok(self.sram[usize::from(addr - SRAM.start)]);
        }
        if addr >= MAPPED {
            return Ok(*self.flash.get(usize::from(addr - MAPPED)).unwrap_or(&0xFF));
        }
        match a {
            // The real IN stops following the pin in power-down (§18.3.5), and
            // how soon after a wake it catches up isn't stated.
            reg::VPORTD_IN => {
                if self.io[SWITCH_CTRL] & PORT_ISC == ISC_INPUT_DISABLE {
                    return Err(
                        "reads PD4 with its input buffer off, so VPORTD.IN never changes \
                         (section 18.3.2.3)"
                            .into(),
                    );
                }
                if !self.in_handler {
                    self.switch_reads += 1;
                    self.switch_read_at = self.now;
                }
                Ok(if self.pressed { !SWITCH } else { 0xFF })
            }
            reg::VPORTD_INTFLAGS => Ok(self.portd_flags),
            reg::MVIO_STATUS => {
                self.boot.supply_read.get_or_insert(self.now);
                let status = if self.vddio2_ok() { MVIO_VDDIO2S } else { 0 };
                self.vddio2_goes()?;
                Ok(status)
            }
            reg::RTC_PITSTATUS => {
                let busy = self.pit_busy > 0;
                self.pit_busy = self.pit_busy.saturating_sub(1);
                Ok(if busy { RTC_CTRLBUSY } else { 0 })
            }
            reg::RTC_PITINTFLAGS => Ok(if self.pit_flag { RTC_PI } else { 0 }),
            reg::TWI0_MSTATUS => {
                let pc1_high = self.enabled_high();
                self.twi.status(self.now, pc1_high)
            }
            reg::TCB0_INTFLAGS => Ok(if self.tcb.capt { TCB_CAPT } else { 0 }),
            reg::WDT_STATUS => {
                self.wdt.polling = self.wdt.pending.is_some();
                Ok(if self.wdt.polling { WDT_SYNCBUSY } else { 0 })
            }
            reg::CRCSCAN_STATUS => Ok(self.crc_status()),
            reg::RSTCTRL_RSTFR => Ok(self.rstfr),
            reg::ADC0_INTFLAGS => {
                let ready = self.adc.ready_at.is_some_and(|at| self.now >= at);
                Ok(if ready && !self.world.scene.adc_dead {
                    ADC_RESRDY
                } else {
                    0
                })
            }
            reg::ADC0_RES => {
                // §8.10.3, §33.5.12
                if self.adc.ready_at.is_none_or(|at| self.now < at) {
                    return Err("reads ADC0.RES with no result ready".into());
                }
                self.adc.ready_at = None;
                let [lo, hi] = self.adc.result.to_le_bytes();
                self.adc.temp = Some(hi);
                Ok(lo)
            }
            _ if a == reg::ADC0_RES + 1 => self.adc.temp.take().ok_or_else(|| {
                "reads ADC0.RES's high byte without its low byte just before, so it gets \
                 whatever ADC0.TEMP held"
                    .into()
            }),
            // §8.6.2.2
            _ if (reg::SIGROW_TEMPSENSE0..reg::SIGROW_TEMPSENSE0 + 2).contains(&a) => {
                Ok(CALIBRATION.slope.to_le_bytes()[a - reg::SIGROW_TEMPSENSE0])
            }
            _ if (reg::SIGROW_TEMPSENSE1..reg::SIGROW_TEMPSENSE1 + 2).contains(&a) => {
                Ok(CALIBRATION.offset.to_le_bytes()[a - reg::SIGROW_TEMPSENSE1])
            }
            _ if self.world.fuse(a).is_some() => {
                self.boot.fuses_read.get_or_insert(self.now);
                Ok(self.world.fuse(a).unwrap_or(0xFF))
            }
            reg::VPORTA_DIR
            | reg::VPORTA_OUT
            | reg::VPORTC_DIR
            | reg::VPORTC_OUT
            | reg::VPORTD_DIR
            | reg::VPORTD_OUT
            | reg::VREF_DAC0REF
            | reg::VREF_ADC0REF => Ok(self.io[a]),
            _ if plain(a) || pin_ctrl(a) => Ok(self.io[a]),
            _ => Err(format!(
                "reads {}, which the simulator doesn't model",
                name(a)
            )),
        }
    }

    fn write(&mut self, addr: u16, value: u8) -> Result<(), String> {
        let a = usize::from(addr);
        self.temp_undisturbed(a)?;
        self.ccp_hold = false;
        if SRAM.contains(&addr) {
            self.sram[usize::from(addr - SRAM.start)] = value;
            return Ok(());
        }
        if addr >= MAPPED {
            return Err(format!(
                "writes {value:#04x} to {addr:#06x}, which is flash, and only NVMCTRL writes that"
            ));
        }
        self.register_writes += 1;
        self.boot.write(self.now, a, value);
        match a {
            reg::CPU_CCP => {
                if value != CCP_IOREG {
                    return Err(format!(
                        "writes {value:#04x} to CPU.CCP, which isn't the IOREG key"
                    ));
                }
                self.unlocked_until = Some(self.now + 4);
                self.ccp_hold = true;
            }
            reg::CLKCTRL_OSCHFCTRLA | reg::CLKCTRL_MCLKCTRLB => {
                self.take_protected_write(a)?;
                let want = if a == reg::CLKCTRL_OSCHFCTRLA {
                    OSCHF_4MHZ
                } else {
                    0
                };
                if value != want {
                    return Err(format!(
                        "writes {value:#04x} to {}: the firmware runs on the internal \
                         oscillator at 4 MHz, undivided, and every timing here assumes it",
                        name(a)
                    ));
                }
                self.io[a] = value;
            }
            reg::VPORTA_DIR | reg::VPORTA_OUT => self.write_porta(a, value)?,
            reg::VPORTC_DIR | reg::VPORTC_OUT => self.write_portc(a, value)?,
            reg::VPORTD_DIR | reg::VPORTD_OUT => self.write_portd(a, value)?,
            reg::VPORTD_INTFLAGS => self.portd_flags &= !value,
            _ if portf(a) => {
                return Err(format!(
                    "writes {value:#04x} to {}: PORTF is the reset pin and UPDI, which the \
                     firmware leaves alone",
                    name(a)
                ));
            }
            _ if pin_ctrl(a) => self.write_pin_ctrl(a, value)?,
            _ if twi(a) => {
                let pc1_high = self.enabled_high();
                self.twi.write(self.now, a, value, pc1_high)?;
            }
            reg::VREF_DAC0REF => self.write_dac_reference(value)?,
            reg::DAC0_CTRLA => match value {
                0 if self.enabled_high() => {
                    return Err(
                        "turns DAC0 off with PC1 high, leaving the amplifier's setpoint to PORT"
                            .into(),
                    );
                }
                0 => self.io[a] = 0,
                DAC_DRIVING => {
                    if self.io[DAC_PIN_CTRL] & PORT_ISC != ISC_INPUT_DISABLE {
                        return Err(
                            "turns DAC0's output on with PD6's input buffer still on, which \
                             section 34.3.1 wants off first"
                                .into(),
                        );
                    }
                    self.io[a] = value;
                }
                _ => {
                    return Err(only_models(a, value, "it off, or on and driving PD6"));
                }
            },
            reg::DAC0_DATA => {
                if value & !(regs::DAC_DATA as u8) != 0 {
                    return Err(format!(
                        "writes {value:#04x} to DAC0.DATAL, whose bits 5:0 aren't DATA \
                         (section 34.5.2) and get written 0 (section 8.10)"
                    ));
                }
                if self.dac.temp.is_some() {
                    return Err(
                        "writes DAC0.DATAL while the last DATAL is still waiting for DATAH".into(),
                    );
                }
                self.dac.temp = Some(value);
            }
            _ if a == reg::DAC0_DATA + 1 => {
                let lo = self.dac.temp.take().ok_or(
                    "writes the high byte of DAC0.DATA without its low byte just before, so the \
                     chip pairs it with whatever TEMP held",
                )?;
                self.set_dac(u16::from_le_bytes([lo, value]) >> DAC_SHIFT)?;
            }
            reg::TCB0_CTRLA => self.write_tcb_ctrla(value)?,
            reg::TCB0_CTRLB => {
                if value != regs::TCB_CNTMODE_INT {
                    return Err(only_models(
                        a,
                        value,
                        "periodic interrupt mode with no output",
                    ));
                }
                self.io[a] = value;
            }
            reg::TCB0_INTCTRL => {
                if value & !TCB_CAPT != 0 {
                    return Err(only_models(a, value, "CAPT"));
                }
                // A flag left up from before fires at once (§24.3.5).
                if value != 0
                    && self.io[a] == 0
                    && self.tcb.capt
                    && self.now - self.tcb.capt_up_at >= self.tcb.period()
                {
                    return Err(
                        "turns TCB0's interrupt on with INTFLAGS.CAPT still up from before, so \
                         the handler runs at once and counts a period that never was"
                            .into(),
                    );
                }
                self.io[a] = value;
                self.update_enabled();
            }
            reg::TCB0_INTFLAGS => {
                if value & TCB_CAPT != 0 {
                    self.tcb.capt = false;
                }
            }
            reg::TCB0_CNT | reg::TCB0_CCMP => {
                if let Some((pending, _)) = self.tcb.temp {
                    return Err(format!(
                        "writes the low byte of {} while {}'s is still waiting for its high byte",
                        name(a),
                        name(pending)
                    ));
                }
                self.tcb.temp = Some((a, value));
            }
            _ if a == reg::TCB0_CNT + 1 || a == reg::TCB0_CCMP + 1 => match self.tcb.temp.take() {
                Some((low, lo)) if low == a - 1 => {
                    self.tcb_written(low, u16::from_le_bytes([lo, value]))?;
                }
                _ => {
                    return Err(format!(
                        "writes the high byte of {} without its low byte just before, so \
                             the chip pairs it with whatever TEMP held",
                        name(a - 1)
                    ));
                }
            },
            reg::RTC_PITINTCTRL => {
                self.io[a] = value;
                self.update_enabled();
            }
            reg::RTC_PITCTRLA => {
                if self.pit_busy > 0 {
                    return Err(format!(
                        "writes {} while RTC.PITSTATUS.CTRLBUSY is set, which section 26.13.12 \
                         doesn't allow",
                        name(a)
                    ));
                }
                if value != PIT_FAST && value != PIT_SLOW {
                    return Err(only_models(
                        a,
                        value,
                        "the PIT on at 512 cycles, 64 Hz, or 32,768, 1 Hz",
                    ));
                }
                if value == PIT_SLOW && self.enabled_high() {
                    return Err(
                        "slows the PIT with PC1 high, so the light's protection runs once a \
                         second and the tick deadline's sums don't hold"
                            .into(),
                    );
                }
                let was_on = self.io[a] & RTC_PITEN != 0;
                let old = self.pit();
                self.io[a] = value;
                let pit = self.pit();
                if !was_on {
                    self.pit_next = self.grid + 1;
                } else if pit != old {
                    self.pit_next = self.pit_phase.first(self.grid, pit);
                }
                self.pit_busy = PIT_SYNC_READS;
            }
            reg::RTC_PITINTFLAGS => {
                if value & RTC_PI != 0 {
                    if self.pit_flag {
                        self.tick_taken_at = self.now;
                    }
                    self.pit_flag = false;
                }
            }
            reg::WDT_CTRLA => {
                self.take_protected_write(a)?;
                if self.wdt.pending.is_some() {
                    return Err("writes WDT.CTRLA while WDT.STATUS.SYNCBUSY is set, which \
                                the datasheet doesn't allow (section 22.3.6)"
                        .into());
                }
                if value & WDT_PERIOD > WDT_PERIOD_MAX || (value & WDT_WINDOW) >> 4 > WDT_PERIOD_MAX
                {
                    return Err(format!(
                        "writes {value:#04x} to WDT.CTRLA: a reserved WDT period or window"
                    ));
                }
                if value & WDT_PERIOD == 0 {
                    return Err(format!(
                        "writes {value:#04x} to WDT.CTRLA, which turns the watchdog off, and \
                         the firmware never does"
                    ));
                }
                if let Some(fed) = self.wdt.fed_at
                    && self.now < fed + self.wdt_time(WDR_SYNC_CLOCKS)
                {
                    return Err(
                        "writes WDT.CTRLA within three WDT clocks of a wdr, which may still be \
                         syncing, and the datasheet doesn't say SYNCBUSY covers that"
                            .into(),
                    );
                }
                if !self.wdt_on() {
                    self.wdt.since = self.now;
                }
                self.wdt.pending = Some((value, self.now + self.wdt_time(WDT_SYNC_CLOCKS)));
                self.plan_next_event();
            }
            reg::WDT_STATUS => {
                return Err(if value & WDT_LOCK != 0 {
                    "sets WDT.STATUS.LOCK, which only debug mode can clear".into()
                } else {
                    "writes WDT.STATUS, whose only writable bit is LOCK".into()
                });
            }
            reg::RSTCTRL_RSTFR => self.rstfr &= !value,
            reg::CRCSCAN_CTRLA => {
                if value != CRCSCAN_ENABLE {
                    return Err(only_models(
                        a,
                        value,
                        "ENABLE, with STATUS polled (NMIEN can't be undone without a reset)",
                    ));
                }
                self.start_crc()?;
                self.io[a] = value;
            }
            reg::ADC0_COMMAND => match value {
                0 => {}
                ADC_STCONV => self.start_conversion()?,
                _ => return Err(only_models(a, value, "STCONV")),
            },
            reg::ADC0_INTFLAGS => {
                // A conversion still running sets the flag again.
                if value & ADC_RESRDY != 0 && !self.converting() {
                    self.adc.ready_at = None;
                }
            }
            _ if ADC_SETUP.contains(&a) && self.converting() => {
                return Err(format!("writes {} mid-conversion", name(a)));
            }
            reg::VREF_ADC0REF => {
                if value != VREF_REFSEL_1V024 && value != VREF_REFSEL_2V048 {
                    return Err(only_models(
                        a,
                        value,
                        "1.024 V and 2.048 V, with ALWAYSON clear",
                    ));
                }
                self.io[a] = value;
            }
            reg::ADC0_CTRLA => {
                if value != 0 && value != ADC_ON {
                    return Err(only_models(
                        a,
                        value,
                        "it off, or on for single 12-bit conversions",
                    ));
                }
                if value & ADC_ENABLE == 0 {
                    // The next conversion catches the ADC starting up.
                    self.adc.last_setup = None;
                }
                self.io[a] = value;
            }
            _ if fuse_or_lock(a) => {
                return Err(format!(
                    "writes {value:#04x} to {}: the firmware never writes the fuses",
                    name(a)
                ));
            }
            _ if plain(a) => self.io[a] = value,
            _ => {
                return Err(format!(
                    "writes {value:#04x} to {}, which the simulator doesn't model",
                    name(a)
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kr1k::ramp::{self, Paths};

    fn poke(chip: &mut Chip, a: usize, value: u8) -> Result<(), String> {
        chip.write(a as u16, value)
    }

    fn refused(chip: &mut Chip, a: usize, value: u8) -> String {
        poke(chip, a, value).expect_err("the write went through")
    }

    fn unlocked(chip: &mut Chip, a: usize, value: u8) -> Result<(), String> {
        poke(chip, reg::CPU_CCP, CCP_IOREG)?;
        poke(chip, a, value)
    }

    fn wide(chip: &mut Chip, low: usize, value: u16) -> Result<(), String> {
        let [lo, hi] = value.to_le_bytes();
        chip.write(low as u16, lo)?;
        chip.write(low as u16 + 1, hi)
    }

    fn dac(chip: &mut Chip, code: u16) -> Result<(), String> {
        wide(chip, reg::DAC0_DATA, code << DAC_SHIFT)
    }

    fn wait(chip: &mut Chip, clocks: u64) {
        chip.advance_to(chip.now + clocks).unwrap();
    }

    /// As `hw::init` leaves it.
    fn dark() -> Chip {
        let mut chip = Chip::new(Vec::new());
        for (a, value) in [
            (DAC_PIN_CTRL, ISC_INPUT_DISABLE),
            (SWITCH_CTRL, ISC_BOTHEDGES),
            (reg::VPORTA_OUT, 0),
            (reg::VPORTA_DIR, PATHS | 1 << board::BUTTON_LED | HELD_LOW),
            (reg::VPORTC_OUT, 0),
            (reg::VPORTC_DIR, ENABLE),
            (reg::VPORTD_OUT, 0),
            (reg::VPORTD_DIR, DAC_PIN),
            (reg::VREF_DAC0REF, VREF_REFSEL_1V024),
            (reg::DAC0_CTRLA, DAC_DRIVING),
            (reg::TCB0_CTRLB, regs::TCB_CNTMODE_INT),
        ] {
            poke(&mut chip, a, value).unwrap();
        }
        wide(&mut chip, reg::TCB0_CCMP, TIMER_PERIOD_CLOCKS - 1).unwrap();
        chip
    }

    fn lit() -> Chip {
        let mut chip = dark();
        tcb_on(&mut chip);
        poke(&mut chip, reg::VPORTC_OUT, ENABLE).unwrap();
        wait(&mut chip, BOOST_START_CYCLES);
        chip
    }

    fn tcb_on(chip: &mut Chip) {
        wide(chip, reg::TCB0_CNT, 0).unwrap();
        poke(chip, reg::TCB0_INTFLAGS, TCB_CAPT).unwrap();
        poke(chip, reg::TCB0_INTCTRL, TCB_CAPT).unwrap();
        poke(chip, reg::TCB0_CTRLA, TCB_RUNNING).unwrap();
    }

    #[test]
    fn pc1_rises_only_with_dac0_on_at_0_its_reference_low_and_the_paths_off() {
        let rise = |setup: &dyn Fn(&mut Chip)| {
            let mut chip = dark();
            setup(&mut chip);
            poke(&mut chip, reg::VPORTC_OUT, ENABLE)
        };
        assert_eq!(rise(&|_| {}), Ok(()));
        let err = rise(&|chip| poke(chip, reg::DAC0_CTRLA, 0).unwrap()).unwrap_err();
        assert!(err.contains("without DAC0 on"), "{err}");
        let err = rise(&|chip| chip.dac.code = 1).unwrap_err();
        assert!(err.contains("with DAC0 at 1"), "{err}");
        let err =
            rise(&|chip| poke(chip, reg::VREF_DAC0REF, VREF_REFSEL_2V500).unwrap()).unwrap_err();
        assert!(err.contains("VREF.DAC0REF at 0x03"), "{err}");
        let moon = Paths::Moon.mask();
        let err = rise(&|chip| poke(chip, reg::VPORTA_OUT, moon).unwrap()).unwrap_err();
        assert!(err.contains("power paths 0x80 on"), "{err}");
        let mut chip = dark();
        poke(&mut chip, reg::VPORTC_DIR, 0).unwrap();
        poke(&mut chip, reg::VPORTC_OUT, ENABLE).unwrap();
        poke(&mut chip, reg::VPORTA_OUT, moon).unwrap();
        let err = refused(&mut chip, reg::VPORTC_DIR, ENABLE);
        assert!(err.contains("power paths"), "{err}");
    }

    #[test]
    fn dac0_goes_above_0_only_8_ms_after_pc1_rose() {
        let mut chip = dark();
        let err = dac(&mut chip, 1).unwrap_err();
        assert!(err.contains("with PC1 low"), "{err}");
        chip.dac.temp = None;
        tcb_on(&mut chip);
        poke(&mut chip, reg::VPORTC_OUT, ENABLE).unwrap();
        wait(&mut chip, BOOST_START_CYCLES - 1);
        let err = dac(&mut chip, 1).unwrap_err();
        assert!(
            err.contains("the stock firmware gives the boost converter"),
            "{err}"
        );
        wait(&mut chip, 1);
        dac(&mut chip, 1).unwrap();
        assert_eq!(chip.dac.code, 1);
    }

    #[test]
    fn dac0_waits_out_a_reference_change() {
        let mut chip = lit();
        poke(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_2V500).unwrap();
        wait(&mut chip, VREF_SETTLE_CYCLES - 1);
        let err = dac(&mut chip, 5).unwrap_err();
        assert!(err.contains("after its reference changed"), "{err}");
        chip.dac.temp = None;
        wait(&mut chip, 1);
        dac(&mut chip, 5).unwrap();
    }

    #[test]
    fn a_raised_reference_or_an_added_path_wants_dac0_quiet() {
        let raise = |chip: &mut Chip| poke(chip, reg::VREF_DAC0REF, VREF_REFSEL_2V500);
        let add = |chip: &mut Chip| poke(chip, reg::VPORTA_OUT, Paths::Moon.mask());
        for change in [raise, add] {
            let mut chip = lit();
            dac(&mut chip, 100).unwrap();
            let err = change(&mut chip).unwrap_err();
            assert!(err.contains("with PC1 high and DAC0 at 100"), "{err}");
            dac(&mut chip, 0).unwrap();
            wait(&mut chip, DAC_SETTLE_CYCLES - 1);
            let err = change(&mut chip).unwrap_err();
            assert!(err.contains("table 38-23"), "{err}");
            wait(&mut chip, 1);
            change(&mut chip).unwrap();
        }
    }

    #[test]
    fn a_lowered_reference_or_a_removed_path_wants_dac0_at_0_but_no_wait() {
        let mut chip = lit();
        poke(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_2V500).unwrap();
        poke(&mut chip, reg::VPORTA_OUT, Paths::High.mask()).unwrap();
        wait(&mut chip, VREF_SETTLE_CYCLES);
        dac(&mut chip, 500).unwrap();
        let err = refused(&mut chip, reg::VPORTA_OUT, Paths::Low.mask());
        assert!(
            err.contains("turns power paths 0x20 off with PC1 high and DAC0 at 500"),
            "{err}"
        );
        let err = refused(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_1V024);
        assert!(err.contains("lowers VREF.DAC0REF"), "{err}");
        dac(&mut chip, 0).unwrap();
        poke(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_1V024).unwrap();
        poke(&mut chip, reg::VPORTA_OUT, Paths::Low.mask()).unwrap();
        let mut chip = dark();
        poke(&mut chip, reg::VPORTA_OUT, Paths::High.mask()).unwrap();
        poke(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_2V500).unwrap();
    }

    #[test]
    fn pc1_drops_with_dac0_at_0_and_its_reference_low() {
        let mut chip = lit();
        dac(&mut chip, 10).unwrap();
        let err = refused(&mut chip, reg::VPORTC_OUT, 0);
        assert!(err.contains("drops PC1 with DAC0 still at 10"), "{err}");
        dac(&mut chip, 0).unwrap();
        wait(&mut chip, DAC_SETTLE_CYCLES);
        poke(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_2V500).unwrap();
        let err = refused(&mut chip, reg::VPORTC_OUT, 0);
        assert!(err.contains("VREF.DAC0REF still above"), "{err}");
        poke(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_1V024).unwrap();
        poke(&mut chip, reg::VPORTC_OUT, 0).unwrap();
    }

    #[test]
    fn dac0s_output_wants_pd6s_input_buffer_off_and_stays_on_with_pc1_high() {
        let mut chip = Chip::new(Vec::new());
        let err = refused(&mut chip, reg::DAC0_CTRLA, DAC_DRIVING);
        assert!(err.contains("PD6's input buffer still on"), "{err}");
        let mut chip = dark();
        let err = refused(&mut chip, DAC_PIN_CTRL, ISC_INTDISABLE);
        assert!(err.contains("back on with DAC0 driving it"), "{err}");
        let mut chip = lit();
        let err = refused(&mut chip, reg::DAC0_CTRLA, 0);
        assert!(err.contains("turns DAC0 off with PC1 high"), "{err}");
        let err = refused(&mut chip, reg::DAC0_CTRLA, DAC_DRIVING | regs::DAC_RUNSTDBY);
        assert!(err.contains("only models it off"), "{err}");
    }

    #[test]
    fn dac0s_data_goes_low_byte_first_with_its_low_bits_clear() {
        let mut chip = lit();
        let err = refused(&mut chip, reg::DAC0_DATA + 1, 1);
        assert!(err.contains("without its low byte"), "{err}");
        let err = refused(&mut chip, reg::DAC0_DATA, 0x01);
        assert!(err.contains("bits 5:0"), "{err}");
        wide(&mut chip, reg::DAC0_DATA, 0xFFC0).unwrap();
        assert_eq!(chip.dac.code, 1023);
    }

    #[test]
    fn shows_compares_every_output_pin() {
        let mut chip = lit();
        assert_eq!(chip.shows(Output::STARTING), Ok(()));
        let level = ramp::output(1);
        let err = chip.shows(level).unwrap_err();
        assert!(err.contains("the library says"), "{err}");
        poke(&mut chip, reg::VPORTA_OUT, level.paths.mask()).unwrap();
        wait(&mut chip, VREF_SETTLE_CYCLES);
        dac(&mut chip, level.dac).unwrap();
        assert_eq!(chip.shows(level), Ok(()));
        let err = chip.shows(Output::OFF).unwrap_err();
        assert!(err.contains("PC1 is high"), "{err}");
    }

    #[test]
    fn tcb0_flags_capt_every_ccmp_plus_one_clocks() {
        let mut chip = dark();
        tcb_on(&mut chip);
        wait(&mut chip, TIMER_PERIOD - 1);
        assert!(!chip.tcb.capt);
        wait(&mut chip, 1);
        assert!(chip.tcb.capt);
        assert_eq!(chip.interrupt(), Some(VECTOR_TCB0));
        poke(&mut chip, reg::TCB0_INTFLAGS, TCB_CAPT).unwrap();
        wait(&mut chip, TIMER_PERIOD);
        assert!(chip.tcb.capt);
    }

    #[test]
    fn tcb0s_interrupt_goes_on_with_its_flag_cleared_first() {
        let mut chip = dark();
        tcb_on(&mut chip);
        poke(&mut chip, reg::TCB0_INTCTRL, 0).unwrap();
        wait(&mut chip, 2 * TIMER_PERIOD);
        let err = refused(&mut chip, reg::TCB0_INTCTRL, TCB_CAPT);
        assert!(err.contains("CAPT still up from before"), "{err}");
        poke(&mut chip, reg::TCB0_INTFLAGS, TCB_CAPT).unwrap();
        poke(&mut chip, reg::TCB0_INTCTRL, TCB_CAPT).unwrap();
    }

    #[test]
    fn tcb0_runs_only_off_the_main_clock_in_periodic_mode() {
        let mut chip = dark();
        let err = refused(
            &mut chip,
            reg::TCB0_CTRLA,
            regs::TCB_CLKSEL_DIV2 | TCB_ENABLE,
        );
        assert!(err.contains("on and undivided"), "{err}");
        let err = refused(&mut chip, reg::TCB0_CTRLB, regs::TCB_CNTMODE_PWM8);
        assert!(err.contains("periodic interrupt mode"), "{err}");
        let err = refused(&mut chip, reg::TCB0_INTCTRL, regs::TCB_OVF);
        assert!(err.contains("only models CAPT"), "{err}");
        tcb_on(&mut chip);
        let err = wide(&mut chip, reg::TCB0_CCMP, 10).unwrap_err();
        assert!(err.contains("CCMP with TCB0 running"), "{err}");
    }

    #[test]
    fn tcb0s_wide_registers_go_low_byte_first_one_at_a_time() {
        let mut chip = dark();
        poke(&mut chip, reg::TCB0_CNT, 0).unwrap();
        let err = refused(&mut chip, reg::TCB0_CCMP, 0);
        assert!(err.contains("TCB0.CNT's is still waiting"), "{err}");
        let err = refused(&mut chip, reg::TCB0_CCMP + 1, 0);
        assert!(err.contains("TCB0.CCMP without its low byte"), "{err}");
    }

    #[test]
    fn sleep_wants_tcb0_running_exactly_while_pc1_is_high() {
        let mut chip = dark();
        poke(&mut chip, reg::SLPCTRL_CTRLA, SLEEP_IDLE | SLPCTRL_SEN).unwrap();
        tcb_on(&mut chip);
        let err = chip.sleep().unwrap_err();
        assert!(err.contains("TCB0 running and PC1 low"), "{err}");
        poke(&mut chip, reg::VPORTC_OUT, ENABLE).unwrap();
        assert_eq!(chip.sleep(), Ok(true));
        poke(&mut chip, reg::TCB0_CTRLA, 0).unwrap();
        let err = chip.sleep().unwrap_err();
        assert!(err.contains("TCB0 stopped and PC1 high"), "{err}");
    }

    #[test]
    fn power_down_wants_pc1_low_tcb0_stopped_and_the_adc_off() {
        let mut chip = lit();
        chip.pit_busy = 0;
        let err = chip.ready_for_power_down().unwrap_err();
        assert!(err.contains("PC1 high"), "{err}");
        poke(&mut chip, reg::VPORTC_OUT, 0).unwrap();
        let err = chip.ready_for_power_down().unwrap_err();
        assert!(err.contains("TCB0 running"), "{err}");
        poke(&mut chip, reg::TCB0_CTRLA, 0).unwrap();
        chip.ready_for_power_down().unwrap();
        let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
        chip.pit_busy = 0;
        let err = chip.ready_for_power_down().unwrap_err();
        assert!(err.contains("ADC0 still enabled"), "{err}");
        start(&mut chip).unwrap();
        let err = chip.ready_for_power_down().unwrap_err();
        assert!(err.contains("mid-conversion"), "{err}");
        let chip = Chip::new(Vec::new());
        let err = chip.ready_for_power_down().unwrap_err();
        assert!(err.contains("CTRLBUSY"), "{err}");
    }

    #[test]
    fn power_down_halts_tcb0_but_not_the_pit_and_wakes_on_the_switch() {
        let mut chip = dark();
        poke(&mut chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        chip.io[reg::RTC_PITCTRLA] = RTC_PITEN;
        chip.pit_busy = 0;
        poke(
            &mut chip,
            reg::SLPCTRL_CTRLA,
            SLEEP_POWER_DOWN | SLPCTRL_SEN,
        )
        .unwrap();
        assert_eq!(chip.sleep(), Ok(true));
        chip.idle().unwrap();
        assert!(chip.pit_flag);
        assert_eq!(
            chip.interrupts(false) & 1 << VECTOR_PORTD,
            1 << VECTOR_PORTD
        );
        assert_eq!(chip.interrupts(false) & 1 << VECTOR_TCB0, 0);
    }

    #[test]
    fn the_dangerous_pins_never_get_a_pull_up() {
        let pin = |base: usize, n: u8| base + usize::from(n);
        for a in [
            pin(reg::PORTC_PIN0CTRL, board::ENABLE),
            SWITCH_CTRL,
            DAC_PIN_CTRL,
            HELD_LOW_CTRL,
            pin(reg::PORTA_PIN0CTRL, board::PATH1),
            pin(reg::PORTA_PIN0CTRL, board::PATH2),
            pin(reg::PORTA_PIN0CTRL, board::PATH3),
        ] {
            let mut chip = Chip::new(Vec::new());
            let err = refused(&mut chip, a, PORT_PULLUPEN);
            assert!(err.contains("a pull-up"), "{}: {err}", name(a));
        }
        let mut chip = Chip::new(Vec::new());
        poke(
            &mut chip,
            pin(reg::PORTC_PIN0CTRL, board::UNUSED_PC2),
            PORT_PULLUPEN,
        )
        .unwrap();
    }

    #[test]
    fn pa1_is_an_output_that_never_goes_high() {
        let mut chip = Chip::new(Vec::new());
        poke(&mut chip, reg::VPORTA_OUT, 0).unwrap();
        poke(&mut chip, reg::VPORTA_DIR, HELD_LOW).unwrap();
        poke(&mut chip, HELD_LOW_CTRL, 0).unwrap();
        let err = refused(&mut chip, reg::VPORTA_OUT, HELD_LOW);
        assert!(err.contains("setting PA1"), "{err}");
        assert_eq!(chip.io[reg::VPORTA_OUT], 0);
    }

    #[test]
    fn the_twi_lines_portf_and_inven_are_left_alone() {
        let mut chip = Chip::new(Vec::new());
        let sda = reg::PORTA_PIN0CTRL + usize::from(board::TWI_SDA);
        assert!(refused(&mut chip, sda, PORT_PULLUPEN).contains("TWI lines"));
        assert!(refused(&mut chip, reg::VPORTA_DIR, TWI).contains("TWI lines"));
        assert!(refused(&mut chip, reg::VPORTF_OUT, 0).contains("PORTF"));
        assert!(refused(&mut chip, reg::PORTF_PIN7CTRL, 0).contains("PORTF"));
        assert!(refused(&mut chip, SWITCH_CTRL, PORT_INVEN).contains("INVEN"));
    }

    #[test]
    fn only_pc1_and_pd6_are_outputs_and_pd6_stays_low() {
        let mut chip = Chip::new(Vec::new());
        assert!(refused(&mut chip, reg::VPORTC_DIR, ENABLE | 0x04).contains("only PC1"));
        assert!(refused(&mut chip, reg::VPORTD_DIR, DAC_PIN | SWITCH).contains("only PD6"));
        assert!(refused(&mut chip, reg::VPORTD_OUT, DAC_PIN).contains("held low"));
    }

    #[test]
    fn pd4_reads_through_the_boards_pull_up_and_an_edge_flags_it() {
        let mut chip = Chip::new(Vec::new());
        let read = |chip: &mut Chip| chip.read(reg::VPORTD_IN as u16);
        assert_eq!(read(&mut chip), Ok(0xFF));
        chip.set_switch(true).unwrap();
        assert_eq!(read(&mut chip), Ok(!SWITCH));
        assert_eq!(chip.portd_flags, 0, "INTDISABLE asks for no edge");
        poke(&mut chip, SWITCH_CTRL, ISC_BOTHEDGES).unwrap();
        chip.set_switch(false).unwrap();
        assert_eq!(chip.portd_flags, SWITCH);
        assert_eq!(chip.interrupt(), Some(VECTOR_PORTD));
        poke(&mut chip, reg::VPORTD_INTFLAGS, SWITCH).unwrap();
        assert_eq!(chip.interrupt(), None);
        poke(&mut chip, SWITCH_CTRL, ISC_INPUT_DISABLE).unwrap();
        assert!(read(&mut chip).unwrap_err().contains("input buffer off"));
        poke(&mut chip, SWITCH_CTRL, ISC_LEVEL).unwrap();
        assert!(chip.set_switch(true).unwrap_err().contains("doesn't model"));
    }

    fn single_supply(mut world: World) -> World {
        world.fuses[reg::FUSE_SYSCFG1 - reg::FUSE_WDTCFG] = regs::FUSE_MVSYSCFG_SINGLE;
        world
    }

    #[test]
    fn vddio2s_reads_the_world_with_dual_supply_and_1_with_single() {
        let mut chip = Chip::new(Vec::new());
        let read = |chip: &mut Chip| chip.read(reg::MVIO_STATUS as u16);
        assert_eq!(read(&mut chip), Ok(MVIO_VDDIO2S));
        chip.set_vddio2(false).unwrap();
        assert_eq!(read(&mut chip), Ok(0));
        assert_eq!(chip.vddio2_lost_at, Some(chip.now));
        chip.set_vddio2(true).unwrap();
        assert_eq!(read(&mut chip), Ok(MVIO_VDDIO2S));
        assert_eq!(chip.vddio2_lost_at, None);
        chip.set_world(single_supply(World::NORMAL)).unwrap();
        chip.set_vddio2(false).unwrap();
        assert_eq!(read(&mut chip), Ok(MVIO_VDDIO2S));
        assert_eq!(chip.vddio2_lost_at, None);
    }

    fn take_tick(chip: &mut Chip) {
        chip.pit_flag = true;
        poke(chip, reg::RTC_PITINTFLAGS, RTC_PI).unwrap();
    }

    fn lit_and_idling() -> Chip {
        let mut chip = lit();
        poke(&mut chip, reg::VPORTA_OUT, PATHS).unwrap();
        dac(&mut chip, 300).unwrap();
        poke(&mut chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        poke(&mut chip, reg::SLPCTRL_CTRLA, SLEEP_IDLE | SLPCTRL_SEN).unwrap();
        chip
    }

    fn turn_off(chip: &mut Chip) {
        dac(chip, 0).unwrap();
        poke(chip, reg::VREF_DAC0REF, VREF_REFSEL_1V024).unwrap();
        poke(chip, reg::VPORTC_OUT, 0).unwrap();
        poke(chip, reg::VPORTA_OUT, 0).unwrap();
        poke(chip, reg::TCB0_CTRLA, 0).unwrap();
        poke(chip, reg::TCB0_INTCTRL, 0).unwrap();
    }

    #[test]
    fn a_pass_that_begins_without_vddio2_cant_sleep_with_the_light_on() {
        let mut chip = lit_and_idling();
        chip.set_vddio2(false).unwrap();
        assert!(chip.lost_lit);
        wait(&mut chip, 10);
        take_tick(&mut chip);
        let err = chip.sleep().unwrap_err();
        assert!(
            err.contains("began with MVIO.STATUS.VDDIO2S clear") && err.contains("DAC0 at 300"),
            "{err}"
        );
        turn_off(&mut chip);
        assert_eq!(chip.sleep(), Ok(true));

        let mut chip = lit_and_idling();
        chip.set_vddio2(false).unwrap();
        take_tick(&mut chip);
        dac(&mut chip, 0).unwrap();
        poke(&mut chip, reg::VPORTC_OUT, 0).unwrap();
        poke(&mut chip, reg::TCB0_CTRLA, 0).unwrap();
        poke(&mut chip, reg::TCB0_INTCTRL, 0).unwrap();
        let err = chip.sleep().unwrap_err();
        assert!(
            err.contains("PC1 clear in PORTC, DAC0 at 0") && err.contains("the paths 0xe0"),
            "{err}"
        );
        poke(&mut chip, reg::VPORTA_OUT, 0).unwrap();
        assert_eq!(chip.sleep(), Ok(true));
    }

    #[test]
    fn a_pass_already_under_way_when_vddio2_goes_is_let_off_until_the_next() {
        let mut chip = lit_and_idling();
        take_tick(&mut chip);
        wait(&mut chip, 10);
        chip.set_vddio2(false).unwrap();
        assert_eq!(chip.sleep(), Ok(true), "it may have looked before it went");
        wait(&mut chip, 10);
        take_tick(&mut chip);
        let err = chip.sleep().unwrap_err();
        assert!(
            err.contains("began with MVIO.STATUS.VDDIO2S clear"),
            "{err}"
        );
    }

    #[test]
    fn single_supply_asks_nothing_when_vddio2_goes() {
        let mut chip = lit_and_idling();
        chip.set_world(single_supply(World::NORMAL)).unwrap();
        chip.set_vddio2(false).unwrap();
        take_tick(&mut chip);
        assert_eq!(chip.sleep(), Ok(true));
    }

    #[test]
    fn vddio2_coming_back_raises_pc1_if_portc_still_has_it_set() {
        let mut chip = lit_and_idling();
        chip.set_vddio2(false).unwrap();
        let err = chip.set_vddio2(true).unwrap_err();
        assert!(
            err.contains("VDDIO2 comes back with PC1 still set in PORTC")
                && err.contains("raises PC1 with DAC0 at 300"),
            "{err}"
        );
        let mut chip = lit_and_idling();
        chip.set_vddio2(false).unwrap();
        turn_off(&mut chip);
        let rises = chip.pc1_rises;
        chip.set_vddio2(true).unwrap();
        assert_eq!(chip.pc1_rises, rises);
        let mut chip = lit();
        chip.set_vddio2(false).unwrap();
        wait(&mut chip, 100);
        chip.set_vddio2(true).unwrap();
        assert_eq!(chip.pc1_rises, 2);
        assert!(dac(&mut chip, 300).unwrap_err().contains("8 ms"));
    }

    #[test]
    fn late_vddio2_goes_after_the_look_or_as_the_firmware_sleeps() {
        let lost = World::from(Scene {
            vddio2: false,
            ..Scene::NORMAL
        });
        let read = |chip: &mut Chip| chip.read(reg::MVIO_STATUS as u16);
        let mut chip = lit_and_idling();
        chip.enter(lost).unwrap();
        assert!(!chip.vddio2_ok());
        let mut chip = lit_and_idling();
        chip.late_vddio2 = true;
        chip.enter(lost).unwrap();
        assert!(chip.vddio2_ok());
        take_tick(&mut chip);
        wait(&mut chip, 10);
        assert_eq!(read(&mut chip), Ok(MVIO_VDDIO2S));
        assert_eq!(chip.vddio2_lost_at, Some(chip.now));
        assert_eq!(read(&mut chip), Ok(0));
        assert_eq!(chip.sleep(), Ok(true), "the pass that had already looked");
        chip.enter(lost).unwrap();
        assert!(!chip.vddio2_going && !chip.vddio2_ok());
        let mut chip = lit_and_idling();
        chip.late_vddio2 = true;
        chip.enter(lost).unwrap();
        take_tick(&mut chip);
        assert_eq!(chip.sleep(), Ok(true));
        assert!(!chip.vddio2_ok());
        chip.late_vddio2 = true;
        turn_off(&mut chip);
        chip.enter(World::NORMAL).unwrap();
        assert!(chip.vddio2_ok());
    }

    fn adc_ready(muxpos: u8) -> Chip {
        let mut chip = Chip::new(Vec::new());
        let reference = if muxpos == ADC_MUXPOS_TEMPSENSE {
            VREF_REFSEL_2V048
        } else {
            VREF_REFSEL_1V024
        };
        for (a, value) in [
            (reg::ADC0_CTRLB, ADC_SAMPNUM_ACC4),
            (reg::ADC0_CTRLC, regs::ADC_PRESC_DIV4),
            (reg::ADC0_CTRLD, regs::ADC_INITDLY_DLY32),
            (reg::ADC0_SAMPCTRL, 32),
            (reg::VREF_ADC0REF, reference),
            (reg::ADC0_MUXPOS, muxpos),
            (reg::ADC0_CTRLA, ADC_ON),
        ] {
            poke(&mut chip, a, value).unwrap();
        }
        chip
    }

    fn start(chip: &mut Chip) -> Result<(), String> {
        poke(chip, reg::ADC0_COMMAND, ADC_STCONV)
    }

    const CONVERSION: u64 = 4 + (32 + 4 * 34 + 54) * 4;

    fn convert(chip: &mut Chip) -> u16 {
        start(chip).unwrap();
        wait(chip, CONVERSION);
        assert_eq!(chip.read(reg::ADC0_INTFLAGS as u16), Ok(ADC_RESRDY));
        let lo = chip.read(reg::ADC0_RES as u16).unwrap();
        let hi = chip.read(reg::ADC0_RES as u16 + 1).unwrap();
        u16::from_le_bytes([lo, hi])
    }

    #[test]
    fn the_first_conversion_after_a_change_is_junk_then_it_reads_the_world() {
        for (muxpos, channel) in [
            (ADC_MUXPOS_VDDDIV10, Channel::Voltage),
            (ADC_MUXPOS_TEMPSENSE, Channel::Temperature),
            (ADC_MUXPOS_GND, Channel::Ground),
        ] {
            let mut chip = adc_ready(muxpos);
            assert_eq!(convert(&mut chip), UNSETTLED);
            assert_eq!(convert(&mut chip), World::NORMAL.reading(channel));
        }
        let voltage = World::NORMAL.reading(Channel::Voltage);
        assert_eq!(kr1k::battery::centivolts(voltage), 380);
        assert_eq!(World::NORMAL.reading(Channel::Temperature), 8603);
        assert_eq!(World::NORMAL.reading(Channel::Ground), 20);
    }

    #[test]
    fn ground_only_converts_against_1_024_v() {
        let mut chip = adc_ready(ADC_MUXPOS_GND);
        poke(&mut chip, reg::VREF_ADC0REF, VREF_REFSEL_2V048).unwrap();
        let err = start(&mut chip).unwrap_err();
        assert!(err.contains("MUXPOS 0x40"), "{err}");
    }

    #[test]
    fn a_stuck_adc_reads_one_value_for_every_input() {
        for muxpos in [ADC_MUXPOS_VDDDIV10, ADC_MUXPOS_TEMPSENSE, ADC_MUXPOS_GND] {
            let mut chip = adc_ready(muxpos);
            chip.world.scene.adc_stuck = Some(6140);
            convert(&mut chip);
            assert_eq!(convert(&mut chip), 6140, "{muxpos:#04x}");
        }
    }

    #[test]
    fn a_result_isnt_there_until_the_conversion_ends_and_reading_it_clears_resrdy() {
        let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
        start(&mut chip).unwrap();
        wait(&mut chip, CONVERSION - 1);
        assert_eq!(chip.read(reg::ADC0_INTFLAGS as u16), Ok(0));
        assert!(
            chip.read(reg::ADC0_RES as u16)
                .unwrap_err()
                .contains("no result")
        );
        let err = refused(&mut chip, reg::ADC0_MUXPOS, ADC_MUXPOS_TEMPSENSE);
        assert!(err.contains("mid-conversion"), "{err}");
        let err = start(&mut chip).unwrap_err();
        assert!(err.contains("still running"), "{err}");
        wait(&mut chip, 1);
        chip.read(reg::ADC0_RES as u16).unwrap();
        assert_eq!(chip.read(reg::ADC0_INTFLAGS as u16), Ok(0));
    }

    #[test]
    fn the_results_high_byte_needs_its_low_byte_first() {
        let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
        start(&mut chip).unwrap();
        wait(&mut chip, CONVERSION);
        let err = chip.read(reg::ADC0_RES as u16 + 1).unwrap_err();
        assert!(err.contains("without its low byte"), "{err}");
        chip.read(reg::ADC0_RES as u16).unwrap();
        chip.read(reg::ADC0_RES as u16 + 1).unwrap();
        assert!(chip.read(reg::ADC0_RES as u16 + 1).is_err(), "only once");
    }

    #[test]
    fn a_conversion_only_starts_set_up_as_the_firmware_sets_it_up() {
        let ten_bit = ADC_ENABLE | regs::ADC_RESSEL_10BIT;
        for (a, value, why) in [
            (reg::ADC0_CTRLB, 0, "four samples"),
            (reg::ADC0_CTRLC, regs::ADC_PRESC_DIV64, "at 62500 Hz"),
            (
                reg::VREF_ADC0REF,
                VREF_REFSEL_2V048,
                "VDD / 10 and ground against 1.024 V",
            ),
            (
                reg::ADC0_MUXPOS,
                regs::ADC_MUXPOS_AIN1,
                "VDD / 10 and ground against 1.024 V",
            ),
        ] {
            let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
            poke(&mut chip, a, value).unwrap();
            let err = start(&mut chip).unwrap_err();
            assert!(err.contains(why), "{}: {err}", name(a));
        }
        let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
        let err = refused(&mut chip, reg::ADC0_CTRLA, ten_bit);
        assert!(err.contains("only models it off"), "{err}");
        let err = refused(&mut chip, reg::VREF_ADC0REF, regs::VREF_ALWAYSON);
        assert!(err.contains("ALWAYSON clear"), "{err}");
        let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
        poke(&mut chip, reg::ADC0_CTRLA, 0).unwrap();
        let err = start(&mut chip).unwrap_err();
        assert!(err.contains("wants the ADC enabled"), "{err}");
        let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
        poke(&mut chip, reg::ADC0_CTRLC, regs::ADC_PRESC_DIV2).unwrap();
        start(&mut chip).unwrap();
    }

    #[test]
    fn the_temperature_sensor_wants_25_us_of_initdly_and_28_of_samplen() {
        for (a, value) in [
            (reg::ADC0_CTRLD, regs::ADC_INITDLY_DLY16),
            (reg::ADC0_SAMPCTRL, 27),
        ] {
            let mut chip = adc_ready(ADC_MUXPOS_TEMPSENSE);
            poke(&mut chip, a, value).unwrap();
            let err = start(&mut chip).unwrap_err();
            assert!(err.contains("at least 25 and 28"), "{}: {err}", name(a));
            let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
            poke(&mut chip, a, value).unwrap();
            start(&mut chip).unwrap();
        }
        let mut chip = adc_ready(ADC_MUXPOS_TEMPSENSE);
        poke(&mut chip, reg::ADC0_SAMPCTRL, 28).unwrap();
        start(&mut chip).unwrap();
    }

    fn synced(chip: &mut Chip) {
        let at = chip.wdt.pending.unwrap().1;
        chip.advance_to(at).unwrap();
        assert!(chip.wdt.pending.is_none());
    }

    fn watched() -> Chip {
        let mut chip = Chip::new(Vec::new());
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap();
        synced(&mut chip);
        chip
    }

    fn ticks(chip: &mut Chip, n: u64) -> Result<(), String> {
        chip.advance_to(chip.now + n * TICK_CYCLES)
    }

    fn clocks(n: u64) -> u64 {
        n * TICK_CYCLES / 16
    }

    #[test]
    fn a_feed_in_the_open_window_is_fine_and_a_closed_one_bites() {
        let mut chip = watched();
        chip.wdr().unwrap();
        ticks(&mut chip, 3).unwrap();
        chip.wdr().unwrap();
        ticks(&mut chip, 1).unwrap();
        assert!(chip.wdr().unwrap_err().contains("closed window"));
    }

    #[test]
    fn a_feed_within_a_clock_of_either_edge_bites() {
        let mut chip = watched();
        chip.wdr().unwrap();
        chip.advance_to(chip.now + clocks(33)).unwrap();
        assert!(chip.wdr().unwrap_err().contains("closed window"));
        let mut chip = watched();
        chip.wdr().unwrap();
        chip.advance_to(chip.now + clocks(34)).unwrap();
        chip.wdr().unwrap();
        let mut chip = watched();
        chip.wdr().unwrap();
        chip.advance_to(chip.now + clocks(62)).unwrap();
        chip.wdr().unwrap();
        let err = ticks(&mut chip, 4).unwrap_err();
        assert!(err.contains("without wdr"), "{err}");
        assert_eq!(chip.wdt_bite_at(), Some(chip.wdt.since + clocks(63)));
    }

    #[test]
    fn going_without_a_feed_bites() {
        let mut chip = watched();
        let err = ticks(&mut chip, 2).unwrap_err();
        assert!(err.contains("without wdr"), "{err}");
    }

    #[test]
    fn the_watchdog_wont_be_turned_off_or_fed_before_it_syncs() {
        let mut chip = watched();
        let err = unlocked(&mut chip, reg::WDT_CTRLA, 0).unwrap_err();
        assert!(err.contains("turns the watchdog off"), "{err}");
        let mut chip = Chip::new(Vec::new());
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap();
        assert!(chip.wdr().unwrap_err().contains("synced"));
        let err = unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap_err();
        assert!(err.contains("SYNCBUSY is set"), "{err}");
        assert_eq!(
            chip.read(reg::WDT_STATUS as u16).unwrap(),
            WDT_SYNCBUSY,
            "busy for three WDT clocks"
        );
        chip.advance_to(clocks(3) - 1).unwrap();
        assert_eq!(chip.read(reg::WDT_STATUS as u16).unwrap(), WDT_SYNCBUSY);
        chip.advance_to(clocks(3)).unwrap();
        assert_eq!(chip.read(reg::WDT_STATUS as u16).unwrap(), 0);
    }

    #[test]
    fn a_new_setting_counts_from_the_last_feed() {
        let mut chip = Chip::new(Vec::new());
        unlocked(&mut chip, reg::WDT_CTRLA, regs::WDT_PERIOD_8KCLK).unwrap();
        synced(&mut chip);
        chip.wdr().unwrap();
        ticks(&mut chip, 1).unwrap();
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap();
        synced(&mut chip);
        assert!(!chip.wdt.windowed);
        chip.wdr().unwrap();
        assert!(chip.wdt.windowed, "the first wdr starts the window");
        let mut chip = Chip::new(Vec::new());
        unlocked(&mut chip, reg::WDT_CTRLA, regs::WDT_PERIOD_8KCLK).unwrap();
        synced(&mut chip);
        chip.wdr().unwrap();
        ticks(&mut chip, 2).unwrap();
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap();
        let at = chip.wdt.pending.unwrap().1;
        let err = chip.advance_to(at).unwrap_err();
        assert!(err.contains("32-clock timeout"), "{err}");
    }

    #[test]
    fn ctrla_waits_three_wdt_clocks_after_a_feed() {
        let mut chip = watched();
        chip.wdr().unwrap();
        ticks(&mut chip, 3).unwrap();
        chip.wdr().unwrap();
        chip.advance_to(chip.now + clocks(3) - 1).unwrap();
        let err = unlocked(&mut chip, reg::WDT_CTRLA, regs::WDT_PERIOD_8KCLK).unwrap_err();
        assert!(err.contains("within three WDT clocks of a wdr"), "{err}");
        chip.elapse().unwrap();
        unlocked(&mut chip, reg::WDT_CTRLA, regs::WDT_PERIOD_8KCLK).unwrap();
    }

    #[test]
    fn power_down_waits_for_the_watchdog_to_sync() {
        let mut chip = dark();
        chip.pit_busy = 0;
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap();
        let err = chip.ready_for_power_down().unwrap_err();
        assert!(err.contains("SYNCBUSY"), "{err}");
        synced(&mut chip);
        chip.ready_for_power_down().unwrap();
    }

    #[test]
    fn the_pit_ticks_slow_on_every_64th_edge_and_never_with_pc1_high() {
        let mut chip = Chip::new(Vec::new());
        chip.pit_busy = 0;
        poke(&mut chip, reg::RTC_PITCTRLA, PIT_FAST).unwrap();
        chip.pit_busy = 0;
        ticks(&mut chip, 3).unwrap();
        assert_eq!(chip.ticks, 3);
        poke(&mut chip, reg::RTC_PITCTRLA, PIT_SLOW).unwrap();
        chip.pit_busy = 0;
        ticks(&mut chip, 63).unwrap();
        assert_eq!((chip.ticks, chip.slow_ticks, chip.grid), (4, 1, 66));
        ticks(&mut chip, 64).unwrap();
        assert_eq!((chip.ticks, chip.slow_ticks), (5, 2));
        poke(&mut chip, reg::RTC_PITCTRLA, PIT_FAST).unwrap();
        chip.pit_busy = 0;
        ticks(&mut chip, 1).unwrap();
        assert_eq!(chip.ticks, 6);
        let err = refused(&mut chip, reg::RTC_PITCTRLA, RTC_PITEN);
        assert!(err.contains("only models the PIT"), "{err}");
        let mut chip = lit();
        chip.pit_busy = 0;
        let err = refused(&mut chip, reg::RTC_PITCTRLA, PIT_SLOW);
        assert!(err.contains("slows the PIT with PC1 high"), "{err}");
    }

    #[test]
    fn pc1_rises_only_under_the_lit_watchdog() {
        let mut chip = dark();
        unlocked(&mut chip, reg::WDT_CTRLA, regs::WDT_PERIOD_8KCLK).unwrap();
        synced(&mut chip);
        let err = refused(&mut chip, reg::VPORTC_OUT, ENABLE);
        assert!(err.contains("WDT.CTRLA at 0x0b"), "{err}");
        chip.advance_to(chip.now + clocks(4)).unwrap();
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap();
        let err = refused(&mut chip, reg::VPORTC_OUT, ENABLE);
        assert!(err.contains("still syncing"), "{err}");
        synced(&mut chip);
        let err = refused(&mut chip, reg::VPORTC_OUT, ENABLE);
        assert!(err.contains("a wdr since"), "{err}");
        chip.wdr().unwrap();
        poke(&mut chip, reg::VPORTC_OUT, ENABLE).unwrap();
    }

    #[test]
    fn the_watchdog_takes_no_reserved_value_and_wdt_status_is_never_written() {
        for value in [WDT_PERIOD_MAX + 1, (WDT_PERIOD_MAX + 1) << 4 | 0x03] {
            let mut chip = Chip::new(Vec::new());
            let err = unlocked(&mut chip, reg::WDT_CTRLA, value).unwrap_err();
            assert!(err.contains("reserved"), "{err}");
        }
        let mut chip = Chip::new(Vec::new());
        let err = refused(&mut chip, reg::WDT_STATUS, WDT_LOCK);
        assert!(err.contains("sets WDT.STATUS.LOCK"), "{err}");
        let err = refused(&mut chip, reg::WDT_STATUS, 0);
        assert!(err.contains("only writable bit is LOCK"), "{err}");
    }

    #[test]
    fn the_watchdogs_periods_left_stop_one_short_of_the_bite() {
        let mut chip = Chip::new(Vec::new());
        assert_eq!(chip.wdt_periods_left(), DOWN_PERIODS, "off");
        unlocked(&mut chip, reg::WDT_CTRLA, regs::WDT_PERIOD_64CLK).unwrap();
        assert_eq!(chip.wdt_periods_left(), 3);
        ticks(&mut chip, 3).unwrap();
        assert_eq!(chip.wdt_periods_left(), 0);
        assert!(ticks(&mut chip, 1).is_err());
    }

    #[test]
    fn a_stopped_oscillator_stops_the_pit_and_the_watchdog() {
        let mut chip = watched();
        chip.io[reg::RTC_PITCTRLA] = RTC_PITEN;
        chip.osc32k_stopped = true;
        ticks(&mut chip, 100).unwrap();
        assert!(!chip.pit_flag);
        assert_eq!(chip.ticks, 0);
        chip.osc32k_stopped = false;
        ticks(&mut chip, 1).unwrap();
        assert!(chip.pit_flag);
        assert!(ticks(&mut chip, 1).unwrap_err().contains("without wdr"));
    }

    #[test]
    fn a_slow_oscillator_stretches_the_pits_period() {
        let mut chip = Chip::new(Vec::new());
        chip.io[reg::RTC_PITCTRLA] = RTC_PITEN;
        chip.tick_cycles = SLOWEST_TICK_CYCLES;
        ticks(&mut chip, 1).unwrap();
        assert_eq!(chip.ticks, 1);
        wait(&mut chip, SLOWEST_TICK_CYCLES - 1);
        assert_eq!(chip.ticks, 1);
        chip.elapse().unwrap();
        assert_eq!(chip.ticks, 2);
        assert_eq!(SLOWEST_TICK_CYCLES.div_ceil(TIMER_PERIOD), 24);
    }

    #[test]
    fn the_pits_setup_waits_for_ctrlbusy() {
        let mut chip = Chip::new(Vec::new());
        let err = refused(&mut chip, reg::RTC_PITCTRLA, PIT_FAST);
        assert!(err.contains("CTRLBUSY is set"), "{err}");
        while chip.read(reg::RTC_PITSTATUS as u16).unwrap() != 0 {}
        poke(&mut chip, reg::RTC_PITCTRLA, PIT_FAST).unwrap();
        assert!(poke(&mut chip, reg::RTC_PITCTRLA, PIT_FAST).is_err());
    }

    #[test]
    fn the_scan_passes_a_signed_image_and_fails_a_changed_one() {
        let signed = crc::sign(&[0x12, 0x34], &stamp::TEST).unwrap();
        let mut bad = signed.clone();
        bad[0] ^= 1;
        for (image, want) in [(signed, CRCSCAN_OK), (bad, 0)] {
            let mut chip = Chip::new(image);
            poke(&mut chip, reg::CRCSCAN_CTRLA, CRCSCAN_ENABLE).unwrap();
            assert_eq!(chip.read(reg::CRCSCAN_STATUS as u16), Ok(CRCSCAN_OK));
            wait(&mut chip, CRC_START_INSTRUCTIONS + 1);
            assert_eq!(chip.read(reg::CRCSCAN_STATUS as u16), Ok(CRCSCAN_BUSY));
            assert_eq!(chip.read(reg::CRCSCAN_STATUS as u16), Ok(CRCSCAN_BUSY));
            assert_eq!(chip.read(reg::CRCSCAN_STATUS as u16), Ok(want));
        }
    }

    #[test]
    fn only_the_whole_flash_scan_is_modelled() {
        let mut chip = Chip::new(Vec::new());
        let err = refused(
            &mut chip,
            reg::CRCSCAN_CTRLA,
            CRCSCAN_ENABLE | regs::CRCSCAN_NMIEN,
        );
        assert!(err.contains("only models ENABLE"), "{err}");
        poke(&mut chip, reg::CRCSCAN_CTRLB, regs::CRCSCAN_SRC_BOOT).unwrap();
        let err = refused(&mut chip, reg::CRCSCAN_CTRLA, CRCSCAN_ENABLE);
        assert!(err.contains("whole-flash"), "{err}");
    }

    #[test]
    fn fuses_read_back_and_never_take_a_write() {
        let mut chip = Chip::new(Vec::new());
        let syscfg0 = reg::FUSE_SYSCFG0 as u16;
        assert_eq!(chip.read(syscfg0), Ok(0xD0));
        assert!(
            chip.write(syscfg0, 0xD8)
                .unwrap_err()
                .contains("never writes")
        );
        let lock = map::LOCK;
        assert!(chip.write(lock, 0).unwrap_err().contains("never writes"));
    }

    #[test]
    fn the_signature_row_holds_this_torchs_calibration_low_byte_first() {
        let mut chip = Chip::new(Vec::new());
        let word = |chip: &mut Chip, a: usize| {
            let lo = chip.read(a as u16).unwrap();
            let hi = chip.read(a as u16 + 1).unwrap();
            u16::from_le_bytes([lo, hi])
        };
        assert_eq!(word(&mut chip, reg::SIGROW_TEMPSENSE0), 0x03EE);
        assert_eq!(word(&mut chip, reg::SIGROW_TEMPSENSE1), 0x0D24);
    }

    #[test]
    fn the_reset_flags_clear_by_writing_a_one() {
        let mut chip = Chip::new(Vec::new());
        assert_eq!(chip.read(reg::RSTCTRL_RSTFR as u16), Ok(RSTCTRL_PORF));
        poke(&mut chip, reg::RSTCTRL_RSTFR, RSTCTRL_PORF).unwrap();
        assert_eq!(chip.read(reg::RSTCTRL_RSTFR as u16), Ok(0));
    }

    #[test]
    fn a_protected_write_wants_the_key_and_the_clock_stays_at_4_mhz() {
        let a = reg::CLKCTRL_OSCHFCTRLA;
        let mut chip = Chip::new(Vec::new());
        assert!(refused(&mut chip, a, OSCHF_4MHZ).contains("CCP"));
        unlocked(&mut chip, a, OSCHF_4MHZ).unwrap();
        let err = unlocked(&mut chip, a, regs::CLKCTRL_FRQSEL_12M).unwrap_err();
        assert!(err.contains("4 MHz"), "{err}");
        let err = unlocked(&mut chip, a, OSCHF_4MHZ | regs::CLKCTRL_AUTOTUNE).unwrap_err();
        assert!(err.contains("4 MHz"), "{err}");
        let err = unlocked(&mut chip, reg::CLKCTRL_MCLKCTRLB, regs::CLKCTRL_PEN).unwrap_err();
        assert!(err.contains("undivided"), "{err}");
    }

    #[test]
    fn the_key_lasts_four_instructions_and_one_write() {
        let a = reg::CLKCTRL_MCLKCTRLB;
        let mut chip = Chip::new(Vec::new());
        poke(&mut chip, reg::CPU_CCP, CCP_IOREG).unwrap();
        assert!(chip.holds_interrupts());
        wait(&mut chip, 4);
        assert!(chip.holds_interrupts());
        poke(&mut chip, a, 0).unwrap();
        assert!(!chip.holds_interrupts(), "the write ends the hold");
        let err = refused(&mut chip, a, 0);
        assert!(err.contains("without the CCP key"), "spent: {err}");
        poke(&mut chip, reg::CPU_CCP, CCP_IOREG).unwrap();
        wait(&mut chip, 5);
        assert!(!chip.holds_interrupts());
        let err = refused(&mut chip, a, 0);
        assert!(err.contains("MCLKCTRLB without the CCP key"), "{err}");
        let err = refused(&mut chip, reg::WDT_CTRLA, 0x33);
        assert!(err.contains("WDT.CTRLA without the CCP key"), "{err}");
        let err = refused(&mut chip, reg::CPU_CCP, regs::CPU_CCP_SPM);
        assert!(err.contains("isn't the IOREG key"), "{err}");
    }

    #[test]
    fn a_chip_left_off_the_modelled_clock_or_timers_is_refused() {
        let chip = Chip::new(Vec::new());
        let err = chip.clocked_as_modelled().unwrap_err();
        assert!(err.contains("TCB0.CCMP is 0"), "{err}");
        let mut chip = dark();
        let err = chip.clocked_as_modelled().unwrap_err();
        assert!(err.contains("PIT isn't on"), "{err}");
        chip.io[reg::RTC_PITCTRLA] = PIT_FAST;
        chip.clocked_as_modelled().unwrap();
        chip.io[reg::CLKCTRL_MCLKCTRLB] = regs::CLKCTRL_PEN;
        let err = chip.clocked_as_modelled().unwrap_err();
        assert!(err.contains("4 MHz"), "{err}");
    }

    #[test]
    fn sleep_wants_sen_and_a_mode_the_model_has() {
        let mut chip = Chip::new(Vec::new());
        let err = chip.sleep().unwrap_err();
        assert!(err.contains("SEN clear"), "{err}");
        poke(
            &mut chip,
            reg::SLPCTRL_CTRLA,
            regs::SLPCTRL_SMODE_STDBY | SLPCTRL_SEN,
        )
        .unwrap();
        let err = chip.sleep().unwrap_err();
        assert!(err.contains("only models idle and power-down"), "{err}");
    }

    #[test]
    fn sleeping_with_no_interrupt_enabled_is_stopping_for_good() {
        let mut chip = dark();
        poke(&mut chip, reg::SLPCTRL_CTRLA, SLEEP_IDLE | SLPCTRL_SEN).unwrap();
        poke(&mut chip, SWITCH_CTRL, ISC_INTDISABLE).unwrap();
        assert_eq!(chip.sleep(), Ok(false));
        assert!(chip.down);
        poke(&mut chip, SWITCH_CTRL, ISC_BOTHEDGES).unwrap();
        assert_eq!(chip.sleep(), Ok(true));
        assert!(!chip.down);
    }

    #[test]
    fn a_register_it_doesnt_model_is_an_error() {
        let mut chip = Chip::new(Vec::new());
        let spi = reg::SPI0_CTRLA as u16;
        assert!(chip.write(spi, 0).unwrap_err().contains("doesn't model"));
        assert!(chip.read(spi).unwrap_err().contains("doesn't model"));
        let client = reg::TWI0_CTRLA as u16 + 0x09;
        assert!(chip.write(client, 0).unwrap_err().contains("doesn't model"));
        let mctrla = reg::TWI0_MCTRLA as u16;
        assert!(chip.read(mctrla).unwrap_err().contains("doesn't model"));
        assert!(chip.write(MAPPED, 0).unwrap_err().contains("flash"));
    }

    #[test]
    fn a_handler_cant_touch_a_peripheral_whose_temp_the_main_loop_is_using() {
        let mut chip = lit();
        poke(&mut chip, reg::DAC0_DATA, 0).unwrap();
        chip.interrupted = chip.temps_in_use();
        let err = refused(&mut chip, reg::DAC0_DATA, 0);
        assert!(err.contains("share its TEMP"), "{err}");
        poke(&mut chip, reg::TCB0_INTFLAGS, TCB_CAPT).unwrap();
        wide(&mut chip, reg::TCB0_CNT, 0).unwrap();
        chip.tcb.temp = None;
        chip.interrupted = TEMP_TCB0;
        let err = refused(&mut chip, reg::TCB0_CNT, 0);
        assert!(err.contains("TCB0's 16-bit"), "{err}");
    }

    use crate::avr::asm::{CLI, NOP, RETI, SEI, SLEEP, bytes as image, jmp, out, sts};

    const SEC: u16 = 0x9408;
    const PUSH_R0: u16 = 0x920F;
    /// `rjmp` to itself.
    const SPIN: u16 = 0xCFFF;

    const ST_X_R16: u16 = 0x930C;

    fn sim(words: impl IntoIterator<Item = u16>) -> Sim {
        let ends = Ends {
            unhandled: 0x1000,
            halt: 0x2000,
        };
        Sim::new(image(words), SRAM.start, ends).unwrap()
    }

    fn stores(words: Vec<u16>) -> Result<(), String> {
        let end = words.len() as u32;
        let mut sim = sim(words);
        sim.cpu.r[26] = reg::GPR_GPR0 as u8;
        while sim.cpu.pc < end {
            sim.step()?;
        }
        Ok(())
    }

    // Errata 2.2.3, GPR1 standing for anything under 64.
    #[test]
    fn a_store_under_64_straight_after_one_over_64_is_lost() {
        let high = sts(16, reg::RTC_PITINTFLAGS as u16);
        let low = sts(16, reg::GPR_GPR1 as u16);
        let sleep = sts(16, reg::SLPCTRL_CTRLA as u16);
        let lost = |words: Vec<u16>| stores(words).unwrap_err();
        let err = lost([high, low].concat());
        assert!(
            err.contains("GPR.GPR1 straight after a store to 64 or over"),
            "{err}"
        );
        let err = lost([&high[..], &[ST_X_R16]].concat());
        assert!(err.contains("GPR.GPR0 straight after"), "{err}");
        let err = lost([high, sleep].concat());
        assert!(err.contains("SLPCTRL.CTRLA straight after"), "{err}");
        stores([&high[..], &[NOP], &low[..]].concat()).unwrap();
        stores([&high[..], &[out(reg::GPR_GPR1 as u16, 16)]].concat()).unwrap();
        stores([&high[..], &[NOP], &sleep[..]].concat()).unwrap();
        stores([high, high].concat()).unwrap();
    }

    #[test]
    fn found_lit_wants_out_cleared_by_the_first_register_write() {
        let run = |writes: [u16; 2]| {
            let image = image([ldi(16, ENABLE), ldi(17, 0)].into_iter().chain(writes));
            found_lit(Sim::new(image, SRAM.start, Ends::default())?)
        };
        let dir = out(reg::VPORTC_DIR as u16, 16);
        let clear = out(reg::VPORTC_OUT as u16, 17);
        assert_eq!(run([clear, dir]), Ok(()));
        let err = run([dir, clear]).unwrap_err();
        assert!(err.contains("leaves PC1 set"), "{err}");
        assert!(run([ldi(16, 0), ldi(17, 0)]).is_err());
    }

    /// Vector 15's slot, as a byte address.
    const SLOT_15: u32 = 15 * VECTOR_WORDS * 2;

    fn forced(halt: u32) -> Sim {
        let mut words = vec![SEI];
        words.resize(SLOT_15 as usize / 2, NOP);
        words.extend([0x9210, reg::DAC0_DATA as u16]);
        words.extend([NOP]);
        words.extend([0x9210, reg::DAC0_DATA as u16 + 1]);
        words.extend([NOP; 4]);
        let ends = Ends {
            unhandled: SLOT_15,
            halt,
        };
        Sim::new(image(words), SRAM.start, ends).unwrap()
    }

    #[test]
    fn a_forced_interrupt_waits_for_sreg_i_and_the_instruction_after_sei() {
        let mut sim = forced(0x200);
        sim.chip.force_interrupt(15);
        sim.step().unwrap(); // sei
        assert_eq!(sim.cpu.handling, None, "SREG.I was clear");
        sim.step().unwrap(); // the nop after it
        assert_eq!(
            sim.cpu.handling, None,
            "the instruction after sei runs first"
        );
        let sp = sim.cpu.sp;
        sim.step().unwrap();
        assert_eq!(sim.cpu.handling, Some(15));
        assert_eq!(sim.cpu.pc * 2, SLOT_15);
        assert_eq!(sim.cpu.sp, sp - 2, "pushed PC");
        assert!(sim.cpu.flag(I), "left SREG.I alone");
        assert_eq!((sim.forced, sim.chip.forced), (Some(15), None));
        assert!(!sim.abandoned);
    }

    #[test]
    fn a_handler_in_halt_may_overwrite_temp_and_no_other_may() {
        let run = |halt: u32| {
            let mut sim = forced(halt);
            sim.chip.dac.temp = Some(0x40);
            sim.chip.force_interrupt(15);
            for _ in 0..3 {
                sim.step()?;
            }
            assert_eq!(sim.cpu.handling, Some(15));
            for _ in 0..3 {
                sim.step()?;
            }
            Ok::<_, String>(sim)
        };
        let err = run(0x200).err().unwrap();
        assert!(err.contains("TEMP"), "{err}");
        let sim = run(SLOT_15).unwrap();
        assert!(sim.abandoned);
        assert_eq!(sim.chip.dac.code, 0);
        assert_eq!(sim.chip.dac.temp, None);
    }

    fn statics() -> Layout {
        Layout {
            data: SRAM.start..SRAM.start + 3,
            bss: SRAM.start + 3..SRAM.start + 5,
            load: MAPPED,
        }
    }

    #[test]
    fn startup_has_to_copy_data_and_zero_bss() {
        let layout = statics();
        let mut chip = Chip::new(vec![1, 2, 3]);
        let err = check_startup(&chip, &layout).unwrap_err();
        assert!(err.contains(".data in SRAM doesn't match"), "{err}");
        for (at, value) in (layout.data.start..).zip([1, 2, 3]) {
            chip.write(at, value).unwrap();
        }
        chip.write(layout.bss.start, 0).unwrap();
        let err = check_startup(&chip, &layout).unwrap_err();
        assert_eq!(err, ".bss isn't zeroed: 0x7004 holds 0xa5");
        chip.write(layout.bss.start + 1, 0).unwrap();
        check_startup(&chip, &layout).unwrap();

        let past = Layout {
            load: MAPPED + 1,
            ..statics()
        };
        let err = check_startup(&chip, &past).unwrap_err();
        assert!(err.contains("outside flash"), "{err}");
    }

    const CALL: [u16; 2] = [0x940E, 0];

    fn loads(layout: &Layout) -> Vec<u16> {
        [
            (26, layout.data.start),
            (30, layout.load),
            (24, layout.data.end),
            (26, layout.bss.start),
            (24, layout.bss.end),
        ]
        .into_iter()
        .flat_map(|(r, value)| {
            let [lo, hi] = value.to_le_bytes();
            [ldi(r, lo), ldi(r + 1, hi)]
        })
        .collect()
    }

    #[test]
    fn patching_startup_wants_the_loads_it_knows_then_the_call() {
        let linked = statics();
        let moved = Layout {
            data: 0x7111..0x7116,
            bss: 0x7116..0x711D,
            load: MAPPED + 0x123,
        };
        let mut patched = image(loads(&linked).into_iter().chain(CALL));
        patch_startup(&mut patched, 0, &linked, &moved).unwrap();
        assert_eq!(patched, image(loads(&moved).into_iter().chain(CALL)));

        let mut swapped = loads(&linked);
        swapped.swap(0, 2);
        let mut swapped = image(swapped.into_iter().chain(CALL));
        let before = swapped.clone();
        let err = patch_startup(&mut swapped, 0, &linked, &moved).unwrap_err();
        assert!(err.contains("update `patch_startup`"), "{err}");
        assert_eq!(swapped, before, "nothing's patched");

        let mut no_call = image(loads(&linked));
        let err = patch_startup(&mut no_call, 0, &linked, &moved).unwrap_err();
        assert!(err.contains("no call to main"), "{err}");
    }

    #[test]
    fn an_image_bigger_than_flash_is_refused() {
        let ends = Ends::default();
        Sim::new(vec![0; FLASH_SIZE], SRAM.start, ends)
            .map(drop)
            .unwrap();
        let err = Sim::new(vec![0; FLASH_SIZE + 1], SRAM.start, ends)
            .err()
            .unwrap();
        assert_eq!(err, "32769 bytes won't fit in flash");
    }

    #[test]
    fn a_push_into_bss_is_refused() {
        // .bss ending a byte short of the top leaves room for one push.
        let run = |bss_end: u16| {
            let mut sim = Sim::new(image([PUSH_R0, PUSH_R0]), bss_end, Ends::default())?;
            sim.step()?;
            assert_eq!(sim.stack(), 1);
            sim.step()
        };
        assert_eq!(run(SRAM.end - 2), Ok(()));
        let err = run(SRAM.end - 1).unwrap_err();
        assert_eq!(err, "0x0002: the stack ran into .bss");
    }

    #[test]
    fn enabling_an_interrupt_with_no_handler_is_refused_once_sreg_i_is_set() {
        let mut sim = sim([NOP, SEI, NOP]);
        poke(&mut sim.chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        sim.step().unwrap();
        let err = sim.step().unwrap_err();
        assert!(
            err.contains("enables RTC_PIT (vector 6) with SREG.I set"),
            "{err}"
        );
    }

    fn handled_by(vector: u8, handler: &[u16]) -> Vec<u16> {
        let slot = usize::from(vector) * VECTOR_WORDS as usize;
        let after = VECTORS * VECTOR_WORDS as usize;
        let mut words = vec![NOP; after];
        words[0] = SEI;
        words[slot..slot + 2].copy_from_slice(&jmp(after as u32 * 2));
        words.extend_from_slice(handler);
        words
    }

    fn handle(body: &[u16]) -> Result<Sim, String> {
        let handler: Vec<u16> = body.iter().copied().chain([RETI]).collect();
        let mut sim = sim(handled_by(VECTOR_PORTD, &handler));
        poke(&mut sim.chip, SWITCH_CTRL, ISC_BOTHEDGES)?;
        sim.chip.portd_flags = SWITCH;
        while sim.cpu.handling.is_none() {
            sim.step()?;
        }
        while sim.cpu.handling.is_some() {
            sim.step()?;
        }
        Ok(sim)
    }

    #[test]
    fn a_handler_gives_back_every_register_sreg_and_sp() {
        let sim = handle(&[NOP, NOP]).map_err(drop).unwrap();
        assert_eq!(sim.longest_handler, 3, "the jmp and the two nops");
        for (body, why) in [
            (ldi(16, 1), "r16 at 0x01, not the 0x00 it found"),
            (SEC, "SREG at 0x81, not the 0x80 it found"),
            (PUSH_R0, "SP at 0x7ffe, having found it at 0x7fff"),
        ] {
            let err = handle(&[body]).err().unwrap();
            assert_eq!(
                err,
                format!("the handler for PORTD_PORT (vector 24): returns with {why}")
            );
        }
    }

    #[test]
    fn a_handler_has_to_fit_in_a_tcb0_period() {
        let err = handle(&[SPIN]).err().unwrap();
        assert!(err.contains("ran past 997 instructions"), "{err}");
    }

    #[test]
    fn taking_an_interrupt_into_unhandled_interrupt_is_refused() {
        let words = handled_by(VECTOR_RTC_PIT, &[RETI]);
        let ends = Ends {
            unhandled: u32::from(VECTOR_RTC_PIT) * VECTOR_WORDS * 2,
            halt: 0x2000,
        };
        let mut sim = Sim::new(image(words), SRAM.start, ends).unwrap();
        poke(&mut sim.chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        sim.chip.pit_flag = true;
        sim.step().unwrap();
        sim.step().unwrap();
        let err = sim.step().unwrap_err();
        assert_eq!(
            err,
            "takes RTC_PIT (vector 6), whose vector goes to __unhandled_interrupt"
        );
    }

    #[test]
    fn code_that_never_gets_there_runs_out_of_budget() {
        let err = sim([SPIN]).run_to(5, 10).unwrap_err();
        assert_eq!(err, "didn't reach 0x000a within 10 instructions");
        let err = sim([SPIN]).tick(false).unwrap_err();
        assert!(err.contains("ran past 15625 instructions"), "{err}");
        let err = found_lit(sim([SPIN])).unwrap_err();
        assert!(err.contains("wrote no register"), "{err}");
        let err = boot_order(sim([SPIN])).unwrap_err();
        assert!(err.contains("didn't wait for a tick"), "{err}");
    }

    fn stop_for_good(first: u16) -> Result<u64, String> {
        let mut sim = sim([first, SLEEP]);
        sim.chip.io[reg::SLPCTRL_CTRLA] = SLEEP_IDLE | SLPCTRL_SEN;
        sim.tick(false)
    }

    #[test]
    fn a_pass_that_stops_for_good_fails() {
        let err = stop_for_good(CLI).unwrap_err();
        assert!(err.contains("stopped for good"), "{err}");
    }

    #[test]
    fn stopping_for_good_wants_sreg_i_clear_too() {
        let err = stop_for_good(SEI).unwrap_err();
        assert!(err.contains("with SREG.I still set"), "{err}");
    }

    #[test]
    fn start_up_is_judged_at_the_first_wait_for_a_tick() {
        let mut sim = sim([SLEEP]);
        sim.chip.io[reg::SLPCTRL_CTRLA] = SLEEP_IDLE | SLPCTRL_SEN;
        let err = boot_order(sim).unwrap_err();
        assert_eq!(err, "never cleared RSTCTRL.RSTFR");
    }

    const IN_ORDER: Boot = Boot {
        flags_cleared: Some(1),
        flash_checked: Some(2),
        fuses_read: Some(3),
        supply_read: Some(4),
        set_up: Some(5),
        pit_on: Some(6),
        watchdog_on: Some(7),
    };

    #[test]
    fn start_up_has_to_do_every_step() {
        IN_ORDER.in_order().unwrap();
        let missing = |boot: Boot| boot.in_order().unwrap_err();
        for (boot, what) in [
            (
                Boot {
                    flags_cleared: None,
                    ..IN_ORDER
                },
                "never cleared RSTCTRL.RSTFR",
            ),
            (
                Boot {
                    flash_checked: None,
                    ..IN_ORDER
                },
                "never started a CRC scan",
            ),
            (
                Boot {
                    fuses_read: None,
                    ..IN_ORDER
                },
                "never read the fuses",
            ),
            (
                Boot {
                    supply_read: None,
                    ..IN_ORDER
                },
                "never read MVIO.STATUS",
            ),
            (
                Boot {
                    set_up: None,
                    ..IN_ORDER
                },
                "never set anything up",
            ),
            (
                Boot {
                    pit_on: None,
                    ..IN_ORDER
                },
                "never turned the PIT on",
            ),
            (
                Boot {
                    watchdog_on: None,
                    ..IN_ORDER
                },
                "never turned the watchdog on",
            ),
        ] {
            assert_eq!(missing(boot), what);
        }
    }

    #[test]
    fn start_up_has_to_be_in_order() {
        for (boot, why) in [
            (
                Boot {
                    flags_cleared: Some(8),
                    ..IN_ORDER
                },
                "before clearing the reset flags",
            ),
            (
                Boot {
                    flash_checked: Some(6),
                    ..IN_ORDER
                },
                "before the CRC scan",
            ),
            (
                Boot {
                    fuses_read: Some(6),
                    ..IN_ORDER
                },
                "before reading the fuses",
            ),
            (
                Boot {
                    supply_read: Some(6),
                    ..IN_ORDER
                },
                "before checking VDDIO2",
            ),
            (
                Boot {
                    pit_on: Some(8),
                    ..IN_ORDER
                },
                "before the PIT",
            ),
        ] {
            let err = boot.in_order().unwrap_err();
            assert!(err.contains(why), "{err}");
        }
    }

    #[test]
    fn the_chip_notes_each_step_of_start_up_as_it_happens() {
        let mut chip = Chip::new(Vec::new());
        let step = |chip: &mut Chip| {
            chip.elapse().unwrap();
            chip.now
        };
        poke(&mut chip, reg::VPORTC_OUT, 0).unwrap();
        poke(&mut chip, reg::VPORTC_DIR, ENABLE).unwrap();
        let flags = step(&mut chip);
        poke(&mut chip, reg::RSTCTRL_RSTFR, RSTCTRL_PORF).unwrap();
        let flash = step(&mut chip);
        poke(&mut chip, reg::CRCSCAN_CTRLB, CRCSCAN_SRC_FLASH).unwrap();
        poke(&mut chip, reg::CRCSCAN_CTRLA, CRCSCAN_ENABLE).unwrap();
        let fuses = step(&mut chip);
        chip.read(reg::FUSE_OSCCFG as u16).unwrap();
        let supply = step(&mut chip);
        chip.read(reg::MVIO_STATUS as u16).unwrap();
        assert_eq!(chip.boot.set_up, None);
        let set_up = step(&mut chip);
        poke(&mut chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        chip.pit_busy = 0;
        let pit = step(&mut chip);
        poke(&mut chip, reg::RTC_PITCTRLA, PIT_FAST).unwrap();
        let watchdog = step(&mut chip);
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_LIT).unwrap();

        let boot = &chip.boot;
        assert_eq!(boot.flags_cleared, Some(flags));
        assert_eq!(boot.flash_checked, Some(flash));
        assert_eq!(boot.fuses_read, Some(fuses));
        assert_eq!(boot.supply_read, Some(supply));
        assert_eq!(boot.set_up, Some(set_up));
        assert_eq!(boot.pit_on, Some(pit));
        assert_eq!(boot.watchdog_on, Some(watchdog));
        boot.in_order().unwrap();
    }

    /// The least a firmware can be and still tick.
    fn ticking() -> Sim {
        let handler = sts(16, reg::RTC_PITINTFLAGS as u16)
            .into_iter()
            .chain([RETI]);
        let mut words = handled_by(VECTOR_RTC_PIT, &handler.collect::<Vec<_>>());
        // `lds r17, VPORTD.IN`, `sleep`, then back to the `lds`.
        words[1] = 0x9110;
        words[2] = reg::VPORTD_IN as u16;
        words[3] = SLEEP;
        words[4] = SPIN - 3;
        let mut sim = sim(words);
        sim.cpu.r[16] = RTC_PI;
        let mut chip = dark();
        poke(&mut chip, SWITCH_CTRL, ISC_INTDISABLE).unwrap();
        poke(&mut chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        chip.pit_busy = 0;
        poke(&mut chip, reg::RTC_PITCTRLA, PIT_FAST).unwrap();
        poke(&mut chip, reg::SLPCTRL_CTRLA, SLEEP_IDLE | SLPCTRL_SEN).unwrap();
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_PERIOD_MAX).unwrap();
        synced(&mut chip);
        chip.flash = std::mem::take(&mut sim.chip.flash);
        chip.aux_unwired = true;
        sim.chip = chip;
        sim
    }

    fn quiet(ticks: usize) -> impl Iterator<Item = Sample> {
        iter::repeat_n(
            Sample {
                pressed: false,
                scene: Scene::NORMAL,
            },
            ticks,
        )
    }

    fn in_step_fails(sim: &mut Sim, ticks: usize) -> String {
        in_step(sim, quiet(ticks), &mut Report::default()).unwrap_err()
    }

    #[test]
    fn in_step_wants_the_clock_the_timers_and_the_watchdog_set_up_by_the_first_tick() {
        let err = in_step_fails(&mut sim([SPIN]), 1);
        assert!(err.starts_with("hw::init: a pass of the main"), "{err}");

        let mut sim = ticking();
        sim.chip.tcb.ccmp = 0;
        let err = in_step_fails(&mut sim, 1);
        assert!(err.starts_with("hw::init: TCB0.CCMP is 0"), "{err}");

        let mut sim = ticking();
        sim.chip.io[reg::WDT_CTRLA] = 0;
        let err = in_step_fails(&mut sim, 1);
        assert_eq!(err, "hw::start_watchdog never turned the watchdog on");
    }

    #[test]
    fn in_step_holds_the_sleep_to_the_librarys() {
        let err = in_step_fails(&mut ticking(), 8);
        assert!(
            err.contains("the library says power down, but the firmware idled"),
            "{err}"
        );
    }

    #[test]
    fn in_step_holds_the_driver_to_the_librarys() {
        let mut sim = ticking();
        poke(&mut sim.chip, reg::VREF_DAC0REF, VREF_REFSEL_2V500).unwrap();
        let err = in_step_fails(&mut sim, 8);
        assert!(
            err.contains("PC1 is low, DAC0 0, VREF.DAC0REF 0x03"),
            "{err}"
        );
    }

    #[test]
    fn a_firmware_that_never_feeds_the_watchdog_is_bitten() {
        let mut sim = ticking();
        // 64 clocks, four ticks, the fourth being the bite.
        sim.chip.io[reg::WDT_CTRLA] = regs::WDT_PERIOD_64CLK;
        for _ in 0..3 {
            sim.tick(false).unwrap();
        }
        let err = sim.tick(false).unwrap_err();
        assert!(err.starts_with("asleep: the watchdog went 6"), "{err}");
    }

    #[test]
    fn a_failed_run_says_which_it_was_and_whether_interrupts_were_landing() {
        let power_on = || Ok(ticking());
        let failed = "the library says power down, but the firmware idled";
        let err = run(&power_on, None, Landing::Watch).err().unwrap();
        assert_eq!(err, format!("random switching: tick 2: {failed}"));
        let err = run(&power_on, None, Landing::Until(5)).err().unwrap();
        assert_eq!(
            err,
            format!("with interrupts mid-pass: random switching: tick 2: {failed}")
        );

        let example = &EXAMPLES[0];
        let err = run(&power_on, Some(example), Landing::Watch).err().unwrap();
        assert!(
            err.starts_with(&format!("{}: tick ", example.name)),
            "{err}"
        );
    }

    // The forced vector has to be one the firmware leaves unhandled.
    #[test]
    fn the_forced_interrupt_has_to_be_one_with_no_handler() {
        let err = unhandled_interrupt(sim(handled_by(VECTOR_UNHANDLED, &[RETI]))).unwrap_err();
        assert_eq!(
            err,
            "TCB1_INT (vector 15) has a handler now, so pick another vector to force"
        );
    }
}
