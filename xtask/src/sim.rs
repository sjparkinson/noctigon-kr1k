//! A fake AVR32DD20 around `avr.rs`'s CPU, wired up like the KR1K's Lume
//! X1-C driver, and the checks that run the real firmware on it. `build()` in
//! main.rs runs them after `layout.rs`'s, so `hex` and `flash` refuse
//! firmware that fails.
//!
//! This finds mistakes, it doesn't prove anything. Everything here is one
//! reading of the datasheet, run against code written from that same reading,
//! and nothing has been compared with a real chip. An earlier torch had a
//! simulator too, and it didn't save it. The likely cause is the 12 V
//! programmer or a slipped probe, not proven, and no model of the chip can
//! see either. A pass means the firmware does what this file thinks it
//! should.
//!
//! Section numbers are DS40002413B, the AVR32/16DD14/20 datasheet, and the
//! errata is DS80001026D. It models only the registers the firmware touches,
//! and turns the rules the real chip enforces quietly into errors. Touching a
//! register it doesn't model is an error too, so new peripheral code fails
//! here until the model learns about it.
//!
//! # The chip
//!
//! - A protected register only takes a write within four instructions of the
//!   CCP key. Interrupts wait out those four instructions, or until the first
//!   store or `sleep` (§7.6.1). The protected registers the firmware writes
//!   are CLKCTRL.OSCHFCTRLA and MCLKCTRLB (§12) and WDT.CTRLA (table 22-1).
//! - The main clock is the internal oscillator at 4 MHz with no prescaler,
//!   which is the reset value of both (§12.5.7, §12.5.2), and anything else is
//!   refused, since every timing here assumes it.
//! - 16-bit registers go low byte then high byte through a TEMP byte, one per
//!   peripheral (§8.10.3). TCB0's CNT and CCMP, DAC0's DATA and ADC0's RES
//!   are the ones the firmware uses. A handler that lands with the main loop
//!   halfway through one can't touch that peripheral's 16-bit registers.
//! - Errata 2.2.3: a store with `st`, `std` or `sts` to 64 or over, followed
//!   straight away by a store under 64 or to SLPCTRL.CTRLA that isn't an
//!   `out`, loses the second store, so it's an error (`avr.rs` has the
//!   reading).
//! - DAC0 converts DATA's top ten bits against VREF.DAC0REF, which this
//!   firmware sets to 1.024 V or 2.5 V, and its output follows the high
//!   byte's write (§34.5.2). Its output buffer wants PD6's input buffer off
//!   (§34.3.1). Power-down turns it off (§34.3.3).
//! - TWI0's host and the AW2016 on its bus are `twi.rs`'s, which has the
//!   rules. The host stops in every sleep mode (§29.3.5), so `sleep` with it
//!   on is refused, idle or power-down.
//! - TCB0 runs in periodic interrupt mode off the main clock, raising CAPT
//!   when CNT reaches CCMP and starting again from 0 (§24.3.3.1.1). That's
//!   taken as CCMP + 1 clocks a period; the datasheet doesn't give the
//!   count. It halts in power-down (§24.3.6). CAPT goes up with the
//!   interrupt off too, and the request is the flag and the enable together
//!   (§24.3.5), so turning the interrupt on wants the flag not to have sat
//!   through a period uncleared.
//! - ADC0 only converts as one of the two measurements the firmware makes,
//!   VDD / 10 against 1.024 V or the temperature sensor against 2.048 V, with
//!   four 12-bit samples accumulated and a clock between 125 kHz and 2 MHz
//!   (table 38-25). The temperature sensor wants 25 µs of INITDLY and 28 µs
//!   of SAMPLEN (§33.3.3.8). Reading RES early, or its high byte first,
//!   starting mid-conversion or changing its setup mid-conversion are errors,
//!   and the first conversion after a change of input reads 0. Reading RES
//!   clears RESRDY (§33.5.12). Power-down wants no conversion running and
//!   the ADC off (§33.3.8).
//! - RTC.PITCTRLA isn't written while PITSTATUS.CTRLBUSY is set (§26.13.12).
//!   The PIT's flag goes up once a period of simulated time, in every sleep
//!   mode (§26.9). A run can stretch the period to the slowest the two
//!   oscillators' limits allow, or stop the 32 kHz oscillator, which stops
//!   the PIT and the watchdog together.
//! - Every port pin senses asynchronously, so any sense configured in ISC
//!   wakes the chip from power-down (§18.3.3.1, table 13-4), and so does the
//!   PIT. TCB0 doesn't, since it's halted. A switch edge raises PD4's flag
//!   when its ISC asks for that edge (§18.3.3).
//! - Enabling an interrupt with SREG.I set and its vector going to
//!   `__unhandled_interrupt` is an error, because the real chip would return
//!   with the flag still up, forever. So is taking one, except in the run
//!   that forces one on purpose (`Chip::force_interrupt`). A handler that
//!   reaches `hw::halt` never returns, so from there it's held to the main
//!   loop's rules, not a handler's (`Sim::abandon`).
//! - A handler gives back every register, SREG and SP as it found them, and
//!   has to fit in a TCB0 period even if every instruction took four clocks.
//! - `sleep` needs SLPCTRL.SEN, idle or power-down, and an enabled interrupt
//!   that could wake it. With none at all it's the chip stopping for good
//!   (§13.3.1), which is `hw::halt`, and it wants SREG.I clear as well
//!   (`avr.rs`).
//! - The watchdog's WDT.CTRLA takes the CCP key and isn't written while
//!   SYNCBUSY is set (§22.3.6), and nothing may change it once it's on. A
//!   `wdr` in the closed window, or on its edge, fails, and so does running
//!   out the open window, as the real chip would reset either way (§22.3.3).
//!   Nothing here resets the chip, so a bite is the firmware failing.
//! - CRCSCAN runs CRC-16-CCITT over all of flash and sets STATUS.OK if it
//!   comes to 0, which is what a correct checksum in the last two bytes does
//!   (§30.3.2, `crc.rs`). The firmware runs on the image `cargo xtask hex`
//!   writes. STATUS.OK reads 1 from reset (§30.5.3), and the scan starts
//!   three cycles after ENABLE (§30.3.1).
//! - The fuses read as the world's, and writing any of them is an error.
//!   SIGROW.TEMPSENSE0 and 1 hold `world::CALIBRATION`. MVIO.STATUS.VDDIO2S
//!   reads 1 with single supply (§19.3.1), and otherwise says whether the run
//!   has VDDIO2. When VDDIO2 comes back the MVIO pins reload PORT
//!   (§19.3.2.1), so PC1 rises then if PORTC has it set, and that's held to
//!   every rule a rise is. The datasheet gives no VDDIO2S threshold and no
//!   delay either way, so both are taken as at once.
//!
//! # The board
//!
//! None of these have a section number. They're the board's, from what the
//! stock firmware does with it and docs/safety.md, and they hold the firmware
//! to them.
//!
//! - PC1, the boost converter and amplifier enable, only rises with DAC0 on
//!   and at 0, its reference at 1.024 V, and every power path low. It only
//!   falls with DAC0 at 0 and its reference back at 1.024 V, which is the
//!   stock firmware's off order. It's low at the first register write after
//!   a start that finds it high, and low in power-down.
//! - DAC0 only goes above 0 with PC1 high, at least 8 ms after it rose, which
//!   is the stock firmware's wait for the boost converter (the only figure
//!   with any history on this board), and at least 2 µs after its reference
//!   last changed (table 38-18's tINTREF, typical).
//! - With PC1 high, the reference and the power paths only change with
//!   DAC0 at 0. How the paths combine on this board is unknown
//!   (docs/safety.md, H3), so no change of either is taken as safe with
//!   DAC0 live. A reference raised to 2.5 V or a path added also wants
//!   DAC0's output to have had time to fall to 0. That time isn't known for
//!   this board. Table 38-23 has 7 µs to 10 µs for a full-scale rise into
//!   50 pF, and the DAC can sink only 0.001 mA, so how fast PD6 falls depends
//!   on what the board pulls it down with. This takes the 10 µs, which is a
//!   floor, not a promise. A reference lowered or a path removed, as the off
//!   order does straight after DAC0 goes to 0, isn't held to the wait.
//! - PC1, PD4 (the switch, with its own pull-up), PD6 (DAC0's pin), PA1
//!   and the three paths never get a pull-up. No pin gets INVEN. PA1's
//!   `OUT` is never set, since the stock build on this torch holds it as an
//!   output at 0 and what it connects to is unknown (src/board.rs). PA2 and PA3, the
//!   AW2016's TWI lines, are never driven through PORT and their settings
//!   never written, since TWI0 takes them over, and nothing in PORTF (reset
//!   and UPDI) is ever written.
//! - The aux are never lit while PC1 is high. An AW2016 write that lights
//!   one with PC1 high is refused, and so is PC1 rising with one lit.
//! - Without VDDIO2, PC1 is tri-stated, and what the driver's enable does
//!   then isn't modelled. A pass that took its tick with VDDIO2S already
//!   clear leaves PC1 clear in PORTC, DAC0 at 0 against 1.024 V and the
//!   paths low before it sleeps (`Chip::dark_without_vddio2`).
//! - TCB0, the tick deadline's timer, runs exactly while PC1 is high,
//!   checked at every `sleep`. Power-down wants PC1 low and TCB0 stopped.
//! - With the light on and no tick, `hw::wait_for_tick` gives up after
//!   `watchdog::TICK_DEADLINE_PERIODS` TCB0 periods and `main` halts.
//!
//! Time counts instructions, not cycles. A real instruction takes at least
//! one clock, so a wait the model measures is never longer than the real
//! one, and a minimum time here is a conservative check. The budgets go the
//! other way and take every instruction as the slowest, four clocks.
//!
//! # The runs
//!
//! 1. Reset to `main` with SRAM full of junk, checking .data and .bss come out
//!    right, then the same startup code with them moved and given contents.
//! 2. The reset vector again with PC1 already an output and high, as far as
//!    the first register write, which has to clear it (`found_lit`). Then
//!    again as far as the first wait for a tick, checking `main` starts up in
//!    the order it has to (`boot_order`). Then lit, with an interrupt that
//!    has no handler forced, which has to end in `hw::halt` with the light
//!    off (`unhandled_interrupt`).
//! 3. Two seconds lit with every tick at its slowest, which mustn't trip
//!    `hw::wait_for_tick`'s deadline (`slow_ticks`).
//! 4. The 32 kHz oscillator stopping with the light on. PC1 has to drop within
//!    the deadline, and the chip then stop for good in power-down
//!    (`oscillator_stops`). A tick lost with the oscillator still running ends
//!    in a watchdog reset instead, which can't run here.
//! 5. An ADC that never finishes a conversion, from a click to ten seconds
//!    later, when the light has to be off (`adc_never_answers`). One stuck
//!    on a good cell's reading for every input, ground included, when the
//!    light has to fade out as a fault (`adc_stuck`). The cell and VDDIO2
//!    read with the light off, which have to land in SRAM where `cargo xtask
//!    peek` reads them, and stay put while it's lit (`supplies_kept`). The cell
//!    falling under the hard floor with the light at the top of the ramp, on
//!    each tick of the ADC's round, when PC1 has to be low within 12 ticks
//!    (`cell_collapses`). Twenty thousand ticks with the switch held down
//!    from the first (`switch_stuck`). VDDIO2 missing, so the light never
//!    comes on (`vddio2_missing`), and VDDIO2 going at the top of the ramp,
//!    straight after the firmware's look, so the light has to be off a tick
//!    and a pass later and stay off (`vddio2_drops`). The AW2016 never
//!    answering, SDA held low and SCL
//!    held low (`bad_bus`).
//! 6. Every example in docs/ui.md (see `spec.rs`), each on a fresh chip.
//! 7. Twenty thousand ticks of random clicks and holds.
//! 8. Both of those again with a switch bounce, and a TCB0 period ending
//!    while the light's on, landing on every instruction the main loop
//!    reaches (`Sim::land`), each as far as the first run kept reaching new
//!    code, and a little past (`both_ways`).
//!
//! All but the first two drive `kr1k::torch` alongside the firmware and check
//! PC1, DAC0, its reference and the paths show the library's output on every
//! tick, the fourth until the oscillator stops. So does the last part of the
//! second, until the forced interrupt. Those runs hold the AW2016 to the
//! library's `torch.aux()` too, at the end of every pass (`AuxWatch`): the
//! colour through the board's channel map, dim or bright, never a wrong one,
//! dark for at most two ticks while it wakes, and in standby whenever the aux
//! are off. Three more runs put a fault on the bus, the AW2016 never
//! answering, SDA held low and SCL held low (`bad_bus`), and the firmware has
//! to give each update up without hanging or retrying while the light still
//! follows the library.
//! `probe` separately runs examples/link.rs, which has real statics,
//! `guards` runs examples/guards.rs, which asks src/avr/hw.rs for power-down
//! with the light on, and `first_light` runs examples/first_light.rs, the
//! first-light image, with no ticks at all, holding it to its promises of no
//! interrupt, no sleep, no watchdog and no timer (`Chip::bare`) as well as
//! the board's rules above.

use std::iter;
use std::ops::Range;
use std::thread;

use kr1k::TICKS_PER_SECOND;
use kr1k::battery::COLLAPSE_BELOW;
use kr1k::board;
use kr1k::ramp::{CEILING, Output, Vref};
use kr1k::sensors::{CYCLE_TICKS, Channel, SUPPLIES_AT, Supplies};
use kr1k::testing::{self, Action, Example, Rig, Sample, Scene};
use kr1k::ui::Aux;
use kr1k::watchdog::{CLOCK_HZ, FEED_TICKS, Feeder, TICK_DEADLINE_PERIODS, TIMER_PERIOD_CLOCKS};

use crate::avr::{Bus, Cpu, I, IO_LOW_END, VECTOR_WORDS, is_two_words, ldi};
use crate::crc;
use crate::elf::{DATA_SPACE, Elf, SRAM};
use crate::layout::{self, jmp_target, word};
use crate::spec;
use crate::stamp;
use crate::switching::RandomSwitching;
use crate::twi::{Fault, Twi};
use crate::updi::map;
use crate::world::{CALIBRATION, FUSE_NAMES, World};

// The registers the chip models, as data-space addresses, and their bits.
// They're generated from the ATDF (src/regs.rs), so they only move when the
// ATDF does, never with the firmware. Only the ones the ATDF names go here,
// since anything hand-written in src/regs.rs is the firmware's choice.
use kr1k::regs::{
    self, ADC_ENABLE, ADC_INITDLY, ADC_MUXPOS_GND, ADC_MUXPOS_TEMPSENSE, ADC_MUXPOS_VDDDIV10,
    ADC_MUXPOS_VDDIO2DIV10, ADC_PRESC, ADC_RESRDY, ADC_RESSEL_12BIT, ADC_SAMPDLY, ADC_SAMPLEN,
    ADC_SAMPNUM, ADC_SAMPNUM_ACC4, ADC_STCONV, CRCSCAN_BUSY, CRCSCAN_ENABLE, CRCSCAN_OK,
    CRCSCAN_SRC_FLASH, DAC_ENABLE, DAC_OUTEN, MVIO_VDDIO2S, PORT_INVEN, PORT_ISC, PORT_PULLUPEN,
    RSTCTRL_PORF, RTC_CTRLBUSY, RTC_PI, RTC_PITEN, SLPCTRL_SEN, TCB_CAPT, TCB_ENABLE,
    VREF_REFSEL_1V024, VREF_REFSEL_2V048, VREF_REFSEL_2V500, WDT_LOCK, WDT_PERIOD, WDT_SYNCBUSY,
    WDT_WINDOW, addr as reg,
};

const CCP_IOREG: u8 = regs::CPU_CCP_IOREG;
/// CLKCTRL.OSCHFCTRLA at 4 MHz, with AUTOTUNE and RUNSTDBY off, which is
/// its reset value (§12.5.7).
const OSCHF_4MHZ: u8 = regs::CLKCTRL_FRQSEL_4M;
const ISC_INTDISABLE: u8 = regs::PORT_ISC_INTDISABLE;
const ISC_BOTHEDGES: u8 = regs::PORT_ISC_BOTHEDGES;
const ISC_RISING: u8 = regs::PORT_ISC_RISING;
const ISC_FALLING: u8 = regs::PORT_ISC_FALLING;
const ISC_INPUT_DISABLE: u8 = regs::PORT_ISC_INPUT_DISABLE;
const ISC_LEVEL: u8 = regs::PORT_ISC_LEVEL;
const SLEEP_IDLE: u8 = regs::SLPCTRL_SMODE_IDLE;
const SLEEP_POWER_DOWN: u8 = regs::SLPCTRL_SMODE_PDOWN;
/// TCB0.CTRLA running off the main clock, undivided.
const TCB_RUNNING: u8 = regs::TCB_CLKSEL_DIV1 | TCB_ENABLE;
/// ADC0.CTRLA as the firmware converts: enabled, single-ended, 12-bit, right
/// adjusted, one conversion per trigger.
const ADC_ON: u8 = ADC_ENABLE | ADC_RESSEL_12BIT | regs::ADC_CONVMODE_SINGLEENDED;
/// DAC0.CTRLA on with its output buffer driving PD6.
const DAC_DRIVING: u8 = DAC_ENABLE | DAC_OUTEN;
/// How far DAC0's 10-bit code sits up DATA (§34.5.2).
const DAC_SHIFT: u32 = regs::DAC_DATA.trailing_zeros();

const VECTOR_RTC_PIT: u8 = regs::VECTOR_RTC_PIT as u8;
const VECTOR_TCB0: u8 = regs::VECTOR_TCB0_INT as u8;
const VECTOR_PORTA: u8 = regs::VECTOR_PORTA_PORT as u8;
const VECTOR_PORTC: u8 = regs::VECTOR_PORTC_PORT as u8;
const VECTOR_PORTD: u8 = regs::VECTOR_PORTD_PORT as u8;
/// Reset plus every interrupt.
const VECTORS: usize = regs::VECTORS;
const _: () = assert!(VECTORS <= 64);

/// WDT.CTRLA.PERIOD = 8KCLK, the longest. Anything over is reserved.
const WDT_PERIOD_MAX: u8 = regs::WDT_PERIOD_8KCLK;

/// CPU cycles a second, from `hw::init`'s clock setup.
const CLK_PER: u32 = 4_000_000;
// The library sizes the tick deadline for the same clock, and
// `clocked_as_modelled` holds the firmware to it.
const _: () = assert!(CLOCK_HZ == CLK_PER);
/// Main clocks in TCB0's period, as the library counts on.
const TIMER_PERIOD: u64 = TIMER_PERIOD_CLOCKS as u64;
/// Reads of WDT.STATUS that report SYNCBUSY after a write to CTRLA, like
/// `PIT_SYNC_READS`.
const WDT_SYNC_READS: u8 = 2;
/// WDT clocks in a tick. The PIT divides the same 32 kHz oscillator by 512,
/// and the WDT runs at 1.024 kHz (§22.3.2).
const WDT_CLOCKS_PER_TICK: u32 = 16;
/// The scan starts three cycles after ENABLE, and the CPU keeps running until
/// then (§30.3.1). No instruction is quicker than a cycle, so at most this many
/// instructions see STATUS from before the scan.
const CRC_START_INSTRUCTIONS: u64 = 3;
/// Reads of STATUS that show BUSY once the scan's started. The scan stalls
/// the CPU until it's done (§30.3.2), so a real chip may never show it; this
/// makes sure the firmware waits if it does.
const CRC_BUSY_READS: u8 = 2;
/// What the first conversion after a change of input reads. The datasheet
/// doesn't promise it's wrong, but table 38-25 gives the temperature sensor
/// 40 µs to settle after MUXPOS changes, and the stock firmware finds it
/// unstable, so the model makes it as wrong as it gets.
const UNSETTLED: u16 = 0;

/// Where flash appears in data space, and how much of it there is. On this
/// part all of flash fits the window, which runs to the top of data space.
const MAPPED: u16 = map::FLASH;
const FLASH_SIZE: usize = map::FLASH_SIZE;
const _: () = assert!(MAPPED as usize + FLASH_SIZE == 0x1_0000);

/// SRAM's contents before startup. Real SRAM powers up holding anything; a
/// fixed pattern makes a byte that should have been written stand out.
const JUNK: u8 = 0xA5;

// The pins, from `kr1k::board`, which src/avr/hw.rs uses too.
/// PC1, the boost converter and amplifier enable.
const ENABLE: u8 = 1 << board::ENABLE;
/// PA5 to PA7, the three power paths.
const PATHS: u8 = board::PATHS;
/// PA2 and PA3, the AW2016's TWI lines.
const TWI: u8 = 1 << board::TWI_SDA | 1 << board::TWI_SCL;
/// PA1, which goes to something unknown and is only ever held low.
const HELD_LOW: u8 = 1 << board::HELD_LOW;
const HELD_LOW_CTRL: usize = reg::PORTA_PIN0CTRL + board::HELD_LOW as usize;
/// PD4, the switch, active low.
const SWITCH: u8 = 1 << board::SWITCH;
const SWITCH_PIN: usize = board::SWITCH as usize;
/// PD6, DAC0's output.
const DAC_PIN: u8 = 1 << board::DAC;
const DAC_PIN_CTRL: usize = reg::PORTD_PIN0CTRL + board::DAC as usize;
const SWITCH_CTRL: usize = reg::PORTD_PIN0CTRL + SWITCH_PIN;

/// Instructions startup may take before reaching `main`.
const BOOT_BUDGET: u64 = 100_000;
/// CPU cycles in a 64 Hz tick at 4 MHz.
const TICK_CYCLES: u64 = CLK_PER as u64 / TICKS_PER_SECOND as u64;
const _: () = assert!(TICK_CYCLES * TICKS_PER_SECOND as u64 == CLK_PER as u64);
/// Instructions one pass of the main loop may take, handlers included, taking
/// each as four clocks. The AVRxt column of the instruction set manual's
/// table 5-4 has `call`, `ret` and `reti` at four with a 16-bit PC, and
/// nothing slower, with one exception it doesn't bound. A load from NVM,
/// which the mapped-flash window is, takes "a minimum of one extra cycle"
/// that "varies dependent on the NVM module implementation" (note 2), and
/// DS40002413B doesn't say how many. So an `lds` of the ramp's table could
/// take more than four, and this budget is likely, not certain, for those.
const TICK_BUDGET: u64 = TICK_CYCLES / 4;
/// Clocks the CPU spends getting into a handler and back out of it, on a
/// part with more than 8 KB of flash and a 16-bit PC (§15.3.2.3, table 15-1,
/// which has six in, and four for `reti`).
const INTERRUPT_CYCLES: u64 = 10;
/// Instructions a handler may take, so it fits in a TCB0 period by the same
/// sum.
const HANDLER_BUDGET: u64 = (TIMER_PERIOD - INTERRUPT_CYCLES) / 4;
/// Clocks DAC0's output gets to fall to 0 before a reference is raised or a
/// path added with PC1 high: 10 µs, table 38-23's settling time at 5.5 V.
const DAC_SETTLE_CYCLES: u64 = CLK_PER as u64 / 100_000;
/// Clocks after a change of VREF.DAC0REF before DAC0 may go above 0: 2 µs,
/// table 38-18's tINTREF.
const VREF_SETTLE_CYCLES: u64 = CLK_PER as u64 / 500_000;
/// Clocks after PC1 rises before DAC0 may go above 0: 8 ms, the stock
/// firmware's wait for the boost converter.
const BOOST_START_CYCLES: u64 = CLK_PER as u64 / 125;

/// How many reads of RTC.PITSTATUS report busy after reset or a write to
/// PITCTRLA. The real chip is busy for two 32 kHz clocks while it syncs
/// (§26.13.12); any non-zero number makes the firmware wait.
const PIT_SYNC_READS: u8 = 2;

#[derive(Clone, Copy, Default)]
pub struct Report {
    /// The most stack the firmware used, in bytes.
    pub stack: u16,
    /// The most instructions one pass of the main loop took, from the runs
    /// without interrupts landed on purpose. Every pass still has to fit the
    /// budget with them (`Sim::tick`).
    pub loop_instructions: u64,
    /// What that's measured against.
    pub loop_budget: u64,
    /// The most instructions one pass took that started off resting and ended
    /// in power-down, which is what the chip stays awake for, 64 times a
    /// second, when it's off.
    pub off_instructions: u64,
    /// The most instructions one run of any handler took.
    pub handler_instructions: u64,
    /// What that's measured against.
    pub handler_budget: u64,
    /// Interrupts `Sim::land` put on an instruction of the main loop.
    pub landed: u32,
    /// Handlers that landed with a 16-bit access half done. None of the
    /// firmware's handlers touch a 16-bit register, so this only counts.
    pub mid_access: u32,
    /// The last tick the main loop reached an instruction for the first time
    /// where `Sim::land` could put an interrupt.
    last_new: u32,
}

impl Report {
    /// Adds in another run's figures.
    fn merge(&mut self, run: &Self) {
        self.stack = self.stack.max(run.stack);
        self.loop_instructions = self.loop_instructions.max(run.loop_instructions);
        self.off_instructions = self.off_instructions.max(run.off_instructions);
        self.handler_instructions = self.handler_instructions.max(run.handler_instructions);
        self.landed += run.landed;
        self.mid_access += run.mid_access;
    }

    fn take(&mut self, sim: &Sim) {
        self.stack = self.stack.max(sim.stack());
        self.handler_instructions = self.handler_instructions.max(sim.longest_handler);
        self.landed += sim.landed;
        self.mid_access += sim.mid_access;
        self.last_new = self.last_new.max(sim.last_new);
    }
}

/// The symbols every program here has, as data-space addresses or word
/// addresses as each one's used.
struct Linked {
    main: u32,
    ends: Ends,
    layout: Layout,
}

/// Where interrupts that can't go back to the code they interrupted end up,
/// as byte addresses.
#[derive(Clone, Copy, Default)]
struct Ends {
    /// `__unhandled_interrupt`.
    unhandled: u32,
    /// `hw::halt`, which `__unhandled_interrupt` jumps to.
    halt: u32,
}

impl Linked {
    fn of(elf: &Elf) -> Result<Self, String> {
        let symbol = |name| symbol(elf, name);
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
}

fn symbol(elf: &Elf, name: &str) -> Result<u32, String> {
    elf.symbol(name)
        .ok_or_else(|| format!("no `{name}` symbol"))
}

/// Runs every check on the firmware. With `examples` false, docs/ui.md's
/// examples are left out, for a test build that's meant to disagree with
/// them, and the rest still runs.
pub fn check(elf: &Elf, examples: bool) -> Result<Report, String> {
    let linked = Linked::of(elf)?;
    let image = elf.flash_image();
    // What `cargo xtask hex` and `flash` write.
    let signed = crc::sign(&image, &stamp::TEST)?;
    let reset = symbol(elf, "__reset")?;
    // `cargo xtask peek` reads the supply readings at a fixed address.
    let supplies = symbol(elf, "SUPPLIES")?;
    if supplies != DATA_SPACE + SUPPLIES_AT as u32 {
        return Err(format!(
            "SUPPLIES is at {supplies:#08x}, not at {:#08x}, where `cargo xtask peek` reads it \
             (kr1k::sensors::SUPPLIES_AT)",
            DATA_SPACE + SUPPLIES_AT as u32
        ));
    }

    // A chip that's just had its cell put in, run to the end of startup.
    let power_on = || -> Result<Sim, String> {
        let mut sim = Sim::new(signed.clone(), linked.layout.bss.end, linked.ends)?;
        sim.run_to(linked.main, BOOT_BUDGET)
            .map_err(|e| format!("startup: {e}"))?;
        Ok(sim)
    };
    let sim = power_on()?;
    check_startup(&sim.chip, &linked.layout).map_err(|e| format!("startup: {e}"))?;
    startup_with_statics(&image, reset, &linked)
        .map_err(|e| format!("startup with statics: {e}"))?;
    found_lit(Sim::new(
        signed.clone(),
        linked.layout.bss.end,
        linked.ends,
    )?)
    .map_err(|e| format!("starting with PC1 high: {e}"))?;
    boot_order(Sim::new(
        signed.clone(),
        linked.layout.bss.end,
        linked.ends,
    )?)
    .map_err(|e| format!("start-up order: {e}"))?;
    unhandled_interrupt(power_on()?)
        .map_err(|e| format!("with an interrupt that has no handler: {e}"))?;
    let examples = if examples {
        spec::examples(spec::UI_MD)?
    } else {
        Vec::new()
    };
    // The clock, stuck-input and supply faults, every example, then the
    // random switching, each example and the switching with and without
    // interrupts landing mid-pass, then the collapsing cell and the faults on
    // the AW2016's bus. Each is its own chip, so they run side by side,
    // because the long ones take minutes of simulated time. Errors come back
    // in this order, whichever thread finishes first. The last four come last
    // since they ramp and click through the UI with the light on, so they'd
    // catch many a mistake as a wrong output before the run that names it.
    let faults: [(FaultCheck, &str); 7] = [
        (slow_ticks, "with the slowest ticks"),
        (oscillator_stops, "with the 32 kHz oscillator stopped"),
        (adc_never_answers, "with a dead ADC"),
        (adc_stuck, "with the ADC stuck on one reading"),
        (supplies_kept, "keeping the supply readings"),
        (switch_stuck, "with the switch stuck down"),
        (vddio2_missing, "with VDDIO2 missing"),
    ];
    // VDDIO2 going while lit, and the collapsing cell, come last too, since
    // they hold the light on and ramp it.
    let late_faults: [(FaultCheck, &str); 5] = [
        (cell_collapses, "with a collapsing cell"),
        (aw2016_absent, "with the AW2016 never answering"),
        (sda_held_low, "with SDA held low"),
        (scl_held_low, "with SCL held low"),
        (vddio2_drops, "with VDDIO2 going while lit"),
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

    let mut report = Report {
        loop_budget: TICK_BUDGET,
        handler_budget: HANDLER_BUDGET,
        ..Report::default()
    };
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

/// One of the fault checks `check` runs on a freshly started chip, which adds
/// what it measured to the report.
type FaultCheck = fn(Sim, &mut Report) -> Result<(), String>;

/// Ticks a run with interrupts landing goes on for after the last tick the
/// run without them reached new code, so whatever a late landing broke has
/// time to show.
const LANDING_MARGIN: u32 = 2 * TICKS_PER_SECOND as u32;

/// An example, or the random switching with `None`, run as it is and then
/// with interrupts landing mid-pass. The second only lands on the first
/// time each instruction's reached, so once the first run's reached all it
/// will, the second has nothing left to land on, and the rest of it would
/// repeat the first, so it stops `LANDING_MARGIN` after.
fn both_ways(
    power_on: &impl Fn() -> Result<Sim, String>,
    example: Option<&Example>,
) -> Result<Report, String> {
    let mut report = run(power_on, example, Landing::Watch)?;
    let until = report.last_new + LANDING_MARGIN;
    report.merge(&run(power_on, example, Landing::Until(until))?);
    Ok(report)
}

/// One run on a fresh chip, of an example from docs/ui.md or the random
/// switching with `None`. `Landing::Until` puts a switch bounce, and a TCB0
/// period's end with the light on, on every instruction it reaches
/// (`Sim::land`), which is the race between the handlers and the main loop.
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
            ui_example(sim, example, &mut report).map_err(|e| format!("{}: {e}", example.row))
        }
        None => against_the_library(sim, &mut report).map_err(|e| format!("random switching: {e}")),
    };
    match (result, landing) {
        (Ok(()), _) => Ok(report),
        (Err(e), true) => Err(format!("with interrupts mid-pass: {e}")),
        (Err(e), false) => Err(e),
    }
}

/// Ticks of random switching to compare against the library, about five
/// minutes of a torch being clicked and held.
const RANDOM_TICKS: usize = 20_000;

/// Runs one of docs/ui.md's examples on the firmware and the library in step,
/// then checks where the library ended up against the example.
fn ui_example(mut sim: Sim, example: &Example, report: &mut Report) -> Result<(), String> {
    // Cut short, it never gets to the end. The level is the library's alone,
    // so the run that did get there has checked it.
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

/// Switches the firmware and the library the same random way. The host tests
/// trust the library; this checks the AVR build of it, and `hw.rs`'s pins, do
/// what it says.
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

/// A click from off, then `ticks` more with the switch left alone, in step
/// with the library, which has to leave the light on.
fn lit(sim: &mut Sim, ticks: u16, report: &mut Report) -> Result<(), String> {
    let script = [Action::Clicks(1), Action::Wait(ticks)];
    in_step(sim, testing::samples(script), report)?;
    if !sim.chip.enabled_high() {
        return Err("a click from off left PC1 low, so there's no light to check".into());
    }
    Ok(())
}

/// The slowest tick the oscillators allow, in main clocks. That's the 32 kHz
/// one 30% slow, and the main one 5% fast, so more of its clocks fit in the
/// time. Figure 38-2 has the 4 MHz oscillator within 5% over the whole
/// supply and temperature range. Table 38-10 gives OSC32K no tolerance at
/// all, so the 30% is the ATtiny1616's figure, kept as a guess, and the same
/// one `kr1k::watchdog` sizes the deadline by.
const SLOWEST_TICK_CYCLES: u64 = TICK_CYCLES * 105 / 70;
/// Main clocks `hw::wait_for_tick` waits for a tick, with the light on, before
/// it gives up on the 32 kHz oscillator.
const DEADLINE_CYCLES: u64 = TICK_DEADLINE_PERIODS as u64 * TIMER_PERIOD;
// The slowest tick this chip can be given is well inside the deadline, so only
// a run that stops the oscillator can trip it.
const _: () = assert!(4 * SLOWEST_TICK_CYCLES < DEADLINE_CYCLES);

/// Two seconds lit with every tick as slow as the oscillators' limits allow.
/// `hw::wait_for_tick`'s deadline mustn't trip, because that stops the chip for
/// good, which `Sim::tick` fails. Only the PIT's period changes. The model's
/// watchdog counts PIT periods, so it's none the wiser.
fn slow_ticks(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    sim.chip.tick_cycles = SLOWEST_TICK_CYCLES;
    lit(&mut sim, 2 * TICKS_PER_SECOND, report)
}

/// A click from off, then ten seconds with the switch alone, then another
/// click, and the ADC never finishing a conversion (`Scene::adc_dead`), so
/// `hw::adc_result` only ever sees RESRDY clear. docs/ui.md's `cell:0V` rows
/// are the same path seen as a garbage reading, where this is silence. The
/// light has to come on, then fade out within a few seconds as a fault and
/// stay out, the second click included, the firmware and the library in step
/// the whole way.
///
/// `in_step` only hands back the last level, so a second rig, fed the same
/// samples, keeps the one at each tick.
fn adc_never_answers(sim: Sim, report: &mut Report) -> Result<(), String> {
    let seconds = |s: usize| s * usize::from(TICKS_PER_SECOND);
    // The second click, after the fault, has to light nothing.
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
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
    let lit = levels
        .iter()
        .position(|&level| level > 0)
        .ok_or("the light never came on")?;
    // A second of readings goes before the fault, then a fade from the level
    // it came on at, a level a tick.
    let out = lit + seconds(4);
    if !levels.get(out..).is_some_and(|l| l.iter().all(|&l| l == 0)) {
        return Err(format!(
            "the light isn't out and staying out from 4 s after it came on ({:?} at {out})",
            levels.get(out)
        ));
    }
    Ok(())
}

/// The most ticks from the one the cell falls under the hard floor on to the
/// one PC1 is low on, counting both, for a light that's been on a while:
/// 12 ticks, 188 ms, which is docs/ui.md's promise and H6's. It's typed as a
/// number rather than worked out from the library's constants, so a change
/// to those that breaks the promise fails here. The sum it comes from is the
/// cell unread for `sensors::UNREAD_TICKS` at worst, then
/// `battery::COLLAPSE_READINGS` in a row, the last due a tick after its
/// conversion starts.
const COLLAPSE_TICKS: usize = 12;

/// The cell falling under `battery::COLLAPSE_BELOW` with the light at the top
/// of the ramp, once on each of the 16 ticks of the ADC's round, firmware and
/// library in step. Each time the light has to be out within
/// `COLLAPSE_TICKS`, straight from the top with no fade, as a flat cell and
/// not a fault, and a click with the cell still down has to light nothing.
/// The cell then reads 3.80 V again, so the next click lights it for the
/// next round.
///
/// `in_step` holds PC1, DAC0 and the paths to the library on every tick, so
/// the library's level going to 0 is PC1 going low. The cell's reading is
/// scripted, and it changes between two ticks, so this is the timetable and
/// the firmware's sums and nothing about a real cell, whose voltage falls
/// as it pleases.
fn cell_collapses(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    let rounds = u16::from(CYCLE_TICKS);
    let script = (0..rounds).flat_map(|round| {
        [
            Action::Cell(380),
            Action::Clicks(1),
            // To the top of the ramp, and a tick longer each round, so the
            // cell falls on each tick of the ADC's round in turn.
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
    // Each tick's cell, and the library's level and aux after it.
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
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
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
        // Until the cell's good again, or the script ends.
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
    // One of the 16 ticks is the worst, so it's been run.
    if slowest != COLLAPSE_TICKS {
        return Err(format!(
            "the slowest took {slowest} ticks, not the {COLLAPSE_TICKS} the worst tick of the \
             round should, so this didn't reach it"
        ));
    }
    Ok(())
}

/// The ADC stuck on the cell's 3.80 V reading for every input, ground
/// included, a second after a click lights the torch, then a second click
/// once the light's out, firmware and library in step. Every voltage reading
/// from then on looks like a good cell, so only ground can tell. The light
/// has to fade out as a fault, and stay out through the second click, which
/// the library has to agree is a fault too.
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
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
    let stuck = ticks
        .iter()
        .position(|tick| tick.0)
        .ok_or("the ADC never stuck")?;
    if ticks[stuck - 1].1 == 0 || ticks[..stuck].iter().any(|tick| tick.2) {
        return Err("the light wasn't on and well before the ADC stuck".into());
    }
    // Two ground readings, then a fade from level 52, a level a tick.
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

/// What the firmware keeps at `kr1k::sensors::SUPPLIES_AT` for `cargo xtask
/// peek`, against what the world's VDDIO2 / 10 and VDD / 10 read. A second
/// with VDDIO2 at 3.30 V and the cell at 4.00 V, which the power-on check
/// reads. Then five minutes with VDDIO2 tied to a 3.60 V cell, which the
/// next check at rest reads. Then a click on, with VDDIO2 dropping to 2.00 V
/// once the light's lit, which nothing reads, since VDDIO2 is only read with
/// the light off. Firmware and library in step, and the library's
/// `Torch::supplies` has to say the same as the firmware's SRAM at the end of
/// each.
fn supplies_kept(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    let quiet = |ticks: u32, cell: u16, vddio2_level: Option<u16>| {
        iter::repeat_n(
            Sample {
                pressed: false,
                scene: Scene {
                    cell,
                    vddio2_level,
                    ..Scene::NORMAL
                },
            },
            ticks as usize,
        )
    };
    let second = u32::from(TICKS_PER_SECOND);
    let click = testing::samples([Action::Cell(360), Action::Clicks(1)]);
    let samples = quiet(second, 400, Some(330))
        .chain(quiet(301 * second, 360, None))
        .chain(click)
        .chain(quiet(second, 360, Some(200)));
    // The firmware's four bytes, and the library's, at the end of each part.
    let marks = [second as usize, 302 * second as usize];
    let seen = std::cell::RefCell::new(Vec::new());
    let mut shadow = Rig::default();
    let samples = samples.enumerate().map(|(tick, sample)| {
        shadow.tick_in(sample.pressed, &sample.scene);
        if marks.contains(&(tick + 1)) {
            seen.borrow_mut().push(shadow.supplies());
        }
        sample
    });
    let end = in_step(&mut sim, samples, report)?;
    if end == Some(0) {
        return Err("the click didn't light it".into());
    }
    let at = SUPPLIES_AT - usize::from(SRAM.start);
    let kept = sim
        .chip
        .sram
        .get(at..at + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(Supplies::from_bytes)
        .ok_or("SUPPLIES_AT isn't in SRAM")?;
    let reading = testing::voltage_reading;
    let want = [
        Supplies {
            cell: reading(400),
            vddio2: reading(330),
        },
        Supplies {
            cell: reading(360),
            vddio2: reading(360),
        },
    ];
    let seen = seen.into_inner();
    if seen != want {
        return Err(format!(
            "the library kept {seen:?} at the end of the first two parts, not {want:?}"
        ));
    }
    if shadow.supplies() != want[1] || kept != want[1] {
        return Err(format!(
            "lit, the library has {:?} and the firmware's SRAM {kept:?}, where both should \
             still be the last reading with the light off, {:?}",
            shadow.supplies(),
            want[1]
        ));
    }
    Ok(())
}

/// The switch pressed from the first tick and never let go, for about five
/// minutes of ticks, firmware and library in step. A jammed button is a hold
/// from off, which does nothing, so the light has to stay off.
fn switch_stuck(mut sim: Sim, report: &mut Report) -> Result<(), String> {
    let samples = iter::repeat_n(
        Sample {
            pressed: true,
            scene: Scene::NORMAL,
        },
        20_000,
    );
    let end = in_step(&mut sim, samples, report)?;
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
    Ok(())
}

/// MVIO says VDDIO2 isn't there, from power-on, so PORTC's pins, PC1 among
/// them, are tri-stated whatever PORT says (§19.3.2.1), and come back with
/// whatever PORT holds if VDDIO2 does. The firmware has to treat it as a
/// fault and keep PC1 low, through a click and a hold, in step with a
/// library that's told the same.
///
/// It starts with a second at rest. A firmware that let the power-on check
/// go would still find VDDIO2 missing on its first pass, now that every
/// waking pass looks, and never light. But it would be a torch with nothing
/// wrong for that one pass, awake to read its cell, where one that failed
/// the check powers down from the first tick, and the rest is where that
/// shows.
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
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
    if sim.chip.pc1_rises != 0 {
        return Err("raised PC1 with VDDIO2 missing from power-on".into());
    }
    Ok(())
}

/// VDDIO2 goes with the light on, at the top of the ramp, so all three paths
/// are up and DAC0 is near the top of its range. It goes at the worst moment
/// for a firmware that looks once a pass, straight after this pass's look
/// (`Chip::vddio2_goes`), so it's a whole tick before anything can see it.
/// The pass that takes the next tick has to leave PC1 clear in PORTC, DAC0
/// at 0 against 1.024 V and the paths low before it sleeps
/// (`Chip::dark_without_vddio2`), which is a tick and a pass after it went.
///
/// Then it has to stay off, in step with the library, through a click and a
/// hold with VDDIO2 still gone, and through another click once it's back,
/// when PC1 would rise again if PORTC still had it set (§19.3.2.1). The
/// library has the aux white after each press, and the AW2016 is held to it.
///
/// Dips shorter than a tick aren't run. A firmware that looks once a pass
/// can miss one whole, and the chip would then put PC1 back up with DAC0
/// live, which `Chip::vddio2_moved` refuses, so such a run could only fail.
/// docs/safety.md has that under H1's what's left.
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
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
    if sim.chip.pc1_rises != 1 {
        return Err(format!(
            "PC1 rose {} times, not just the once before VDDIO2 went",
            sim.chip.pc1_rises
        ));
    }
    Ok(())
}

/// The AW2016 never acknowledges its address.
fn aw2016_absent(sim: Sim, report: &mut Report) -> Result<(), String> {
    bad_bus(sim, Fault::Absent, report)
}

/// SDA held low, so every address loses arbitration with a bus error.
fn sda_held_low(sim: Sim, report: &mut Report) -> Result<(), String> {
    bad_bus(sim, Fault::SdaLow, report)
}

/// SCL held low, so no byte ever finishes and no flag ever comes up, which
/// only the update's budget of polls gets the firmware out of.
fn scl_held_low(sim: Sim, report: &mut Report) -> Result<(), String> {
    bad_bus(sim, Fault::SclLow, report)
}

/// The light on and off, lockout and a press in it, then on, ramped and off
/// again, all with `fault` on the bus. Every update fails, and the firmware
/// has to give each up without hanging or retrying (`AuxWatch`), every pass
/// inside the budget (`Sim::tick`), and the light has to follow the library
/// all the way, ending off.
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
    if end != Some(0) {
        return Err(format!("ends at level {end:?}, not off"));
    }
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

/// Steps `hw::halt` may take from dropping PC1 to its `sleep`, counting
/// instructions, or waits if it's gone to sleep some other way.
const HALT_BUDGET: u64 = 100;
/// PIT periods' worth of time to watch a chip that's stopped for good.
const DOWN_PERIODS: u64 = 4 * TICKS_PER_SECOND as u64;

/// The 32 kHz oscillator stops with the light on, and takes the tick and the
/// watchdog with it. `hw::wait_for_tick`'s deadline is all that's left, and
/// `main` answers it with `hw::halt`. This checks PC1 drops within the
/// deadline of the stop, and not before it, which a count that ran fast would
/// do. Then that the firmware gets to a `sleep` in power-down with no
/// interrupt enabled, by `ready_for_power_down`'s rules, and that nothing
/// runs and PC1 stays low for four seconds after.
///
/// The oscillator stops just as a tick lands, so the firmware has one more
/// pass, and the deadline counts from the wait that ends it.
///
/// A tick that goes missing with the oscillator still running ends another
/// way, in the watchdog resetting the chip. That can't run here, because this
/// chip has no resets, so a bite is a failed run.
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
    // `lit` took the rest. `hw::halt` is a call deeper than the loop.
    report.stack = report.stack.max(sim.stack());
    Ok(())
}

/// The vector `unhandled_interrupt` forces. Nothing in the firmware touches
/// TCB1 and the model doesn't have it, so its slot goes to
/// `__unhandled_interrupt`.
const VECTOR_UNHANDLED: u8 = regs::VECTOR_TCB1_INT as u8;

/// An interrupt with no handler fires with the light on, which nothing in the
/// firmware can make happen, so `Chip::force_interrupt` stands in for a
/// peripheral gone wrong. src/avr/rt.rs's `__unhandled_interrupt` jumps into
/// `hw::halt`, so within `HALT_BUDGET` instructions of taking it, PC1 has to
/// be low, DAC0 at 0, the paths low, and the chip asleep for good in
/// power-down. Then it has to stay that way, with no tick taken, since
/// LVL0EX is still set and halt turned the PIT's interrupt off anyway.
///
/// On a real chip the watchdog then resets it, since nothing feeds it. This
/// chip has no resets, so the run stops one PIT period short of the bite.
/// The interrupt is forced on the pass after a `wdr`, so that's the most
/// periods there are to watch, whatever the click timings.
fn unhandled_interrupt(mut sim: Sim) -> Result<(), String> {
    if sim.handled & 1 << VECTOR_UNHANDLED != 0 {
        return Err(format!(
            "{} has a handler now, so pick another vector to force",
            vector_name(VECTOR_UNHANDLED)
        ));
    }
    lit(&mut sim, TICKS_PER_SECOND / 4, &mut Report::default())?;
    // A pass that feeds leaves the count lower than the pass before it did.
    let mut fed = false;
    for _ in 0..=FEED_TICKS {
        let before = sim.chip.wdt.ticks;
        sim.tick(false)?;
        if sim.chip.wdt.ticks < before {
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

/// Feeds the same switch samples to the firmware, from just after `main`, and
/// to `kr1k::torch`, and checks the pins show the library's output on every
/// tick. Returns the level it ends at, or `None` if `Landing::Until` cut it
/// short.
fn in_step(
    sim: &mut Sim,
    samples: impl IntoIterator<Item = Sample>,
    report: &mut Report,
) -> Result<Option<u8>, String> {
    // The firmware reads the fuses before its first tick, so the world it
    // powers up in is the first sample's.
    let mut samples = samples.into_iter().peekable();
    let power_on = World::from(samples.peek().map_or(Scene::NORMAL, |s| s.scene));
    sim.chip.set_world(power_on)?;
    if power_on.scene.corrupt_flash {
        // The checksum's high byte, which is never code, so the firmware
        // still runs however big it gets.
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
    let mut rig = Rig::new(power_on.faults());
    let mut was_resting = false;
    let mut feeder = Feeder::new();
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
        // What MVIO says as the pass begins, which is what the library's
        // told this tick. It's the scene's, but for a chip whose fuses have
        // single supply, and for a run that takes VDDIO2 away mid-pass,
        // after the firmware's looked (`Chip::vddio2_goes`).
        let scene = Scene {
            vddio2: sim.chip.vddio2_ok(),
            ..sample.scene
        };
        let power_downs = sim.chip.power_downs;
        let took = sim.tick(sample.pressed).map_err(at)?;
        // Interrupts landed on purpose run their handlers on top of the
        // pass, which a real tick never sees that many of.
        if !landing {
            report.loop_instructions = report.loop_instructions.max(took);
        }
        let powered_down = sim.chip.power_downs > power_downs;
        // Only a pass that started off resting, which is what a torch that's
        // off spends every tick.
        if powered_down && was_resting {
            report.off_instructions = report.off_instructions.max(took);
        }
        rig.tick_in(sample.pressed, &scene);
        sim.chip.shows(rig.torch.output()).map_err(at)?;
        if !sim.chip.aux_unwired {
            aux.tick(&sim.chip.twi, rig.torch.aux()).map_err(at)?;
        }
        was_resting = rig.torch.resting();
        // `main`'s feeder, ticked at the end of each pass as it is there. The
        // sleep that ended this pass was the firmware's answer to the state
        // the pass left behind.
        feeder.tick();
        let power_down = feeder.power_down(was_resting);
        if powered_down != power_down {
            return Err(at(format!(
                "the library says {}, but the firmware {}",
                if power_down { "power down" } else { "idle" },
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

/// Ticks the aux may stay dark after the torch wants a colour. From a state
/// nobody knows, that's a reset, then a wake, and the colour on the third.
const AUX_CATCH_UP_TICKS: u8 = 2;

/// Holds the AW2016 to the colour the library wants, at the end of every
/// pass, and the firmware to giving up on a bus that fails.
///
/// With a sound bus, whatever the aux show is the colour the torch wants, at
/// the brightness `ui::Aux::bright` says, through the board's channel map,
/// never a wrong one. It can lag dark for `AUX_CATCH_UP_TICKS` while the chip
/// wakes. When the torch wants them off the AW2016 is in standby by the end
/// of that pass, since that's what H13 in docs/safety.md counts on. Dark
/// while the light's on comes from that, and `twi.rs` and `Chip::pc1_moves`
/// refuse the aux lit with PC1 high on the way.
///
/// With a fault on the bus, every update fails, and the firmware may only try
/// again once the colour it gave up on has changed, never every tick. Each
/// pass turns the host on once at most.
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

/// What examples/link.rs's `main` leaves in `LINK_PROBE_COPY`. That's its
/// .rodata table, then its .data static's starting values, then a byte it
/// doesn't touch.
const PROBE_EXPECTED: [u8; 7] = [0x12, 0x34, 0x56, 0x9A, 0xBC, 0xDE, 0];

/// Runs the link probe from reset with SRAM full of junk, and checks it read
/// its const table through the mapped-flash window and its .data static from
/// where the startup code copied it.
pub fn probe(elf: &Elf) -> Result<(), String> {
    let linked = Linked::of(elf)?;
    let layout = &linked.layout;
    let rodata = elf.section(".rodata").ok_or("no .rodata section")?;
    if rodata.is_empty() || layout.data.is_empty() || layout.bss.is_empty() {
        return Err(".rodata, .data or .bss is empty, so there's nothing to check".into());
    }
    let mut sim = Sim::new(elf.flash_image(), layout.bss.end, linked.ends)?;
    sim.run_to(linked.main, BOOT_BUDGET)
        .and_then(|()| check_startup(&sim.chip, layout))
        .map_err(|e| format!("startup: {e}"))?;
    sim.run_to(symbol(elf, "link_probe_done")? / 2, BOOT_BUDGET)?;

    let copy = (symbol(elf, "LINK_PROBE_COPY")? - DATA_SPACE) as u16;
    let got = sim.chip.sram(&(copy..copy + PROBE_EXPECTED.len() as u16));
    if got != PROBE_EXPECTED {
        return Err(format!(
            "LINK_PROBE_COPY holds {got:02x?}, expected {PROBE_EXPECTED:02x?}"
        ));
    }
    Ok(())
}

/// Runs the guards probe, examples/guards.rs, from reset to `guards_done`.
/// It asks `hw::wait_for_tick` for power-down on every wait, four times with
/// the light on and once with it off, so it has to power down exactly once.
/// Powering down with PC1 high is refused on the way, by
/// `ready_for_power_down`, so a missing guard fails before the count does.
pub fn guards(elf: &Elf) -> Result<(), String> {
    let linked = Linked::of(elf)?;
    let mut sim = Sim::new(elf.flash_image(), linked.layout.bss.end, linked.ends)?;
    sim.run_to(
        symbol(elf, "guards_done")? / 2,
        BOOT_BUDGET + 8 * TICK_BUDGET,
    )?;
    match sim.chip.power_downs {
        1 => Ok(()),
        n => Err(format!(
            "powered down {n} times, but only its one wait with the light off should"
        )),
    }
}

/// Main clocks in a millisecond, which is how many instructions the
/// simulator counts as one.
const MS: u64 = CLK_PER as u64 / 1000;

/// One of `first_light`'s runs, on a chip started with the switch as the
/// `bool` says, and what it's called.
type FirstLightRun = (fn(Sim) -> Result<(), String>, bool, &'static str);

/// Runs the first-light image, examples/first_light.rs, from reset, through
/// clicks, bounces, presses while it arms and a switch held down, and holds
/// it to its promises as well as every rule the firmware has. It never sets
/// SREG.I or enables an interrupt, never runs `sleep` or `wdr`, never writes
/// WDT.CTRLA and never starts TCB0 or the PIT (`Chip::bare`). PC1, DAC0, its
/// reference and the paths keep every rule they have for the firmware,
/// including DAC0's 8 ms after PC1 rises, which here only a busy-wait can
/// meet.
///
/// The level is `ui::FLOOR`, and the example's waits are its own constants,
/// so the runs press well inside and well outside them, 400 ms against
/// 500 ms of arming, 10 ms and 200 ms against 30 ms of debounce.
pub fn first_light(elf: &Elf) -> Result<(), String> {
    let linked = Linked::of(elf)?;
    let image = elf.flash_image();
    let fresh = || -> Result<Sim, String> {
        let mut sim = Sim::new(image.clone(), linked.layout.bss.end, linked.ends)?;
        sim.chip.bare = true;
        Ok(sim)
    };
    found_lit(fresh()?).map_err(|e| format!("starting with PC1 high: {e}"))?;
    let power_on = |pressed: bool| -> Result<Sim, String> {
        let mut sim = fresh()?;
        sim.chip.set_switch(pressed)?;
        sim.run_to(linked.main, BOOT_BUDGET)
            .and_then(|()| check_startup(&sim.chip, &linked.layout))
            .map_err(|e| format!("startup: {e}"))?;
        Ok(sim)
    };

    let runs: [FirstLightRun; 6] = [
        (at_rest, false, "from reset"),
        (clicks, false, "clicked on and off"),
        (pressed_arming, false, "pressed while it arms"),
        (held_from_reset, true, "held from reset"),
        (held_on, false, "held once on"),
        (bounces, false, "with a bouncing switch"),
    ];
    let power_on = &power_on;
    thread::scope(|scope| {
        let runs = runs.map(|(run, pressed, name)| {
            scope.spawn(move || run(power_on(pressed)?).map_err(|e| format!("{name}: {e}")))
        });
        runs.into_iter()
            .try_for_each(|run| run.join().unwrap_or_else(|_| Err("a run panicked".into())))
    })
}

/// Runs `sim` for `clocks` with the switch as `pressed` says.
fn hold(sim: &mut Sim, pressed: bool, clocks: u64) -> Result<(), String> {
    sim.chip.set_switch(pressed)?;
    let until = sim.chip.now + clocks;
    while sim.chip.now < until {
        sim.step()?;
    }
    Ok(())
}

/// A press, or a release with `pressed` false, that bounces for 5 ms before
/// it settles, then `ms` settled.
fn bounced(sim: &mut Sim, pressed: bool, ms: u64) -> Result<(), String> {
    for i in 0..20 {
        hold(sim, pressed == (i % 2 == 0), MS / 4)?;
    }
    hold(sim, pressed, ms * MS)
}

/// The driver at first light's level.
fn lit_at_floor(sim: &Sim) -> Result<(), String> {
    sim.chip
        .shows(kr1k::ramp::output(kr1k::ui::FLOOR))
        .map_err(|e| format!("should be lit: {e}"))
}

fn dark(sim: &Sim) -> Result<(), String> {
    sim.chip
        .shows(Output::OFF)
        .map_err(|e| format!("should be dark: {e}"))
}

/// PC1 has risen `n` times.
fn rises(sim: &Sim, n: u32) -> Result<(), String> {
    match sim.chip.pc1_rises {
        rises if rises == n => Ok(()),
        rises => Err(format!("PC1 has risen {rises} times, not {n}")),
    }
}

/// Two seconds with the switch left alone. PC1 never rises, and the chip
/// rests as `peek` saw the stock firmware rest on this torch (docs/breakout.md's
/// results), PC1 an output
/// and low, DAC0 driving PD6 at 0 against 1.024 V with PD6's input buffer
/// off, and the paths outputs and low. PA1 is an output at 0 with no
/// pull-up, as the stock build had it. PD4 reads with no pull-up and no
/// sense, and the unused pins have their pull-ups, as board.rs has them.
fn at_rest(mut sim: Sim) -> Result<(), String> {
    hold(&mut sim, false, 2000 * MS)?;
    rises(&sim, 0)?;
    dark(&sim)?;
    let io = &sim.chip.io;
    let pin = |base: usize, n: u8| io[base + usize::from(n)];
    let checks = [
        (io[reg::VPORTC_DIR] & ENABLE != 0, "PC1 isn't an output"),
        (
            io[reg::DAC0_CTRLA] == DAC_DRIVING,
            "DAC0 isn't on and driving PD6",
        ),
        (
            io[DAC_PIN_CTRL] & PORT_ISC == ISC_INPUT_DISABLE,
            "PD6's input buffer is on",
        ),
        (
            io[reg::VPORTA_DIR] & PATHS == PATHS,
            "the paths aren't outputs",
        ),
        (io[reg::VPORTA_DIR] & HELD_LOW != 0, "PA1 isn't an output"),
        (io[reg::VPORTA_OUT] & HELD_LOW == 0, "PA1 isn't low"),
        (io[HELD_LOW_CTRL] == 0, "PA1 has a pull-up or a sense"),
        (io[SWITCH_CTRL] == 0, "PD4 has a sense or a pull-up"),
        (
            [
                pin(reg::PORTA_PIN0CTRL, board::FET),
                pin(reg::PORTC_PIN0CTRL, board::UNUSED_PC2),
                pin(reg::PORTC_PIN0CTRL, board::UNUSED_PC3),
                pin(reg::PORTD_PIN0CTRL, board::UNUSED_PD5),
                pin(reg::PORTD_PIN0CTRL, board::POWER_BANK),
            ]
            .iter()
            .all(|&ctrl| ctrl & PORT_PULLUPEN != 0),
            "an unused pin has no pull-up",
        ),
    ];
    match checks.into_iter().find(|&(ok, _)| !ok) {
        Some((_, why)) => Err(why.into()),
        None => Ok(()),
    }
}

/// Armed, then three clicks, on, off and on again, each held 200 ms and let
/// go for 300 ms. It's lit from the press, and stays as it is through the
/// release.
fn clicks(mut sim: Sim) -> Result<(), String> {
    hold(&mut sim, false, 1000 * MS)?;
    for click in 1..=3 {
        hold(&mut sim, true, 200 * MS)?;
        let shown = |sim: &Sim| {
            if click % 2 == 1 {
                lit_at_floor(sim)
            } else {
                dark(sim)
            }
        };
        shown(&sim).map_err(|e| format!("press {click}: {e}"))?;
        hold(&mut sim, false, 300 * MS)?;
        shown(&sim).map_err(|e| format!("after release {click}: {e}"))?;
    }
    rises(&sim, 2)
}

/// A 200 ms press 400 ms after reset, before it's armed, does nothing. Nor
/// does the next press, until the switch has read released long enough
/// again, and then one does.
fn pressed_arming(mut sim: Sim) -> Result<(), String> {
    hold(&mut sim, false, 400 * MS)?;
    hold(&mut sim, true, 200 * MS)?;
    dark(&sim)?;
    hold(&mut sim, false, 300 * MS)?;
    hold(&mut sim, true, 200 * MS)?;
    dark(&sim)?;
    rises(&sim, 0)?;
    hold(&mut sim, false, 1000 * MS)?;
    hold(&mut sim, true, 200 * MS)?;
    lit_at_floor(&sim)
        .map_err(|e| format!("pressed once armed: {e}"))
        .and_then(|()| rises(&sim, 1))
}

/// Five seconds with the switch held from before reset never lights. Let go
/// long enough, a press does.
fn held_from_reset(mut sim: Sim) -> Result<(), String> {
    hold(&mut sim, true, 5000 * MS)?;
    dark(&sim)?;
    rises(&sim, 0)?;
    hold(&mut sim, false, 1000 * MS)?;
    hold(&mut sim, true, 200 * MS)?;
    lit_at_floor(&sim).and_then(|()| rises(&sim, 1))
}

/// A press held for five seconds turns it on once and does nothing more. Let
/// go, the next press turns it off.
fn held_on(mut sim: Sim) -> Result<(), String> {
    hold(&mut sim, false, 1000 * MS)?;
    hold(&mut sim, true, 5000 * MS)?;
    lit_at_floor(&sim)?;
    rises(&sim, 1)?;
    hold(&mut sim, false, 300 * MS)?;
    hold(&mut sim, true, 200 * MS)?;
    dark(&sim).and_then(|()| rises(&sim, 1))
}

/// Presses and releases that bounce for 5 ms, and 10 ms presses too short to
/// count, each toggle once at most, the glitches never.
fn bounces(mut sim: Sim) -> Result<(), String> {
    hold(&mut sim, false, 1000 * MS)?;
    hold(&mut sim, true, 10 * MS)?;
    hold(&mut sim, false, 300 * MS)?;
    dark(&sim).map_err(|e| format!("after a 10 ms press: {e}"))?;
    bounced(&mut sim, true, 200)?;
    lit_at_floor(&sim)?;
    bounced(&mut sim, false, 300)?;
    lit_at_floor(&sim)?;
    hold(&mut sim, true, 10 * MS)?;
    hold(&mut sim, false, 300 * MS)?;
    lit_at_floor(&sim).map_err(|e| format!("after a 10 ms press: {e}"))?;
    bounced(&mut sim, true, 200)?;
    dark(&sim)?;
    bounced(&mut sim, false, 300)?;
    dark(&sim)?;
    rises(&sim, 1)
}

/// Where src/avr/rt.rs's startup copies and zeroes, as data-space addresses.
struct Layout {
    data: Range<u16>,
    bss: Range<u16>,
    /// .data's initial values, in the mapped-flash window.
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

/// Moves .data and .bss somewhere awkward, gives .data five bytes of initial
/// values, patches the addresses into the real startup code and runs it. Then
/// checks it wrote exactly those bytes and nothing either side. `reset` is the
/// byte address of `__reset`.
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

/// Rewrites the `ldi`s in src/avr/rt.rs's startup that load the .data and
/// .bss addresses, after checking they're the ones expected, in order.
fn patch_startup(
    image: &mut [u8],
    reset: usize,
    linked: &Layout,
    moved: &Layout,
) -> Result<(), String> {
    // (register, byte) for each `ldi`, as rt.rs loads them, which is X =
    // __data_start, Z = __data_load, r25:r24 = __data_end, X = __bss_start,
    // r25:r24 = __bss_end.
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
        if w & 0xFE0E == 0x940E {
            break; // call main
        }
        if w >> 12 == 0xE {
            let k = ((w >> 4) & 0xF0 | w & 0x0F) as u8;
            found.push((at, (16 + ((w >> 4) & 0x0F)) as u8, k));
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

/// Runs `sim` from the reset vector with PORTC as a lit torch has it, PC1 an
/// output and high, and checks the first register the firmware writes clears
/// PC1 in VPORTC.OUT. That's `hw::hold_enable_low`'s job, ahead of the CRC
/// scan and everything else.
///
/// A stray jump to the vector would find the port like that, since nothing
/// short of a reset clears OUT. This isn't a model of one, though. Only PORTC
/// starts dirty, and the rest of the chip, SP and SREG included, is as a reset
/// leaves it. It shows what the firmware's first write does to a pin it finds
/// high, and that's all.
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

/// Runs `sim` from reset to the first wait for a tick, and checks `main` got
/// there in the order it has to. The reset flags are cleared before the
/// watchdog's on. The CRC scan, the fuse read and the VDDIO2 check come
/// before the chip's set up at all. The PIT's on before the watchdog.
/// `found_lit` checks what comes before all of these.
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

/// A bit for each vector whose slot in `image`'s table is a `jmp` to
/// somewhere other than `unhandled` (a word address). layout.rs checks every
/// slot is a `jmp`.
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

/// What a handler found when it landed, with its return address pushed.
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
    /// Instructions run, which unlike `Chip::now` stops while asleep.
    executed: u64,
    /// Instructions that count towards a pass of the main loop. That's all
    /// of them, except handlers' while `land` is landing interrupts, since a
    /// real pass never sees one on every new instruction.
    counted: u64,
    /// `__unhandled_interrupt`, as a word address.
    unhandled: u32,
    /// `hw::halt`, as a word address.
    halt: u32,
    /// A bit per vector whose slot doesn't jump there.
    handled: u64,
    /// The vector `Chip::force_interrupt` asked for, once the CPU's taken
    /// it. It's allowed to go to `__unhandled_interrupt`, since that's what
    /// it's for.
    forced: Option<u8>,
    /// The running handler has reached `hw::halt`, so it'll never return to
    /// the code it interrupted.
    abandoned: bool,
    /// What the running handler has to give back.
    landed_on: Option<Landed>,
    /// Instructions the running handler has taken.
    handler_steps: u64,
    /// The most any handler has taken.
    longest_handler: u64,
    /// What `land` does.
    landing: Landing,
    /// A bit per word of flash, for the instructions `land` has seen.
    reached: Vec<bool>,
    /// The tick `in_step` is on.
    tick_no: u32,
    /// The last tick `land` saw an instruction for the first time.
    last_new: u32,
    /// Interrupts landed so far.
    landed: u32,
    /// Handlers so far that landed with a 16-bit access half done.
    mid_access: u32,
}

/// What `Sim::land` does the first time the main loop reaches an instruction
/// where an interrupt could land.
#[derive(Clone, Copy)]
enum Landing {
    /// Nothing.
    Off,
    /// Notes the tick, in `Sim::last_new`.
    Watch,
    /// Lands interrupts there, until `in_step` stops after this tick.
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
            executed: 0,
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
            mid_access: 0,
        })
    }

    /// Before the first time the main loop reaches each instruction, PD4's
    /// flag goes up as a bounce would raise it, if its sense is on, and so
    /// does TCB0's CAPT if its interrupt's on, as the end of a period would.
    /// The handler lands there or, with interrupts held off, at the first
    /// place it can. Left alone a 1 ms period only lands on the instructions
    /// it happens to meet, and a bounce on none.
    ///
    /// Neither is a real edge or a real period, so the switch doesn't move
    /// and TCB0's count doesn't restart. A landed CAPT is one more towards
    /// `hw::wait_for_tick`'s deadline, so the run that checks the deadline
    /// isn't early leaves landing off.
    ///
    /// `Landing::Watch` only notes when each instruction's first reached, so
    /// `both_ways` knows how far a landing run has new code to land on.
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

    /// The most stack used so far, in bytes.
    const fn stack(&self) -> u16 {
        SRAM.end - 1 - self.lowest_sp
    }

    fn step(&mut self) -> Result<(), String> {
        let pc = self.cpu.pc;
        let awake = !self.cpu.asleep;
        let handling = self.cpu.handling;
        // `hw::halt` is main-loop code, wherever it was reached from, and
        // gets the main loop's rules.
        self.chip.in_handler = handling.is_some() && !self.abandoned;
        self.chip.interrupts_on = self.cpu.flag(I);
        self.cpu.step(&mut self.chip)?;
        // Only a reset wakes a chip with no interrupt enabled (§13.3.1), and
        // figure 15-1 has the global enable gate the wake-up as well. Neither
        // says the other's enough, so stopping for good wants both, as
        // `hw::halt` does.
        if self.chip.down && self.cpu.flag(I) {
            return Err(format!(
                "{:#06x}: stops for good with SREG.I still set, which leaves the \
                 interrupt enables alone keeping it down",
                pc * 2
            ));
        }
        if self.chip.bare {
            if self.cpu.flag(I) {
                return Err(format!(
                    "{:#06x}: sets SREG.I, and first light has no interrupts",
                    pc * 2
                ));
            }
            if self.chip.enabled != 0 {
                return Err(format!(
                    "{:#06x}: enables {}, and first light has no interrupts",
                    pc * 2,
                    vector_name(self.chip.enabled.trailing_zeros() as u8)
                ));
            }
        }
        self.watch_handler(handling)?;
        if self.cpu.handling.is_some() && self.cpu.pc == self.halt {
            self.abandon();
        }
        if awake {
            self.executed += 1;
            let landing = matches!(self.landing, Landing::Until(_));
            if !landing || handling.is_none() || self.abandoned {
                self.counted += 1;
            }
            self.chip.elapse()?;
        }
        self.lowest_sp = self.lowest_sp.min(self.cpu.sp);
        if self.cpu.sp < self.stack_floor {
            return Err(format!("{:#06x}: the stack ran into .bss", pc * 2));
        }
        // The real chip would `reti` straight back into the main loop with
        // the flag still up, and take the interrupt again, forever. Better to
        // catch it as soon as one's enabled, before it ever fires.
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

    /// Takes note of a handler starting or finishing. `before` is what the
    /// CPU was handling before the step just run.
    fn watch_handler(&mut self, before: Option<u8>) -> Result<(), String> {
        match (before, self.cpu.handling) {
            (None, Some(vector)) => {
                if self.chip.forced == Some(vector) {
                    self.chip.forced = None;
                    self.forced = Some(vector);
                }
                self.chip.interrupted = self.chip.temps_in_use();
                self.mid_access += u32::from(self.chip.interrupted != 0);
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
            // A handler in `hw::halt` has stopped being one in all but
            // LVL0EX, and sleeps from there until a reset.
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

    /// The running handler has reached `hw::halt`, which never returns, so
    /// the rules about giving back what it found stop applying. That's
    /// `__unhandled_interrupt`'s way out, and it'd be any handler's that
    /// panicked.
    ///
    /// That includes TEMP (`Chip::temp_undisturbed`). If the handler landed
    /// with the main loop halfway through a 16-bit register, `hw::set_output`'s
    /// writes in `halt` overwrite the low byte the main loop left in DAC0's
    /// TEMP. The rule's there because the main loop would carry on and write
    /// its high byte with the wrong low one. Here it never carries on, so
    /// there's nothing left to corrupt, and `halt`'s own writes go low byte
    /// then high byte like any other. The half-done access goes too, since
    /// nothing will ever finish it, and the real TEMP takes `halt`'s low byte
    /// over the top of it.
    ///
    /// The handler's budget goes too, since `halt` sleeps from here until a
    /// reset, and so does the check that it gives back the registers, SREG
    /// and SP, since it never returns them to anything.
    const fn abandon(&mut self) {
        self.abandoned = true;
        self.chip.interrupted = 0;
        self.chip.tcb.temp = None;
        self.chip.dac.temp = None;
        self.chip.adc.temp = None;
        self.landed_on = None;
    }

    /// A handler lands between any two instructions of the main loop, so
    /// anything it changes, the compiled code around it sees change at random.
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

    /// Runs until PC reaches `target`, a word address.
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

    /// One pass of the main loop with the switch as given, run until the
    /// next tick. Returns how many instructions the pass took, which stops
    /// when the firmware first goes to sleep, counting handlers' unless
    /// they're landing on purpose (`Sim::counted`). A firmware that stops for good
    /// (`hw::halt`) with ticks still coming has failed.
    fn tick(&mut self, pressed: bool) -> Result<u64, String> {
        self.chip.set_switch(pressed)?;
        let (start, ticks) = (self.counted, self.chip.ticks);
        self.chip.tick_polled = false;
        let mut pass_end = None;
        while self.chip.ticks == ticks {
            self.step()?;
            if self.chip.down {
                return Err(
                    "sleeps with no interrupt enabled, so only a reset wakes it \
                     (section 13.3.1): it's stopped for good"
                        .into(),
                );
            }
            if self.chip.tick_polled {
                pass_end.get_or_insert(self.counted);
            }
            let ran = pass_end.unwrap_or(self.counted) - start;
            if ran > TICK_BUDGET {
                return Err(format!(
                    "a pass of the main loop ran past {TICK_BUDGET} instructions, \
                     so it hangs or misses 64 Hz ticks"
                ));
            }
        }
        Ok(pass_end.unwrap_or(self.counted) - start)
    }
}

/// Whether an ISC value senses anything, so raises a flag and asks for an
/// interrupt (§18.5.16's ISC table).
const fn senses(isc: u8) -> bool {
    matches!(isc, ISC_BOTHEDGES | ISC_RISING | ISC_FALLING | ISC_LEVEL)
}

/// The watchdog, as the firmware can see it. Its settings are in
/// `io[WDT_CTRLA]`. It's timed in PIT periods, not instructions, since the two
/// share an oscillator, so a period is always 16 WDT clocks.
#[derive(Default)]
struct Wdt {
    /// Ticks since the last `wdr`, or since CTRLA was written.
    ticks: u32,
    /// Whether a `wdr` has started window mode's closed windows.
    windowed: bool,
    /// Reads of WDT.STATUS that still report SYNCBUSY after a CTRLA write.
    busy: u8,
}

/// WDT clocks in a WDT.CTRLA PERIOD or WINDOW setting, 0 for off.
const fn wdt_clocks(setting: u8) -> u32 {
    match setting {
        0 => 0,
        n => 8 << (n - 1),
    }
}

/// When each step of `main`'s start-up first happened, in `Chip::now`, for
/// `boot_order`.
#[derive(Default)]
struct Boot {
    /// The first RSTCTRL.RSTFR write, `hw::take_reset_flags`.
    flags_cleared: Option<u64>,
    /// The first CRCSCAN.CTRLA write, `hw::flash_ok`.
    flash_checked: Option<u64>,
    /// The first fuse read, `hw::fuses`.
    fuses_read: Option<u64>,
    /// The first MVIO.STATUS read, `hw::vddio2_ok`.
    supply_read: Option<u64>,
    /// The first write that isn't one of those or PORTC's, so the start of
    /// `hw::init`.
    set_up: Option<u64>,
    /// The first RTC.PITCTRLA write with PITEN.
    pit_on: Option<u64>,
    /// The first WDT.CTRLA write, `hw::start_watchdog`.
    watchdog_on: Option<u64>,
}

impl Boot {
    /// Notes a register write at `now`.
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

/// CRCSCAN's state beyond its plain registers.
struct CrcScan {
    /// The instruction that last wrote ENABLE, and what STATUS read just
    /// before it.
    started: Option<(u64, u8)>,
    /// Reads of STATUS still to show BUSY.
    busy_reads: u8,
    /// STATUS once the scan's done. OK from reset (§30.5.3), before any scan,
    /// which is why the firmware has to wait for its own.
    status: u8,
}

/// ADC0's state beyond its plain registers.
#[derive(Default)]
struct Adc {
    /// The input and reference the last conversion used, to spot a switch.
    last_setup: Option<(u8, u8)>,
    /// The result of the last conversion started.
    result: u16,
    /// When that conversion finishes, while its result's unread.
    ready_at: Option<u64>,
    /// The high byte of RES, latched into ADC0.TEMP by reading the low byte.
    temp: Option<u8>,
}

/// DAC0, beyond DAC0.CTRLA in `Chip::io`.
#[derive(Default)]
struct Dac {
    /// The 10-bit code the output follows.
    code: u16,
    /// DAC0's TEMP, holding DATAL until DATAH is written.
    temp: Option<u8>,
    /// When the code last went to 0, which is when the output started
    /// falling there. Meaningless while it isn't 0.
    zero_since: u64,
}

/// TCB0, beyond the 8-bit registers in `Chip::io`.
#[derive(Default)]
struct Tcb {
    ccmp: u16,
    /// CNT as last written, or where it stopped.
    cnt: u16,
    /// TCB0's TEMP, and which register's write it's holding.
    temp: Option<(usize, u8)>,
    /// INTFLAGS.CAPT.
    capt: bool,
    /// When CAPT last went up from clear.
    capt_up_at: u64,
    /// When CNT next reaches CCMP, while it's running.
    next_capt: u64,
}

impl Tcb {
    /// Clocks in a period, counting 0 to CCMP.
    fn period(&self) -> u64 {
        u64::from(self.ccmp) + 1
    }
}

/// Bits of `Chip::interrupted`, one per peripheral with a TEMP the firmware
/// uses.
const TEMP_TCB0: u8 = 1 << 0;
const TEMP_DAC0: u8 = 1 << 1;
const TEMP_ADC0: u8 = 1 << 2;

struct Chip {
    flash: Vec<u8>,
    sram: Vec<u8>,
    /// Plain storage for the registers that are only storage.
    io: Vec<u8>,
    /// The time, counted an instruction at a time while the CPU's awake, and
    /// jumping on to the next event while it sleeps.
    now: u64,
    /// When something next happens, so most instructions needn't look.
    next_event: u64,
    /// The last instruction a protected register still takes a write on.
    unlocked_until: Option<u64>,
    /// Whether the CCP period's still holding interrupts off. The key's first
    /// store to the bus or `sleep` ends it.
    ccp_hold: bool,
    pressed: bool,
    pit_busy: u8,
    /// RTC.PITINTFLAGS.PI.
    pit_flag: bool,
    /// PORTD.INTFLAGS.
    portd_flags: u8,
    /// When the PIT's current period ends.
    next_pit: u64,
    /// Main clocks in a PIT period, which is the 32 kHz oscillator's speed
    /// against the main one's. `TICK_CYCLES` unless a run slows it.
    tick_cycles: u64,
    /// The 32 kHz oscillator has stopped. The PIT and the WDT both count it
    /// (§26.4.1.1, §22.3.2), so neither does anything more, and a period is
    /// only that much time going by. The firmware writes neither after
    /// start-up, so a sync left waiting on the oscillator isn't modelled.
    osc32k_stopped: bool,
    /// The last `sleep` ran with no interrupt enabled, so only a reset wakes
    /// the chip (§13.3.1), and this one has no resets.
    down: bool,
    /// PIT periods so far with the PIT on.
    ticks: u32,
    /// Whether the firmware's gone to sleep since this was cleared.
    tick_polled: bool,
    tcb: Tcb,
    dac: Dac,
    wdt: Wdt,
    crc: CrcScan,
    adc: Adc,
    /// RSTCTRL.RSTFR.
    rstfr: u8,
    /// What the ADC measures, whether VDDIO2 is there, and the fuses.
    world: World,
    /// When MVIO.STATUS.VDDIO2S last dropped, while it's still clear.
    vddio2_lost_at: Option<u64>,
    /// Whether the light was lit when it dropped, PC1 set in PORTC and DAC0
    /// above 0, so a run can tell it took VDDIO2 from a lit torch.
    lost_lit: bool,
    /// When RTC.PITINTFLAGS.PI was last cleared, which is the firmware
    /// taking a tick, and the start of the pass of the main loop that it
    /// wakes.
    tick_taken_at: u64,
    /// Takes VDDIO2 away at the worst moment for a firmware that looks once
    /// a pass, rather than as the tick begins (`Chip::enter`).
    late_vddio2: bool,
    /// VDDIO2 goes straight after the next read of MVIO.STATUS, or as the
    /// firmware next sleeps if it gets there without looking.
    vddio2_going: bool,
    /// A bit per vector whose interrupt is enabled.
    enabled: u64,
    /// A request `force_interrupt` put up, until the CPU takes it.
    forced: Option<u8>,
    /// Whether the instruction running is in a handler.
    in_handler: bool,
    /// Whether SREG.I was set as the instruction running started.
    interrupts_on: bool,
    /// The TEMPs the running handler caught halfway through an access, which
    /// it mustn't disturb (`TEMP_TCB0` and the rest).
    interrupted: u8,
    /// When PC1 last rose.
    pc1_rose_at: u64,
    /// When VREF.DAC0REF last changed.
    vref_changed_at: u64,
    /// Times `sleep` has run in power-down.
    power_downs: u32,
    /// Writes to anything but SRAM so far.
    register_writes: u32,
    boot: Boot,
    /// Holds the program to the first-light image's promises, no interrupt,
    /// no sleep, no watchdog and no timer (`first_light`), and nothing on
    /// TWI0, since first light leaves the AW2016 alone.
    bare: bool,
    /// Times PC1 has risen.
    pc1_rises: u32,
    /// TWI0's host and the AW2016 on its bus (`twi.rs`).
    twi: Twi,
    /// A test program that never drives the aux, so `in_step` doesn't hold
    /// it to them.
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
            interrupts_on: false,
            interrupted: 0,
            pc1_rose_at: 0,
            vref_changed_at: 0,
            power_downs: 0,
            register_writes: 0,
            boot: Boot::default(),
            bare: false,
            pc1_rises: 0,
            twi: Twi::new(),
            aux_unwired: false,
        }
    }

    /// Puts up a request for `vector`, from a peripheral the model doesn't
    /// have, for a run to see what the firmware does with an interrupt it
    /// never enabled. The CPU takes it like any other, at the next
    /// instruction boundary where SREG.I is set and nothing holds interrupts
    /// off (LVL0EX, `sei`, SPL or the CCP key), lowest vector first, pushing
    /// PC, leaving SREG alone and jumping to the vector's slot (`avr.rs`).
    ///
    /// This is a stand-in, and a one-shot one. It isn't in `enabled`, so it
    /// wakes nothing and the rule about enabling an interrupt with no handler
    /// doesn't see it, and it goes once it's taken (`Sim::watch_handler`). A
    /// real stray interrupt stays enabled and flagged, since `hw::halt`
    /// doesn't know which peripheral to clear. With LVL0EX set it can't be
    /// taken again, but whether a pending interrupt that's blocked like that
    /// (LVL0EX set, SREG.I clear) still wakes `halt`'s `sleep` isn't in the
    /// datasheet. Modelled as enabled, this chip would refuse that `sleep`
    /// for a question the datasheet doesn't answer, so it isn't. Either way
    /// PC1 is low before the first `sleep`, and `halt` sleeps in a loop, so
    /// the worst case is spinning dark rather than sleeping dark, until the
    /// watchdog resets the chip about 63 ms later.
    const fn force_interrupt(&mut self, vector: u8) {
        self.forced = Some(vector);
    }

    fn sram(&self, range: &Range<u16>) -> &[u8] {
        &self.sram[usize::from(range.start - SRAM.start)..usize::from(range.end - SRAM.start)]
    }

    /// PC1, as the boost converter sees it, with VDDIO2 there.
    fn enabled_high(&self) -> bool {
        self.io[reg::VPORTC_OUT] & self.io[reg::VPORTC_DIR] & ENABLE != 0
    }

    /// The power paths that are on, as a PORTA mask.
    fn paths(&self) -> u8 {
        self.io[reg::VPORTA_OUT] & self.io[reg::VPORTA_DIR] & PATHS
    }

    fn dac_reference(&self) -> u8 {
        self.io[reg::VREF_DAC0REF]
    }

    /// MVIO.STATUS.VDDIO2S. It reads 1 with single supply, since PORTC is on
    /// VDD then (§19.3.1).
    fn vddio2_ok(&self) -> bool {
        self.world.vddio2s()
    }

    /// Puts the chip in `world`, which a run does between passes. A change
    /// of VDDIO2 is the only part of it the chip has to act on.
    fn set_world(&mut self, world: World) -> Result<(), String> {
        let was = self.vddio2_ok();
        self.world = world;
        self.vddio2_moved(was)
    }

    /// Takes VDDIO2 away, or brings it back, now.
    fn set_vddio2(&mut self, there: bool) -> Result<(), String> {
        let was = self.vddio2_ok();
        self.world.scene.vddio2 = there;
        self.vddio2_moved(was)
    }

    /// `set_world` as a tick begins. With `late_vddio2`, a world that takes
    /// VDDIO2 away keeps it for now and arms `vddio2_goes` instead.
    fn enter(&mut self, mut world: World) -> Result<(), String> {
        if self.late_vddio2 && self.world.scene.vddio2 && !world.scene.vddio2 {
            world.scene.vddio2 = true;
            self.vddio2_going = true;
        }
        self.set_world(world)
    }

    /// Takes VDDIO2 away if `enter` armed it. The chip calls this straight
    /// after a read of MVIO.STATUS, which has already had its answer, and as
    /// the firmware sleeps. For a firmware that looks once a pass that's the
    /// longest it can go unseen, a whole tick and the pass that then finds
    /// it. For one that never looks it's the end of the pass, and nothing
    /// finds it. A firmware that looked more than once a pass would see this
    /// sooner than `in_step` tells the library, and this would need another
    /// way of choosing its moment.
    fn vddio2_goes(&mut self) -> Result<(), String> {
        if !self.vddio2_going {
            return Ok(());
        }
        self.vddio2_going = false;
        self.set_vddio2(false)
    }

    /// MVIO.STATUS.VDDIO2S has just changed, if `was` isn't what it reads
    /// now. Without VDDIO2 the MVIO pins are tri-stated, and when it comes
    /// back they "reload the current configuration of the PORT registers"
    /// (§19.3.2.1). So if PORTC still has PC1 high then, PC1 rises there and
    /// then, and it's held to everything a rise is. The datasheet gives no
    /// delay for either, so both are taken as at once.
    ///
    /// Nothing here says what the driver's enable does while PC1 is
    /// tri-stated. That's the board's, and nobody knows it. The worst case
    /// is that it stays wherever it was, so the firmware is held to what it
    /// still controls (`dark_without_vddio2`), and `enabled_high` goes on
    /// meaning what PORTC asks for.
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

    /// With MVIO.STATUS.VDDIO2S clear, PC1 is tri-stated whatever PORTC says
    /// (§19.3.2.1), so nothing the firmware writes holds the driver's enable,
    /// and DAC0 and the paths, which are on VDD, are all it has left. A pass
    /// of the main loop that took its tick with VDDIO2S already clear has
    /// them at 0 and low before it sleeps, with PC1 clear in PORTC as well,
    /// so the pin has nothing to take up if VDDIO2 comes back. That's a tick
    /// and a pass after it went, at most. A pass that was already under way
    /// when it went is let off, since it may have looked before then.
    ///
    /// It's the firmware's rule, not the datasheet's. One that waited for a
    /// second reading before acting would fail it, and that would be a
    /// choice to make here on purpose.
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

    /// Refuses a chip `hw::init` left on a clock or a timer the model doesn't
    /// have. Writes of anything else are refused already, so this is what
    /// catches a write that never happened.
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
        if self.io[reg::RTC_PITCTRLA] != regs::RTC_PERIOD_CYC512 | RTC_PITEN {
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

    /// Spends the CCP key on a write to protected register `a`, or refuses
    /// the write if there's no key to spend.
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

    /// Moves time on to `to`, and runs whatever falls due on the way. This is
    /// the one place simulated time moves.
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
                continue;
            }
            if self.io[reg::RTC_PITCTRLA] & RTC_PITEN != 0 {
                self.pit_flag = true;
                self.ticks += 1;
            }
            self.wdt_period()?;
        }
        self.plan_next_event();
        Ok(())
    }

    /// The open window in WDT clocks, which is the whole timeout in normal
    /// mode, or 0 with the WDT off.
    fn wdt_open(&self) -> u32 {
        wdt_clocks(self.io[reg::WDT_CTRLA] & WDT_PERIOD)
    }

    /// The closed window in WDT clocks, 0 in normal mode or before the first
    /// `wdr` starts window mode (§22.3.3.2).
    fn wdt_closed(&self) -> u32 {
        if self.wdt.windowed {
            wdt_clocks((self.io[reg::WDT_CTRLA] & WDT_WINDOW) >> 4)
        } else {
            0
        }
    }

    /// PIT periods that can still go by without a `wdr` before the watchdog
    /// bites, up to `DOWN_PERIODS`, which is all of them with it off.
    fn wdt_periods_left(&self) -> u64 {
        let timeout = self.wdt_closed() + self.wdt_open();
        if timeout == 0 {
            return DOWN_PERIODS;
        }
        let last_safe = (timeout - 1) / WDT_CLOCKS_PER_TICK;
        u64::from(last_safe.saturating_sub(self.wdt.ticks)).min(DOWN_PERIODS)
    }

    /// A PIT period's worth of WDT clocks, while the watchdog's on. It runs
    /// in every sleep mode (§22.3.4), so this counts asleep too. Nothing here
    /// resets the chip, so running out is the firmware failing.
    fn wdt_period(&mut self) -> Result<(), String> {
        if self.wdt_open() == 0 {
            return Ok(());
        }
        self.wdt.ticks += 1;
        let clocks = self.wdt.ticks * WDT_CLOCKS_PER_TICK;
        let timeout = self.wdt_closed() + self.wdt_open();
        if clocks >= timeout {
            return Err(format!(
                "the watchdog went {} ticks ({clocks} WDT clocks) without wdr, against a \
                 {timeout}-clock timeout, so the chip would have reset",
                self.wdt.ticks
            ));
        }
        Ok(())
    }

    /// CRCSCAN.STATUS as a read sees it now.
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

    /// Starts a whole-flash scan. That's CRC-16-CCITT over every byte of
    /// flash, erased bytes reading 0xFF, passing if it comes to 0.
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

    /// Starts a conversion, if the ADC's set up for one of the two
    /// measurements the firmware makes, and works out what it'll read and
    /// when.
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
            // Section 33.5.7 names the input and says nothing more about it.
            // It reads `testing::GROUND_READING`, the worst offset table
            // 38-24 allows, since a single-ended result can't go under 0.
            (ADC_MUXPOS_GND, VREF_REFSEL_1V024) => Channel::Ground,
            // VDDIO2 / 10, which section 33.5.7 names and says nothing more
            // about. It reads the world's VDDIO2, the cell unless a run says
            // otherwise, and 0 with VDDIO2 missing, which the datasheet
            // doesn't say either, so it's the reading furthest from a rail
            // that's there.
            (ADC_MUXPOS_VDDIO2DIV10, VREF_REFSEL_1V024) => Channel::Vddio2,
            (ADC_MUXPOS_TEMPSENSE, VREF_REFSEL_2V048) => {
                // Section 33.3.3.8 wants at least 25 µs of INITDLY and 28 µs
                // of SAMPLEN, in ADC clocks.
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
                     simulator only models VDD / 10, VDDIO2 / 10 and ground against 1.024 V \
                     and the temperature sensor against 2.048 V"
                ));
            }
        };
        let setup = Some((muxpos, reference));
        // VDDIO2 / 10 with VDDIO2 missing reads 0 (`Scene::reading` has why).
        self.adc.result = if self.adc.last_setup == setup {
            self.world.reading(channel)
        } else {
            UNSETTLED
        };
        self.adc.last_setup = setup;
        // Section 33.3.3.4.4's sum for four samples, plus INITDLY, which
        // only applies after an enable or a reference change but is counted
        // every time, so a result is never early. In main clocks, which the
        // model's time can only fall short of.
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
    }

    /// One instruction's worth of time.
    fn elapse(&mut self) -> Result<(), String> {
        self.advance_to(self.now + 1)
    }

    /// Whether the pins are doing what `output` asks: PC1, DAC0's code, its
    /// reference and the paths.
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

    /// DAC0 is at 0, which any change of reference or path wants with PC1
    /// high. `what` says which.
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

    /// DAC0's output is at 0 and has had time to get there, which a raised
    /// reference or an added path wants with PC1 high. `what` says which.
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

    /// Presses or releases the switch on PD4. A change sets PD4's flag in
    /// PORTD.INTFLAGS if its ISC asks for that edge (§18.3.3).
    ///
    /// `Sim::tick` only moves the switch between ticks, so an edge that's
    /// asked for always raises its flag.
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

    /// A bit per vector, a port's being set when any of its pins senses, the
    /// PIT's when its interrupt's on, and TCB0's when its interrupt's on and
    /// `tcb` says it counts.
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

    /// The TEMPs a handler landing now would catch halfway, as
    /// `Chip::interrupted` bits.
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

    /// What has to be true before `sleep` in power-down. The light has to be
    /// off, and nothing left running that power-down would leave stuck.
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
        Ok(())
    }

    /// A write to PORTx.PINnCTRL, refusing what the board can't take.
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

    /// A write to VPORTA.OUT or DIR, watching the paths.
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

    /// A write to VPORTC.OUT or DIR, watching what PC1 does.
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
            // A refused write changes nothing, so a test can carry on.
            self.io[a] = old;
        }
        moved
    }

    /// Checks PC1's rise or fall against the rules, once VPORTC's been
    /// written. `was` is whether it was high before.
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
            self.twi.catch_up(self.now, false)?;
            if self.twi.aw2016.lit() {
                return Err(
                    "raises PC1 with the aux lit: they go out before the main LED comes on \
                     (docs/ui.md)"
                        .into(),
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

    /// A write to VPORTD.OUT or DIR. Only PD6 is ever an output, and PORT
    /// only ever holds it low under DAC0.
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

    /// A write to VREF.DAC0REF, which only goes up with DAC0 quiet if PC1's
    /// high.
    fn write_dac_reference(&mut self, value: u8) -> Result<(), String> {
        if value != VREF_REFSEL_1V024 && value != VREF_REFSEL_2V500 {
            return Err(format!(
                "writes {value:#04x} to VREF.DAC0REF; the simulator only models 1.024 V and \
                 2.5 V, with ALWAYSON clear"
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

    /// DAC0's output changes to `code`, as DATAH's write lands.
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

    /// A write to TCB0.CTRLA, starting or stopping it.
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
                return Err(format!(
                    "writes {value:#04x} to TCB0.CTRLA; the simulator only models it off, or \
                     on and undivided"
                ));
            }
        }
        self.io[reg::TCB0_CTRLA] = value;
        self.plan_next_event();
        Ok(())
    }

    /// The firmware has finished writing a 16-bit TCB0 register.
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

    /// Refuses an access to `a` by a handler that caught the main loop
    /// halfway through a 16-bit register of the same peripheral, since they
    /// share its TEMP (§8.10.3).
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

/// CLK_PER's divisor for ADC0.CTRLC.PRESC, which is a table of named values
/// rather than a shift (§33.5.3).
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

/// Registers that are plain storage as far as the firmware's concerned.
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

/// Whether `addr` is one of PORTA's, PORTC's or PORTD's PINnCTRL.
/// Refuses a write first light promises never to make, to the watchdog, or
/// starting TCB0 or the PIT. Enabling an interrupt is refused in `Sim::step`,
/// whichever register does it.
fn bare_write(a: usize, value: u8) -> Result<(), String> {
    match a {
        reg::WDT_CTRLA => Err("writes WDT.CTRLA, and first light has no watchdog".into()),
        reg::TCB0_CTRLA if value & TCB_ENABLE != 0 => {
            Err("starts TCB0, and first light has no timers".into())
        }
        reg::RTC_PITCTRLA if value & RTC_PITEN != 0 => {
            Err("starts the PIT, and first light has no timers".into())
        }
        // `hw::halt` turns TWI0's host off, which talks to nobody.
        reg::TWI0_MCTRLA if value == 0 => Ok(()),
        _ if twi(a) => Err(format!(
            "writes {}, and first light leaves the AW2016 alone",
            name(a)
        )),
        _ => Ok(()),
    }
}

/// Whether `addr` is one of TWI0's registers.
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

/// Whether `addr` is one of PORTF's registers, or VPORTF's.
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

/// The registers that decide what a conversion measures. Changing one
/// mid-conversion gives unpredictable results (§33.3.2) or isn't modelled.
const ADC_SETUP: [usize; 7] = [
    reg::VREF_ADC0REF,
    reg::ADC0_CTRLA,
    reg::ADC0_CTRLB,
    reg::ADC0_CTRLC,
    reg::ADC0_CTRLD,
    reg::ADC0_SAMPCTRL,
    reg::ADC0_MUXPOS,
];

/// The fuses and the lock bytes after them.
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

impl Bus for Chip {
    fn fetch(&self, pc: u32) -> Option<u16> {
        word(&self.flash, pc as usize * 2)
    }

    fn wdr(&mut self) -> Result<(), String> {
        if self.bare {
            return Err("runs `wdr`, and first light has no watchdog".into());
        }
        if self.wdt_open() == 0 {
            return Ok(());
        }
        if self.wdt.busy > 0 {
            return Err("wdr before the WDT.CTRLA write has synced".into());
        }
        let clocks = self.wdt.ticks * WDT_CLOCKS_PER_TICK;
        let closed = self.wdt_closed();
        if closed > 0 && clocks <= closed {
            return Err(format!(
                "wdr {} ticks ({clocks} WDT clocks) after the last, in or on the edge of \
                 the {closed}-clock closed window, so the chip would have reset",
                self.wdt.ticks
            ));
        }
        self.wdt.ticks = 0;
        self.wdt.windowed = self.io[reg::WDT_CTRLA] & WDT_WINDOW != 0;
        Ok(())
    }

    /// Lowest vector first, the default static priority (§15.3.2.4.3). A
    /// port's request is a flag on a pin whose ISC senses, and the PIT's and
    /// TCB0's are the flag and the enable together. So is `force_interrupt`'s.
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

    /// Interrupts are ignored for the four instructions after the key, and
    /// left pending (§7.6.1). That the store or `sleep` which ends the
    /// protected change ends this too is likely, not stated.
    fn holds_interrupts(&self) -> bool {
        self.ccp_hold && self.unlocked_until.is_some_and(|until| self.now <= until)
    }

    /// With an interrupt enabled that can't wake it from the mode it's in,
    /// that's a mistake. With none enabled at all, it's the chip stopping for
    /// good, since only a reset wakes it (§13.3.1). That sets `down`, which is
    /// for whoever's running the chip to judge.
    fn sleep(&mut self) -> Result<bool, String> {
        if self.bare {
            return Err("runs `sleep`, and first light never sleeps".into());
        }
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
            // Every pin senses asynchronously (section 18.3.3.1), and TCB0
            // is halted (table 13-4).
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

    /// Jumps to the next thing that happens. TCB0 halts in power-down
    /// (§24.3.6), so its count waits for it there.
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
            // Erased flash past the end of the image reads as 0xFF.
            return Ok(*self.flash.get(usize::from(addr - MAPPED)).unwrap_or(&0xFF));
        }
        match a {
            // PD4 reads through its input buffer, against the board's own
            // pull-up, so a released switch reads 1 and a pressed one 0. IN
            // follows the pin at once here. The real one stops following it
            // in power-down, since input synchronisation runs off CLK_PER
            // (section 18.3.5), and the datasheet doesn't say how soon after
            // a wake it's caught up.
            reg::VPORTD_IN => {
                if self.io[SWITCH_CTRL] & PORT_ISC == ISC_INPUT_DISABLE {
                    return Err(
                        "reads PD4 with its input buffer off, so VPORTD.IN never changes \
                         (section 18.3.2.3)"
                            .into(),
                    );
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
                let busy = self.wdt.busy > 0;
                self.wdt.busy = self.wdt.busy.saturating_sub(1);
                Ok(if busy { WDT_SYNCBUSY } else { 0 })
            }
            reg::CRCSCAN_STATUS => Ok(self.crc_status()),
            reg::RSTCTRL_RSTFR => Ok(self.rstfr),
            reg::ADC0_INTFLAGS => {
                let ready = self.adc.ready_at.is_some_and(|at| self.now >= at);
                // A dead ADC never sets RESRDY.
                Ok(if ready && !self.world.scene.adc_dead {
                    ADC_RESRDY
                } else {
                    0
                })
            }
            reg::ADC0_RES => {
                // Reading the low byte latches the high byte into ADC0.TEMP
                // (section 8.10.3) and clears RESRDY (section 33.5.12).
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
            // The calibration is two 16-bit words, low byte first (section
            // 8.6.2.2).
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
        if self.bare {
            bare_write(a, value)?;
        }
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
                    return Err(format!(
                        "writes {value:#04x} to DAC0.CTRLA; the simulator only models it off, \
                         or on and driving PD6"
                    ));
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
                    return Err(format!(
                        "writes {value:#04x} to TCB0.CTRLB; the simulator only models periodic \
                         interrupt mode with no output"
                    ));
                }
                self.io[a] = value;
            }
            reg::TCB0_INTCTRL => {
                if value & !TCB_CAPT != 0 {
                    return Err(format!(
                        "writes {value:#04x} to TCB0.INTCTRL; the simulator only models CAPT"
                    ));
                }
                // The request is the flag and the enable together (section
                // 24.3.5), and the flag goes up whether the interrupt's on or
                // not, so one left up from before fires at once. One that went
                // up since a clear, within a period, is a real one, and fine.
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
                self.io[a] = value;
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
                if self.wdt.busy > 0 {
                    return Err("writes WDT.CTRLA while WDT.STATUS.SYNCBUSY is set, which \
                                the datasheet doesn't allow (section 22.3.6)"
                        .into());
                }
                if self.wdt_open() > 0 {
                    return Err(format!(
                        "writes {value:#04x} to WDT.CTRLA with the watchdog already on, \
                         which the simulator doesn't model, and off isn't allowed"
                    ));
                }
                if value & WDT_PERIOD > WDT_PERIOD_MAX || (value & WDT_WINDOW) >> 4 > WDT_PERIOD_MAX
                {
                    return Err(format!(
                        "writes {value:#04x} to WDT.CTRLA: a reserved WDT period or window"
                    ));
                }
                self.io[a] = value;
                self.wdt = Wdt {
                    busy: WDT_SYNC_READS,
                    ..Wdt::default()
                };
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
                    return Err(format!(
                        "writes {value:#04x} to CRCSCAN.CTRLA; the simulator only models \
                         ENABLE, with STATUS polled (NMIEN can't be undone without a reset)"
                    ));
                }
                self.start_crc()?;
                self.io[a] = value;
            }
            reg::ADC0_COMMAND => match value {
                0 => {}
                ADC_STCONV => self.start_conversion()?,
                _ => {
                    return Err(format!(
                        "writes {value:#04x} to ADC0.COMMAND; the simulator only models STCONV"
                    ));
                }
            },
            reg::ADC0_INTFLAGS => {
                // Writing a 1 clears RESRDY. It only clears the flag, so a
                // conversion that's still running finishes and sets it again.
                if value & ADC_RESRDY != 0 && !self.converting() {
                    self.adc.ready_at = None;
                }
            }
            _ if ADC_SETUP.contains(&a) && self.converting() => {
                return Err(format!("writes {} mid-conversion", name(a)));
            }
            reg::VREF_ADC0REF => {
                if value != VREF_REFSEL_1V024 && value != VREF_REFSEL_2V048 {
                    return Err(format!(
                        "writes {value:#04x} to VREF.ADC0REF; the simulator only models \
                         1.024 V and 2.048 V, with ALWAYSON clear"
                    ));
                }
                self.io[a] = value;
            }
            reg::ADC0_CTRLA => {
                if value != 0 && value != ADC_ON {
                    return Err(format!(
                        "writes {value:#04x} to ADC0.CTRLA; the simulator only models it off, \
                         or on for single 12-bit conversions"
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

    /// A write of `value` to register `a`.
    fn poke(chip: &mut Chip, a: usize, value: u8) -> Result<(), String> {
        chip.write(a as u16, value)
    }

    /// The error from a write of `value` to register `a`.
    fn refused(chip: &mut Chip, a: usize, value: u8) -> String {
        poke(chip, a, value).expect_err("the write went through")
    }

    /// A write of `value` to protected register `a`, with the key just before.
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

    /// A chip with the pins, DAC0 and TCB0 set up as `hw::init` leaves them,
    /// the light off.
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

    /// `dark`, with TCB0 started as `set_output` starts it and PC1 raised,
    /// then the boost converter's 8 ms gone by.
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
        // OUT before DIR finds it the same.
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
        // Straight after DAC0 goes to 0 is fine, as the off order has it.
        dac(&mut chip, 0).unwrap();
        poke(&mut chip, reg::VREF_DAC0REF, VREF_REFSEL_1V024).unwrap();
        poke(&mut chip, reg::VPORTA_OUT, Paths::Low.mask()).unwrap();
        // With PC1 low nothing's asked for at all.
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
        // 0x3FF << 6 is 0xFFC0, the top ten bits.
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
        // Fresh from reset, the PIT still reads busy.
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

    /// The world's fuses with MVSYSCFG at single supply.
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
        // With single supply it reads 1 whatever VDDIO2 does, so the chip
        // never counts it as lost.
        chip.set_world(single_supply(World::NORMAL)).unwrap();
        chip.set_vddio2(false).unwrap();
        assert_eq!(read(&mut chip), Ok(MVIO_VDDIO2S));
        assert_eq!(chip.vddio2_lost_at, None);
    }

    /// The firmware taking a tick, as `hw::rtc_pit` does.
    fn take_tick(chip: &mut Chip) {
        chip.pit_flag = true;
        poke(chip, reg::RTC_PITINTFLAGS, RTC_PI).unwrap();
    }

    /// `lit`, at a level on the high path, set up to sleep in idle as the
    /// firmware does with the light on.
    fn lit_and_idling() -> Chip {
        let mut chip = lit();
        poke(&mut chip, reg::VPORTA_OUT, PATHS).unwrap();
        dac(&mut chip, 300).unwrap();
        poke(&mut chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        poke(&mut chip, reg::SLPCTRL_CTRLA, SLEEP_IDLE | SLPCTRL_SEN).unwrap();
        chip
    }

    /// The driver off in the firmware's order, with TCB0 stopped.
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
        // Gone before the tick's taken, so this pass could have seen it.
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
        // Off in PORTC, DAC0 and the paths, and it may.
        turn_off(&mut chip);
        assert_eq!(chip.sleep(), Ok(true));

        // PC1 clear in PORTC isn't enough, since it's the one pin that isn't
        // listening. The paths are still up here.
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

    /// With single supply the firmware can't see VDDIO2 go, so nothing's
    /// asked of it.
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
        // Still lit in the registers, so PC1 comes up with DAC0 live.
        let mut chip = lit_and_idling();
        chip.set_vddio2(false).unwrap();
        let err = chip.set_vddio2(true).unwrap_err();
        assert!(
            err.contains("VDDIO2 comes back with PC1 still set in PORTC")
                && err.contains("raises PC1 with DAC0 at 300"),
            "{err}"
        );
        // Off in the registers, and it comes back to nothing.
        let mut chip = lit_and_idling();
        chip.set_vddio2(false).unwrap();
        turn_off(&mut chip);
        let rises = chip.pc1_rises;
        chip.set_vddio2(true).unwrap();
        assert_eq!(chip.pc1_rises, rises);
        // PC1 set with DAC0 at 0 and the paths low is a rise like any other,
        // and DAC0 waits for the boost converter again.
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
        // Not late, it's gone as the tick begins.
        let mut chip = lit_and_idling();
        chip.enter(lost).unwrap();
        assert!(!chip.vddio2_ok());
        // Late, the look still sees it, and it's gone straight after.
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
        // The next tick's world still has it gone, and nothing's armed.
        chip.enter(lost).unwrap();
        assert!(!chip.vddio2_going && !chip.vddio2_ok());
        // With no look at all, it goes as the pass sleeps.
        let mut chip = lit_and_idling();
        chip.late_vddio2 = true;
        chip.enter(lost).unwrap();
        take_tick(&mut chip);
        assert_eq!(chip.sleep(), Ok(true));
        assert!(!chip.vddio2_ok());
        // Coming back is never late.
        chip.late_vddio2 = true;
        turn_off(&mut chip);
        chip.enter(World::NORMAL).unwrap();
        assert!(chip.vddio2_ok());
    }

    /// A chip with the ADC set up as `hw::init` and `adc_channel` do.
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

    /// Instructions a conversion as `adc_ready` sets it up takes: 32 ADC
    /// clocks of INITDLY and four samples of 2 + 32 + 13.5, at four main
    /// clocks each, and four main clocks more.
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
        // 3.8 V and 25 °C, as the library's tests have them, and ground at
        // the worst offset table 38-24 allows.
        let voltage = World::NORMAL.reading(Channel::Voltage);
        assert_eq!(kr1k::battery::centivolts(voltage), 380);
        assert_eq!(World::NORMAL.reading(Channel::Temperature), 8603);
        assert_eq!(World::NORMAL.reading(Channel::Ground), 20);
    }

    /// Ground reads against 1.024 V, as the voltage does, and nothing else.
    #[test]
    fn ground_only_converts_against_1_024_v() {
        let mut chip = adc_ready(ADC_MUXPOS_GND);
        poke(&mut chip, reg::VREF_ADC0REF, VREF_REFSEL_2V048).unwrap();
        let err = start(&mut chip).unwrap_err();
        assert!(err.contains("MUXPOS 0x40"), "{err}");
    }

    /// A stuck ADC reads the same for every input.
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
            // 4 MHz / 2 is 2 MHz, which is fine, and / 64 is 62.5 kHz.
            (reg::ADC0_CTRLC, regs::ADC_PRESC_DIV64, "at 62500 Hz"),
            (
                reg::VREF_ADC0REF,
                VREF_REFSEL_2V048,
                "VDD / 10, VDDIO2 / 10 and ground against 1.024 V",
            ),
            (
                reg::ADC0_MUXPOS,
                regs::ADC_MUXPOS_AIN1,
                "VDD / 10, VDDIO2 / 10 and ground against 1.024 V",
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

    /// Section 33.3.3.8, at a 1 MHz ADC clock.
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
            // VDD / 10 needs neither.
            let mut chip = adc_ready(ADC_MUXPOS_VDDDIV10);
            poke(&mut chip, a, value).unwrap();
            start(&mut chip).unwrap();
        }
        let mut chip = adc_ready(ADC_MUXPOS_TEMPSENSE);
        poke(&mut chip, reg::ADC0_SAMPCTRL, 28).unwrap();
        start(&mut chip).unwrap();
    }

    /// A chip whose watchdog runs 32 WDT clocks closed and 32 open, two ticks
    /// each, and has synced.
    fn watched() -> Chip {
        let mut chip = Chip::new(Vec::new());
        unlocked(&mut chip, reg::WDT_CTRLA, 0x33).unwrap();
        chip.wdt.busy = 0;
        chip
    }

    fn ticks(chip: &mut Chip, n: u64) -> Result<(), String> {
        chip.advance_to(chip.now + n * TICK_CYCLES)
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
    fn going_without_a_feed_bites() {
        let mut chip = watched();
        let err = ticks(&mut chip, 2).unwrap_err();
        assert!(err.contains("without wdr"), "{err}");
    }

    #[test]
    fn the_watchdog_wont_be_turned_off_or_fed_before_it_syncs() {
        let mut chip = watched();
        let err = unlocked(&mut chip, reg::WDT_CTRLA, 0).unwrap_err();
        assert!(err.contains("already on"), "{err}");
        let mut chip = Chip::new(Vec::new());
        unlocked(&mut chip, reg::WDT_CTRLA, 0x33).unwrap();
        assert!(chip.wdr().unwrap_err().contains("synced"));
        let err = unlocked(&mut chip, reg::WDT_CTRLA, 0x33).unwrap_err();
        assert!(err.contains("SYNCBUSY is set"), "{err}");
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
        // 64 clocks, four ticks, the fourth being the bite.
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
        // Long past the watchdog's 64 clocks.
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
        // The period under way at the change runs out as it was.
        ticks(&mut chip, 1).unwrap();
        assert_eq!(chip.ticks, 1);
        wait(&mut chip, SLOWEST_TICK_CYCLES - 1);
        assert_eq!(chip.ticks, 1);
        chip.elapse().unwrap();
        assert_eq!(chip.ticks, 2);
        // 30% slow against 5% fast, in TCB0 periods.
        assert_eq!(SLOWEST_TICK_CYCLES.div_ceil(TIMER_PERIOD), 24);
    }

    #[test]
    fn the_pits_setup_waits_for_ctrlbusy() {
        let mut chip = Chip::new(Vec::new());
        let err = refused(&mut chip, reg::RTC_PITCTRLA, RTC_PITEN);
        assert!(err.contains("CTRLBUSY is set"), "{err}");
        while chip.read(reg::RTC_PITSTATUS as u16).unwrap() != 0 {}
        poke(&mut chip, reg::RTC_PITCTRLA, RTC_PITEN).unwrap();
        // Busy again after the write.
        assert!(poke(&mut chip, reg::RTC_PITCTRLA, RTC_PITEN).is_err());
    }

    #[test]
    fn the_scan_passes_a_signed_image_and_fails_a_changed_one() {
        let signed = crc::sign(&[0x12, 0x34], &stamp::TEST).unwrap();
        let mut bad = signed.clone();
        bad[0] ^= 1;
        for (image, want) in [(signed, CRCSCAN_OK), (bad, 0)] {
            let mut chip = Chip::new(image);
            poke(&mut chip, reg::CRCSCAN_CTRLA, CRCSCAN_ENABLE).unwrap();
            // OK from reset, until the scan starts three cycles later.
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
        chip.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | RTC_PITEN;
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
    fn first_light_never_sleeps_feeds_a_watchdog_or_starts_a_timer() {
        let mut chip = dark();
        chip.bare = true;
        poke(&mut chip, reg::SLPCTRL_CTRLA, SLEEP_IDLE | SLPCTRL_SEN).unwrap();
        assert!(chip.sleep().unwrap_err().contains("never sleeps"));
        assert!(chip.wdr().unwrap_err().contains("no watchdog"));
        let e = unlocked(&mut chip, reg::WDT_CTRLA, WDT_PERIOD_MAX).unwrap_err();
        assert!(e.contains("no watchdog"), "{e}");
        assert!(refused(&mut chip, reg::TCB0_CTRLA, TCB_RUNNING).contains("no timers"));
        let pit = regs::RTC_PERIOD_CYC512 | RTC_PITEN;
        assert!(refused(&mut chip, reg::RTC_PITCTRLA, pit).contains("no timers"));
        // Stopping them is fine, as `hw::halt` does.
        poke(&mut chip, reg::TCB0_CTRLA, 0).unwrap();
    }

    #[test]
    fn a_register_it_doesnt_model_is_an_error() {
        let mut chip = Chip::new(Vec::new());
        // SPI0.CTRLA, which nothing on this board needs.
        let spi = reg::SPI0_CTRLA as u16;
        assert!(chip.write(spi, 0).unwrap_err().contains("doesn't model"));
        assert!(chip.read(spi).unwrap_err().contains("doesn't model"));
        // TWI0's client side isn't modelled either, nor its host's MCTRLA
        // read back.
        let client = reg::TWI0_CTRLA as u16 + 0x09;
        assert!(chip.write(client, 0).unwrap_err().contains("doesn't model"));
        let mctrla = reg::TWI0_MCTRLA as u16;
        assert!(chip.read(mctrla).unwrap_err().contains("doesn't model"));
        assert!(chip.write(MAPPED, 0).unwrap_err().contains("flash"));
    }

    /// A handler that catches the main loop halfway through DAC0.DATA can't
    /// touch DAC0's 16-bit registers, and can touch anything else.
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

    // From here on the tests drive the CPU too.

    const NOP: u16 = 0x0000;
    const SEI: u16 = 0x9478;
    const SEC: u16 = 0x9408;
    const SLEEP: u16 = 0x9588;
    const RETI: u16 = 0x9518;
    /// `push r0`.
    const PUSH_R0: u16 = 0x920F;
    /// `rjmp` to itself.
    const SPIN: u16 = 0xCFFF;

    /// `jmp`, to a byte address in the first 128 KB.
    fn jmp(to: u32) -> [u16; 2] {
        [0x940C, (to / 2) as u16]
    }

    /// `sts a, r16`.
    fn sts_r16(a: usize) -> [u16; 2] {
        [0x9300, a as u16]
    }

    /// `out a, r16`, for an I/O address.
    fn out_r16(a: usize) -> u16 {
        0xB800 | (a as u16 & 0x30) << 5 | 16 << 4 | a as u16 & 0x0F
    }

    /// `st X, r16`.
    const ST_X_R16: u16 = 0x930C;

    fn image(words: impl IntoIterator<Item = u16>) -> Vec<u8> {
        words.into_iter().flat_map(u16::to_le_bytes).collect()
    }

    /// A chip running `words` from reset, with `__unhandled_interrupt` and
    /// `hw::halt` somewhere it never gets to.
    fn sim(words: impl IntoIterator<Item = u16>) -> Sim {
        let ends = Ends {
            unhandled: 0x1000,
            halt: 0x2000,
        };
        Sim::new(image(words), SRAM.start, ends).unwrap()
    }

    /// Runs `words` to their end, with r16 at 0 and X at GPR0.
    fn stores(words: Vec<u16>) -> Result<(), String> {
        let end = words.len() as u32;
        let mut sim = sim(words);
        sim.cpu.r[26] = reg::GPR_GPR0 as u8;
        while sim.cpu.pc < end {
            sim.step()?;
        }
        Ok(())
    }

    /// Errata 2.2.3, with GPR1 standing for anything under 64.
    #[test]
    fn a_store_under_64_straight_after_one_over_64_is_lost() {
        let high = sts_r16(reg::RTC_PITINTFLAGS);
        let low = sts_r16(reg::GPR_GPR1);
        let lost = |words: Vec<u16>| stores(words).unwrap_err();
        let err = lost([high, low].concat());
        assert!(
            err.contains("GPR.GPR1 straight after a store to 64 or over"),
            "{err}"
        );
        let err = lost([&high[..], &[ST_X_R16]].concat());
        assert!(err.contains("GPR.GPR0 straight after"), "{err}");
        let err = lost([high, sts_r16(reg::SLPCTRL_CTRLA)].concat());
        assert!(err.contains("SLPCTRL.CTRLA straight after"), "{err}");
        // The workarounds.
        stores([&high[..], &[NOP], &low[..]].concat()).unwrap();
        stores([&high[..], &[out_r16(reg::GPR_GPR1)]].concat()).unwrap();
        stores([&high[..], &[NOP], &sts_r16(reg::SLPCTRL_CTRLA)[..]].concat()).unwrap();
        // Two stores over 64 are fine.
        stores([high, sts_r16(reg::RTC_PITINTFLAGS)].concat()).unwrap();
    }

    #[test]
    fn found_lit_wants_out_cleared_by_the_first_register_write() {
        // r16 is PC1, for DIR, and r17 is 0, for OUT.
        let out = |a: usize, r: u16| 0xB800 | (a as u16 & 0x30) << 5 | r << 4 | a as u16 & 0x0F;
        let run = |writes: [u16; 2]| {
            let image = image([ldi(16, ENABLE), ldi(17, 0)].into_iter().chain(writes));
            found_lit(Sim::new(image, SRAM.start, Ends::default())?)
        };
        let dir = out(reg::VPORTC_DIR, 16);
        let clear = out(reg::VPORTC_OUT, 17);
        assert_eq!(run([clear, dir]), Ok(()));
        let err = run([dir, clear]).unwrap_err();
        assert!(err.contains("leaves PC1 set"), "{err}");
        // Neither write, so it runs off the end of the image.
        assert!(run([ldi(16, 0), ldi(17, 0)]).is_err());
    }

    /// Where `forced` puts vector 15's slot, as a byte address.
    const SLOT_15: u32 = 15 * VECTOR_WORDS * 2;

    /// `sei`, then `nop`s up to vector 15's slot, which writes DAC0.DATA with
    /// r1, low byte then high, with `halt` at `halt`.
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
            // The main loop's halfway through DAC0.DATA.
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

    /// Three bytes of .data, loaded from the start of flash, then two of .bss.
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

    /// `call`, to word 0.
    const CALL: [u16; 2] = [0x940E, 0];

    /// The `ldi`s src/avr/rt.rs's startup loads `layout` with, in its order.
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

        // The loads in another order, which is some other startup code.
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

    /// Words up to `vector`'s slot, starting with `sei`, then a `jmp` in the
    /// slot to `handler`, which follows the table.
    fn handled_by(vector: u8, handler: &[u16]) -> Vec<u16> {
        let slot = usize::from(vector) * VECTOR_WORDS as usize;
        let after = VECTORS * VECTOR_WORDS as usize;
        let mut words = vec![NOP; after];
        words[0] = SEI;
        words[slot..slot + 2].copy_from_slice(&jmp(after as u32 * 2));
        words.extend_from_slice(handler);
        words
    }

    /// Runs a PORTD handler made of `body` and a `reti`, as far as its
    /// return, with PD4's flag up.
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

    /// The PIT's slot holds a `jmp`, so it counts as handled, but the slot
    /// itself is where `__unhandled_interrupt` is.
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

    /// `[first, sleep]` with SLPCTRL set for idle and nothing enabled, as far
    /// as the end of the tick.
    fn stop_for_good(first: u16) -> Result<u64, String> {
        let mut sim = sim([first, SLEEP]);
        sim.chip.io[reg::SLPCTRL_CTRLA] = SLEEP_IDLE | SLPCTRL_SEN;
        sim.tick(false)
    }

    /// `cli` then `sleep`, as `hw::halt` does it.
    #[test]
    fn a_pass_that_stops_for_good_fails() {
        let err = stop_for_good(0x94F8).unwrap_err();
        assert!(err.contains("stopped for good"), "{err}");
    }

    /// `sei` then `sleep`, so only the enables being off keep it down.
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

    /// A start-up in the order `main` does it, each step at its place in
    /// line.
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

    /// The chip notes each step as `main` gets to it, and PORTC's writes and
    /// the scan's setup aren't setting the chip up.
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
        poke(&mut chip, reg::RTC_PITCTRLA, RTC_PITEN).unwrap();
        let watchdog = step(&mut chip);
        unlocked(&mut chip, reg::WDT_CTRLA, 0x33).unwrap();

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

    /// The least a firmware can be and still tick. It sleeps in idle, and its
    /// PIT handler clears the flag with r16, which is set up beforehand so
    /// the handler has nothing to give back. The chip is left as `hw::init`
    /// would have it, the light off, with a watchdog too long to need feeding
    /// here.
    fn ticking() -> Sim {
        let handler = sts_r16(reg::RTC_PITINTFLAGS).into_iter().chain([RETI]);
        let mut words = handled_by(VECTOR_RTC_PIT, &handler.collect::<Vec<_>>());
        // `sleep`, then back to it.
        words[1] = SLEEP;
        words[2] = SPIN - 1;
        let mut sim = sim(words);
        sim.cpu.r[16] = RTC_PI;
        let mut chip = dark();
        poke(&mut chip, SWITCH_CTRL, ISC_INTDISABLE).unwrap();
        poke(&mut chip, reg::RTC_PITINTCTRL, RTC_PI).unwrap();
        chip.pit_busy = 0;
        poke(
            &mut chip,
            reg::RTC_PITCTRLA,
            regs::RTC_PERIOD_CYC512 | RTC_PITEN,
        )
        .unwrap();
        poke(&mut chip, reg::SLPCTRL_CTRLA, SLEEP_IDLE | SLPCTRL_SEN).unwrap();
        unlocked(&mut chip, reg::WDT_CTRLA, WDT_PERIOD_MAX).unwrap();
        chip.flash = std::mem::take(&mut sim.chip.flash);
        // A few instructions that never drive the aux.
        chip.aux_unwired = true;
        sim.chip = chip;
        sim
    }

    /// Ticks with the switch left alone and nothing wrong.
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

    /// A torch that's off rests, and the library has it power down, which
    /// `ticking` never does.
    #[test]
    fn in_step_holds_the_sleep_to_the_librarys() {
        let err = in_step_fails(&mut ticking(), 8);
        assert!(
            err.contains("the library says power down, but the firmware idled"),
            "{err}"
        );
    }

    /// The light's off, as the library says, but DAC0's reference has been
    /// left at 2.5 V, which the library's off doesn't have.
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

    /// The watchdog runs while the chip sleeps, and `ticking` never feeds it.
    #[test]
    fn a_firmware_that_never_feeds_the_watchdog_is_bitten() {
        let mut sim = ticking();
        // 64 clocks, four ticks, the fourth being the bite.
        sim.chip.io[reg::WDT_CTRLA] = regs::WDT_PERIOD_64CLK;
        for _ in 0..3 {
            sim.tick(false).unwrap();
        }
        let err = sim.tick(false).unwrap_err();
        assert!(err.starts_with("asleep: the watchdog went 4"), "{err}");
    }

    #[test]
    fn a_failed_run_says_which_it_was_and_whether_interrupts_were_landing() {
        let power_on = || Ok(ticking());
        let failed = "the library says power down, but the firmware idled";
        // Ticks 0 to 3 read the cell and VDDIO2, the check every torch makes
        // on its first tick, so the library idles too. Tick 4's the first it
        // rests.
        let err = run(&power_on, None, Landing::Watch).err().unwrap();
        assert_eq!(err, format!("random switching: tick 4: {failed}"));
        let err = run(&power_on, None, Landing::Until(5)).err().unwrap();
        assert_eq!(
            err,
            format!("with interrupts mid-pass: random switching: tick 4: {failed}")
        );

        let example = spec::examples(spec::UI_MD)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let err = run(&power_on, Some(&example), Landing::Watch)
            .err()
            .unwrap();
        assert!(err.starts_with(&format!("{}: tick ", example.row)), "{err}");
    }

    /// The forced vector has to be one the firmware leaves unhandled, or the
    /// run would show nothing about `__unhandled_interrupt`.
    #[test]
    fn the_forced_interrupt_has_to_be_one_with_no_handler() {
        let err = unhandled_interrupt(sim(handled_by(VECTOR_UNHANDLED, &[RETI]))).unwrap_err();
        assert_eq!(
            err,
            "TCB1_INT (vector 15) has a handler now, so pick another vector to force"
        );
    }
}
