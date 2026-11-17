//! What the cell and the temperature allow, with the UI's level in and the
//! level to run at out.
//!
//! Each tick this runs the ADC's [`Schedule`], hands each reading to
//! low-voltage protection (`battery::Protection`) or thermal regulation
//! (`thermal::Thermal`), and runs the light at the lowest of the UI's level,
//! the thermal ceiling and the battery ceiling. Both ceilings fade
//! (`gradual::Ceiling`), so a step-down never jumps.
//!
//! Two things stop the light altogether, and [`Limits::health`] says which.
//! A flat cell turns it off and keeps it off until the cell reads as charged
//! (`battery::Protection::flat`). A fault turns it off and keeps it off until
//! the cell comes out. A fault is a failed power-on self-test, a cell that
//! can't be read (`battery::Verdict::Blind`), since with no reading there's
//! no low-voltage cut-off, and this cell has no protection of its own, or
//! VDDIO2 going missing. An ADC that reads ground as anything but near zero
//! (`sensors::Ground`) can't read the cell either, however believable its
//! voltage readings look, so that's the same fault as a cell it can't read.
//! A cell that can't be read with the light on fades it down the ramp first,
//! so it doesn't snap out in someone's hand.
//!
//! VDDIO2 going missing is the one fault with no fade. PC1, the driver's
//! enable, is on that supply and tri-stated without it, so there's nothing
//! left holding the enable to fade with, and the light's level is 0 on the
//! tick that reads it missing ([`Limits::tick`]).
//!
//! A collapsing cell snaps it out too. While the light's on, every voltage reading
//! the schedule has, the four-a-second ones and the quick ones between them,
//! also goes to `battery::Protection::quick`, and two in a row under
//! `battery::COLLAPSE_BELOW` turn the light off that tick, as a flat cell,
//! with no fade and whatever else is going on, a fault's fade included. VDD
//! is the cell and nothing resets the chip as it sags, so by then a fade is
//! time the MCU may not have. The quick readings go nowhere else.
//!
//! Garbage readings fail safe. Each side has its own idea of a reading that
//! can't be real (`battery::plausible`, `thermal::plausible`) and lets a few
//! go. After that a garbage voltage is a fault, and a garbage temperature
//! drops the thermal ceiling to its floor and does no more.

use crate::battery::{self, Protection, Verdict};
use crate::ramp::CEILING;
use crate::selftest::Faults;
use crate::sensors::{Convert, Ground, Reading, Schedule, Supplies};
use crate::temp::Calibration;
use crate::thermal::Thermal;

/// How many ticks of a stretch of light start a conversion that
/// `battery::Protection::quick` never sees, the first two.
///
/// A conversion starts just before its tick's output is set and samples for
/// about 200 µs after it. The first tick of a light raises PC1 with the DAC
/// at zero, and the second brings the DAC up to the level, so the
/// conversions those two start each run across a load landing on the cell.
/// What the boost converter draws as it starts on this board is unmeasured,
/// and a sag as a load lands isn't a cell collapsing, so neither reading
/// counts. From the third tick the load's been there for a whole tick,
/// 15.6 ms, by the time a conversion starts. The ramp and the ceilings only
/// ever move a level a tick after that, which is a small step each time, and
/// those readings count. Unmeasured.
pub const LANDING_TICKS: u8 = 2;

/// Whether the light may come on at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Good,
    /// Turned off for a flat cell, and not yet read as charged.
    Flat,
    /// A failed self-test, a cell it couldn't read, or VDDIO2 gone missing.
    /// Only a reset clears it.
    Fault,
}

/// One tick's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The level to run at, `0` meaning off.
    pub level: u8,
    /// The cell's flat, or there's a fault, so turn the UI off.
    pub off: bool,
    /// The centivolts of a voltage reading that came due this tick with the
    /// light off, and off for the two ticks before as well, so the
    /// conversion ran with no load on the cell. Implausible readings
    /// included, and a result that never came reads as zero.
    pub unloaded: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct Limits {
    schedule: Schedule,
    calibration: Calibration,
    protection: Protection,
    thermal: Thermal,
    ground: Ground,
    /// The last four-a-second voltage reading, raw.
    last_voltage: u16,
    /// Whether that reading's conversion ran with no load on the cell.
    last_unloaded: bool,
    /// The last pair of supply readings, for the programmer.
    supplies: Supplies,
    /// Whether the UI was on last tick.
    on: bool,
    /// The level the light's at, as of the last tick.
    level: u8,
    /// Whether the light was lit the tick before that. A conversion starts
    /// before that tick's output is set, so a reading that comes due now
    /// ran across the two.
    lit_before: bool,
    /// How many ticks in a row the light's been lit, as of the last tick,
    /// for [`LANDING_TICKS`]. It stops counting at 255.
    lit_for: u8,
    convert: Option<Convert>,
    /// A self-test failed, the cell couldn't be read, or VDDIO2 went missing.
    fault: bool,
}

impl Limits {
    /// `calibration` is the chip's own, from the signature row. `faults` is
    /// what the power-on self-tests found, and any fault keeps the light off
    /// for as long as this lives.
    pub const fn new(calibration: Calibration, faults: Faults) -> Self {
        Self {
            schedule: Schedule::new(),
            calibration,
            protection: Protection::new(),
            thermal: Thermal::new(),
            ground: Ground::new(),
            last_voltage: 0,
            last_unloaded: false,
            supplies: Supplies { cell: 0, vddio2: 0 },
            on: false,
            level: 0,
            lit_before: false,
            lit_for: 0,
            convert: None,
            fault: faults.any(),
        }
    }

    /// The conversion the ADC should start this tick, if any.
    pub const fn convert(&self) -> Option<Convert> {
        self.convert
    }

    /// The level the light's at, `0` meaning off, which is the last tick's
    /// [`Step::level`].
    pub const fn level(&self) -> u8 {
        self.level
    }

    /// The last raw readings of the cell and VDDIO2, taken with the light
    /// off. A measurement for `cargo xtask peek`, which nothing here acts on.
    pub const fn supplies(&self) -> Supplies {
        self.supplies
    }

    /// The battery and thermal ceilings right now.
    pub const fn ceilings(&self) -> (u8, u8) {
        (self.protection.ceiling(), self.thermal.ceiling())
    }

    /// Whether the light may come on, and if not, why not. A fault outranks
    /// a flat cell, since a flat cell can be charged and a fault can't.
    pub const fn health(&self) -> Health {
        if self.fault {
            Health::Fault
        } else if self.protection.flat() {
            Health::Flat
        } else {
            Health::Good
        }
    }

    /// A tick in place of [`Limits::tick`] while the torch rests. The light's
    /// off, and nothing's measured, since the ADC can't run in power-down. A
    /// conversion started now would come back on waking with an invalid
    /// result (DS40002413B §33.3.8). So a battery ceiling only climbs once
    /// readings start again, and a long rest forgets the last ones
    /// (`battery::FORGET_TICKS`). The torch's checks of the cell at rest
    /// (`torch::CHECK_TICKS`) are ordinary ticks with the UI off, five of
    /// them, between runs of these.
    ///
    /// Nothing looks at VDDIO2 here either. At rest PORT has PC1 low, DAC0 is
    /// at 0 and the paths are low, so PC1 letting go changes nothing the
    /// firmware controls, and a supply that dips and comes back at rest
    /// isn't a fault. The light can't come on without ticks of
    /// [`Limits::tick`] first, and those look.
    pub const fn rest(&mut self) {
        self.rest_for(1);
    }

    /// `ticks` of [`Limits::rest`] at once, for a slow tick at rest
    /// (`watchdog::SLOW_TICKS`). None is no rest at all.
    pub const fn rest_for(&mut self, ticks: u16) {
        if ticks == 0 {
            return;
        }
        self.schedule.rest();
        self.protection.rest_for(ticks);
        self.ground.rest();
        self.convert = None;
        self.on = false;
        self.level = 0;
        self.lit_for = 0;
    }

    /// One tick. `ui` is the level the UI wants (`0` for off), `result` the
    /// ADC's result, if it has one ready, and `vddio2` whether MVIO says
    /// VDDIO2 is there on this pass (`MVIO.STATUS.VDDIO2S`, DS40002413B
    /// section 19.5.3, which `hw::vddio2_ok` reads).
    ///
    /// VDDIO2 missing is a fault from this tick, and the level is 0 on this
    /// tick, whatever it was, with no fade. PC1, the driver's enable, is on
    /// VDDIO2 and tri-stated without it (section 19.3.2.1), so nothing the
    /// firmware writes reaches the enable, and DAC0 and the paths, which are
    /// on VDD, are all that's left to turn down. If VDDIO2 came back, PC1
    /// would take up whatever PORT held (the same section), so the light
    /// being off in PORT by then is what stops PC1 rising with DAC0 live.
    ///
    /// One reading is enough. The bit isn't a measurement to average. It's
    /// the chip saying PC1 is tri-stated now, so the thing to guard against
    /// has already happened, and a second reading would leave DAC0 and the
    /// paths up for another tick with nothing holding the enable. A wrong
    /// one costs a torch that's dark until the cell comes out, which is what
    /// one watchdog reset costs too. Nobody has seen the bit read 0 on this
    /// torch, with or without cause.
    ///
    /// Never inlined. It's called from one place, `torch::Torch::tick`, and
    /// left to itself LLVM inlines it and what it calls into the firmware's
    /// loop on the AVR build, which costs flash and a bigger stack frame for
    /// `main` (`cargo xtask hex` has the figures). It changes nothing about
    /// what it does.
    #[inline(never)]
    pub fn tick(&mut self, ui: u8, result: Option<u16>, vddio2: bool) -> Step {
        let on = ui != 0;
        if on && !self.on {
            self.thermal.reset();
        }
        self.on = on;
        // The level the light's at this tick, before the ceilings move. It's
        // last tick's level while the UI holds steady, and the UI's on the
        // tick it turns on. On a tick the UI ramps down it's still last
        // tick's, the higher of the two, so a ceiling pulled down to it and
        // then stepped lands where the UI already is. Otherwise the ramp's
        // level and the ceiling's would both come off in the one tick.
        let ui = ui.min(CEILING);
        let level = match on {
            true => ui
                .min(self.thermal.ceiling())
                .min(self.protection.ceiling())
                .max(self.level),
            false => 0,
        };

        let (reading, convert) = self.schedule.tick(result, on);
        self.convert = convert;
        // Whether a voltage reading that's due now is one for the hard
        // floor, taken with the light on and the load already there as its
        // conversion started.
        let settled = on && self.lit_for > LANDING_TICKS;
        if !settled {
            self.protection.dark();
        }
        // Whether a reading due now ran with no load on the cell, the light
        // off on this tick and the two before, since its conversion started
        // last tick, before that tick's output was set.
        let quiet = level == 0 && self.level == 0 && !self.lit_before;
        let mut unloaded = None;
        let mut collapsed = false;
        match reading {
            Some(Reading::Voltage(raw) | Reading::Quick(raw)) => {
                let centivolts = battery::centivolts(raw);
                // Only the four-a-second reading is a sample for the filter
                // and the holds, and for the aux.
                if matches!(reading, Some(Reading::Voltage(_))) {
                    self.last_voltage = raw;
                    self.last_unloaded = quiet;
                    if quiet {
                        unloaded = Some(centivolts);
                    }
                    if self.protection.sample(centivolts, level) == Verdict::Blind {
                        self.fault = true;
                    }
                }
                collapsed = settled && self.protection.quick(centivolts);
            }
            Some(Reading::Temperature(raw)) => {
                let sixteenths = self.calibration.sixteenths(raw);
                if on {
                    self.thermal.sample(sixteenths, level);
                }
            }
            // An ADC that's stuck can't read the cell, whatever it says, so
            // it's the same fault as a cell it can't read.
            Some(Reading::Ground(raw)) => {
                if self.ground.reading(raw) {
                    self.fault = true;
                }
            }
            // For the programmer to read and nothing else. It pairs with
            // the voltage reading two ticks before, from the same round, and
            // only when both ran unloaded. VDDIO2's turn starts with the
            // light off, but a light that went out or came on partway
            // through the round would put load sag in one of the two.
            Some(Reading::Vddio2(raw)) if self.last_unloaded && quiet => {
                self.supplies = Supplies {
                    cell: self.last_voltage,
                    vddio2: raw,
                };
            }
            Some(Reading::Vddio2(_)) | None => {}
        }
        // PC1 has let go of the driver's enable, so there's no fade to wait
        // for. That's this tick only. The fault it leaves keeps the light
        // out from here on, as any fault with the light out does.
        let cut = !vddio2;
        if cut {
            self.fault = true;
        }
        // Every tick, so a good reading during the fade can't send the
        // battery ceiling back up and leave a fault lit.
        if self.fault {
            self.protection.fail(level);
        }

        let thermal = self.thermal.tick();
        let battery = self.protection.tick();
        // A fault with the light out keeps it out, and one with it lit lets
        // the battery ceiling fade it to the bottom first. Level 1 gets its
        // tick like every level above it, so it's last tick's level that
        // decides.
        // A fault's fade has the battery ceiling heading for level 1, so a
        // low cell during it can call itself flat from any level. The fault
        // decides when that light goes out instead.
        let fault = self.fault && (cut || self.level <= 1);
        let flat = !self.fault && self.protection.flat();
        // A collapse doesn't wait for a fault's fade either. With VDDIO2's
        // cut, it's one of the two things that turn the light off from any
        // level, at once. If both come together the fault outranks the flat
        // cell in `health`.
        let off = on && (fault || flat || collapsed);
        self.lit_before = self.level > 0;
        self.level = if on && !off {
            ui.min(thermal).min(battery)
        } else {
            0
        };
        self.lit_for = match self.level {
            0 => 0,
            _ => self.lit_for.saturating_add(1),
        };
        Step {
            level: self.level,
            off,
            unloaded,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::TICKS_PER_SECOND;
    use crate::ramp::LEVELS;
    use crate::sensors::{CYCLE_TICKS, UNREAD_TICKS};
    use crate::testing::{Adc, CALIBRATION, temperature_reading, voltage_reading};
    use crate::thermal::{self, FLOOR};
    use std::vec::Vec;

    /// Self-tests that found something.
    const FAILED: Faults = Faults {
        hang: true,
        ..Faults::NONE
    };

    /// Limits and a fake ADC, a tick at a time, checking the output never
    /// jumps by more than a level while the UI holds steady.
    struct Run {
        limits: Limits,
        adc: Adc,
        ui: u8,
        /// Whether MVIO says VDDIO2 is there.
        vddio2: bool,
        uis: Vec<u8>,
        levels: Vec<u8>,
        offs: usize,
    }

    impl Run {
        fn new() -> Self {
            Self::with(Faults::NONE)
        }

        /// After power-on self-tests that found `faults`.
        fn with(faults: Faults) -> Self {
            Self {
                limits: Limits::new(CALIBRATION, faults),
                adc: Adc::new(),
                ui: 0,
                vddio2: true,
                uis: Vec::new(),
                levels: Vec::new(),
                offs: 0,
            }
        }

        fn tick(&mut self) -> u8 {
            let ui = self.ui;
            let step = self.limits.tick(ui, self.adc.result(), self.vddio2);
            self.adc.start(self.limits.convert());
            if step.off {
                self.offs += 1;
                self.ui = 0;
            }
            // Only a change of UI level (or the cell going flat, or VDDIO2
            // going missing) may jump.
            let steady = self.uis.last() == Some(&ui) && ui != 0 && !step.off;
            if let (Some(&last), true) = (self.levels.last(), steady) {
                assert!(
                    last.abs_diff(step.level) <= 1,
                    "jumped {last} → {}",
                    step.level
                );
            }
            self.uis.push(ui);
            self.levels.push(step.level);
            step.level
        }

        fn ticks(&mut self, ticks: u32) -> u8 {
            for _ in 0..ticks {
                self.tick();
            }
            *self.levels.last().unwrap()
        }

        fn secs(&mut self, seconds: u32) -> u8 {
            self.ticks(seconds * u32::from(TICKS_PER_SECOND))
        }
    }

    /// The raw temperature reading for `degrees` Celsius.
    fn celsius(degrees: u8) -> u16 {
        temperature_reading(thermal::celsius(degrees))
    }

    #[test]
    fn a_good_cell_and_a_cool_head_leave_the_ui_alone() {
        let mut run = Run::new();
        assert_eq!(run.secs(1), 0, "dark from off");
        for ui in [1, 50, CEILING] {
            run.ui = ui;
            assert_eq!(run.secs(60), ui);
            assert!(run.levels.iter().rev().take(59 * 64).all(|&l| l == ui));
        }
        assert_eq!(run.offs, 0);
        assert_eq!(run.limits.health(), Health::Good);
    }

    #[test]
    fn nothing_runs_above_the_ramps_ceiling() {
        let mut run = Run::new();
        run.ui = LEVELS;
        assert_eq!(run.secs(60), CEILING);
        assert!(run.levels.iter().all(|&l| l <= CEILING));
    }

    #[test]
    fn a_hot_head_fades_down_to_the_floor() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.temperature = celsius(70);
        let level = run.secs(2);
        assert!(level < CEILING && level > FLOOR, "{level}");
        assert_eq!(run.secs(120), FLOOR);
        assert_eq!(run.limits.ceilings().1, FLOOR);
        assert_eq!(run.offs, 0);
    }

    #[test]
    fn a_hot_head_never_goes_under_the_floor_and_comes_back_up_as_it_cools() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.adc.temperature = celsius(80);
        assert_eq!(run.secs(120), FLOOR);
        assert!(run.levels.iter().all(|&l| l == 0 || l >= FLOOR));
        run.adc.temperature = celsius(25);
        let level = run.secs(5);
        assert!(level > FLOOR && level < CEILING, "{level}");
        assert_eq!(run.secs(120), CEILING);
        assert_eq!(run.limits.ceilings().1, CEILING);
        assert_eq!(run.offs, 0);
    }

    #[test]
    fn heat_leaves_a_level_under_the_floor_alone() {
        let mut run = Run::new();
        run.ui = FLOOR - 10;
        run.adc.temperature = celsius(80);
        assert_eq!(run.secs(120), FLOOR - 10);
    }

    #[test]
    fn a_low_cell_steps_down_gradually_then_turns_off() {
        let mut run = Run::new();
        run.adc.voltage = voltage_reading(295);
        run.ui = 50;
        assert_eq!(run.secs(4), 50);
        assert_eq!(run.secs(2), 35);
        assert_eq!(run.secs(5), 20);
        // From 50 to 35 took a level a tick, not one jump.
        for level in 35..50 {
            assert!(run.levels.contains(&level), "{level}");
        }
        run.secs(30);
        assert_eq!(run.offs, 1);
        assert_eq!(*run.levels.last().unwrap(), 0);
        assert_eq!(run.limits.health(), Health::Flat);
    }

    #[test]
    fn a_flat_cell_fades_down_the_ramp_before_it_turns_off() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(1);
        run.adc.voltage = voltage_reading(280);
        // The filter takes a few seconds to fall under 2.9 V, then the hold
        // a second, so it's still at the top.
        assert_eq!(run.secs(4), CEILING);
        assert_eq!(run.offs, 0);
        // Then it fades all the way down, without ever jumping, and with the
        // cell still flat it turns off once it's had a second at level 1.
        run.secs(10);
        assert_eq!(run.offs, 1);
        let lit: Vec<u8> = run
            .levels
            .iter()
            .rev()
            .skip_while(|&&l| l == 0)
            .copied()
            .collect();
        let bottom = lit.iter().take_while(|&&l| l == 1).count();
        let hold = usize::from(battery::HOLD_SAMPLES);
        let sample = usize::from(CYCLE_TICKS);
        assert!(
            (hold - 1) * sample <= bottom && bottom <= (hold + 1) * sample,
            "{bottom} ticks at level 1"
        );
        assert_eq!(lit[bottom], 2, "faded the whole way");
    }

    #[test]
    fn a_voltage_reading_on_the_tick_it_turns_on_counts() {
        // Voltage readings come due on the third tick of each round of 16.
        let sample = usize::from(CYCLE_TICKS);
        let mut run = Run::new();
        run.adc.voltage = voltage_reading(280);
        let on_at = 4 * sample + 2;
        run.ticks(on_at as u32);
        run.ui = 1;
        run.ticks(1);
        assert_eq!(run.levels[on_at], 1);
        run.secs(2);
        // A second of flat samples, the first on the turn-on tick itself.
        let off_at = on_at + (usize::from(battery::HOLD_SAMPLES) - 1) * sample;
        assert!(run.levels[on_at..off_at].iter().all(|&l| l == 1));
        assert_eq!(run.levels[off_at], 0);
        assert_eq!(run.offs, 1);
    }

    #[test]
    fn a_flat_cell_stays_off_until_it_reads_as_charged() {
        let mut run = Run::new();
        run.adc.voltage = voltage_reading(280);
        run.ui = 50;
        run.secs(6);
        assert_eq!(run.offs, 1);
        // Asked again, it won't light, not even for a tick.
        run.ui = 50;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.offs, 2);
        // A rested cell that's still low changes nothing.
        run.adc.voltage = voltage_reading(battery::CHARGED_AT - 1);
        run.secs(10);
        assert_eq!(run.limits.health(), Health::Flat);
        // A charged one, read with the light off, as a press reads it.
        run.adc.voltage = voltage_reading(380);
        run.ticks(u32::from(CYCLE_TICKS));
        assert_eq!(run.limits.health(), Health::Good);
        run.ui = 50;
        assert_eq!(run.ticks(1), 50, "as if new, with no ceiling");
        assert_eq!(run.secs(30), 50);
    }

    /// The most ticks from the one the cell falls under the floor on to the
    /// one the light's out on, counting both, for a light that's been on a
    /// while. The cell can go unread for `UNREAD_TICKS`, then it takes
    /// `COLLAPSE_READINGS` in a row, and the last of them comes due a tick
    /// after its conversion starts.
    const COLLAPSE_TICKS: usize = UNREAD_TICKS as usize + battery::COLLAPSE_READINGS as usize;

    #[test]
    fn a_collapsing_cell_puts_the_light_out_within_twelve_ticks_wherever_in_the_round_it_falls() {
        assert_eq!(COLLAPSE_TICKS, 12);
        let mut took = Vec::new();
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ui = CEILING;
            assert_eq!(run.ticks(64 + at), CEILING);
            run.adc.voltage = voltage_reading(battery::COLLAPSE_BELOW - 1);
            let fell = run.levels.len();
            run.ticks(COLLAPSE_TICKS as u32);
            let lit = run.levels[fell..].iter().take_while(|&&l| l > 0).count();
            assert!(lit < COLLAPSE_TICKS, "at {at}: still lit after {lit} ticks");
            // Out from the top in one tick, with no fade, and as a flat cell.
            assert!(run.levels[fell..fell + lit].iter().all(|&l| l == CEILING));
            assert!(run.levels[fell + lit..].iter().all(|&l| l == 0), "at {at}");
            assert_eq!(run.offs, 1, "at {at}");
            assert_eq!(run.limits.health(), Health::Flat, "at {at}");
            took.push(lit + 1);
        }
        // Two readings a tick apart at best, and the whole of the gap at
        // worst.
        assert_eq!(took.iter().min(), Some(&3));
        assert_eq!(took.iter().max(), Some(&COLLAPSE_TICKS));
    }

    #[test]
    fn one_reading_under_the_floor_does_nothing() {
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ui = 100;
            run.ticks(64 + at);
            // Low for the one conversion, if this tick starts one.
            run.adc.voltage = voltage_reading(250);
            run.ticks(1);
            run.adc.voltage = voltage_reading(380);
            assert_eq!(run.secs(10), 100, "at {at}");
            assert_eq!(run.offs, 0, "at {at}");
            assert_eq!(run.limits.ceilings().0, CEILING, "at {at}");
        }
    }

    #[test]
    fn exactly_2_80_volts_is_left_to_the_filter_and_the_holds() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = voltage_reading(battery::COLLAPSE_BELOW);
        assert_eq!(run.secs(2), CEILING, "not a collapse");
        run.secs(20);
        assert_eq!(run.offs, 1);
        // Faded the whole way down, where one count lower snaps out.
        let lit: Vec<u8> = run.levels.iter().copied().filter(|&l| l > 0).collect();
        assert_eq!(lit.last(), Some(&1));
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = voltage_reading(battery::COLLAPSE_BELOW - 1);
        run.secs(2);
        let lit: Vec<u8> = run.levels.iter().copied().filter(|&l| l > 0).collect();
        assert_eq!(lit.last(), Some(&CEILING));
    }

    #[test]
    fn a_collapsed_cell_stays_off_until_it_reads_as_charged() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = voltage_reading(270);
        run.ticks(COLLAPSE_TICKS as u32);
        assert_eq!(run.offs, 1);
        assert_eq!(run.limits.health(), Health::Flat);
        // Asked again, it won't light, not even for a tick.
        run.ui = CEILING;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.offs, 2);
        // A cell that rests under 3.60 V stays flat.
        run.adc.voltage = voltage_reading(battery::CHARGED_AT - 1);
        run.secs(10);
        assert_eq!(run.limits.health(), Health::Flat);
        // One that rests at 3.60 V, read with the light off as a press
        // reads it, lights again with no ceiling.
        run.adc.voltage = voltage_reading(battery::CHARGED_AT);
        run.ticks(u32::from(CYCLE_TICKS));
        assert_eq!(run.limits.health(), Health::Good);
        run.ui = 50;
        assert_eq!(run.ticks(1), 50);
        assert_eq!(run.limits.ceilings().0, CEILING);
    }

    /// The conversions started on a light's first two ticks run across PC1
    /// rising and the DAC coming up, and a sag there is a load landing and
    /// not a cell collapsing.
    #[test]
    fn the_load_landing_as_the_light_comes_on_is_no_collapse() {
        assert_eq!(LANDING_TICKS, 2);
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ticks(32 + at);
            run.ui = 50;
            run.adc.voltage = voltage_reading(250);
            run.ticks(u32::from(LANDING_TICKS));
            run.adc.voltage = voltage_reading(380);
            assert_eq!(run.secs(5), 50, "on at {at}");
            assert_eq!(run.offs, 0, "on at {at}");
        }
    }

    #[test]
    fn a_cell_that_stays_down_after_the_load_lands_is_a_collapse() {
        let mut took = Vec::new();
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ticks(32 + at);
            run.ui = 50;
            run.adc.voltage = voltage_reading(250);
            let on = run.levels.len();
            // Turned on after the round's tick 2, VDDIO2's turn has the
            // ADC, and the quick readings wait for the next round.
            run.ticks(u32::from(CYCLE_TICKS) + COLLAPSE_TICKS as u32);
            let lit = run.levels[on..].iter().take_while(|&&l| l > 0).count();
            assert_eq!(*run.levels.last().unwrap(), 0, "on at {at}");
            assert_eq!(run.limits.health(), Health::Flat, "on at {at}");
            took.push(lit);
        }
        // Never before the third and fourth ticks' conversions are both in,
        // and never later than the next round's third reading.
        assert_eq!(took.iter().min(), Some(&4));
        assert!(
            *took.iter().max().unwrap() <= usize::from(CYCLE_TICKS) + 3,
            "{took:?}"
        );
    }

    #[test]
    fn a_collapse_does_not_wait_for_a_faults_fade() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = 0;
        while run.limits.health() == Health::Good {
            run.tick();
        }
        run.adc.voltage = voltage_reading(270);
        run.ticks(COLLAPSE_TICKS as u32);
        assert_eq!(run.offs, 1);
        assert_eq!(*run.levels.last().unwrap(), 0);
        let lit: Vec<u8> = run.levels.iter().copied().filter(|&l| l > 0).collect();
        let last = *lit.last().unwrap();
        assert!(
            last > CEILING - 20,
            "out from {last}, well short of the bottom"
        );
        // Still a fault, which outranks a flat cell.
        assert_eq!(run.limits.health(), Health::Fault);
        run.adc.voltage = voltage_reading(400);
        run.secs(10);
        assert_eq!(run.limits.health(), Health::Fault);
    }

    /// A collapse and VDDIO2 going both put the light out on the tick
    /// they're seen. Together, it's out that tick, and VDDIO2's fault
    /// outranks the collapse's flat cell, so a charged cell doesn't bring it
    /// back. A collapse after VDDIO2's gone finds the light already out.
    #[test]
    fn vddio2_missing_outranks_a_collapse_and_both_are_out_at_once() {
        for gap in 0..3 {
            let mut run = Run::new();
            run.ui = CEILING;
            run.secs(2);
            run.adc.voltage = voltage_reading(270);
            // Up to the tick before the collapse would come due.
            while run.limits.health() == Health::Good {
                let mut ahead = run.limits.clone();
                let step = ahead.tick(run.ui, run.adc.clone().result(), true);
                if step.off {
                    break;
                }
                run.tick();
            }
            for _ in 0..gap {
                run.tick();
            }
            run.vddio2 = false;
            assert_eq!(run.ticks(1), 0, "gap {gap}");
            assert_eq!(run.limits.health(), Health::Fault, "gap {gap}");
            run.vddio2 = true;
            run.adc.voltage = voltage_reading(400);
            run.secs(2);
            run.ui = 50;
            assert_eq!(run.ticks(1), 0, "gap {gap}: charged, still a fault");
            assert_eq!(run.limits.health(), Health::Fault, "gap {gap}");
        }
    }

    #[test]
    fn the_floor_does_nothing_with_the_light_off() {
        let mut run = Run::new();
        run.adc.voltage = voltage_reading(250);
        // Off but still read, as it is while the switch is held down.
        assert_eq!(run.secs(60), 0);
        assert_eq!(run.limits.health(), Health::Good);
        assert_eq!(run.offs, 0);
    }

    /// The quick readings feed the hard floor and nothing else. A reading
    /// that's low for one conversion moves the filter only if that
    /// conversion is the four-a-second one, started on the round's second
    /// tick.
    #[test]
    fn only_the_four_a_second_reading_reaches_the_filter() {
        let mut moved = Vec::new();
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ui = 100;
            run.ticks(64 + at);
            assert_eq!(run.limits.protection.centivolts(), Some(380));
            run.adc.voltage = voltage_reading(300);
            run.ticks(1);
            run.adc.voltage = voltage_reading(380);
            run.ticks(1);
            if run.limits.protection.centivolts() != Some(380) {
                moved.push(at);
            }
        }
        assert_eq!(moved, [1]);
    }

    #[test]
    fn a_low_cell_during_a_faults_fade_still_fades_a_level_a_tick() {
        // Under 3.00 V long enough for a step down, but a second of garbage
        // makes it a fault first, and the step comes due during the fade.
        // The battery ceiling is heading for level 1 then, so the step used to
        // call the cell flat and cut the light out from wherever the fade had
        // got to.
        let mut run = Run::new();
        run.ui = CEILING;
        run.adc.voltage = voltage_reading(295);
        run.secs(4);
        run.adc.voltage = 0;
        run.secs(1);
        assert_eq!(run.limits.health(), Health::Fault);
        run.adc.voltage = voltage_reading(295);
        run.secs(5);
        assert_eq!(run.offs, 1);
        let lit: Vec<u8> = run.levels.iter().copied().filter(|&l| l > 0).collect();
        for level in 1..CEILING {
            assert!(lit.contains(&level), "faded through {level}");
        }
        assert_eq!(lit.last(), Some(&1));
    }

    #[test]
    fn garbage_voltages_fade_it_out_and_latch_a_fault() {
        for garbage in [0, 4 * 4095, 1] {
            let mut run = Run::new();
            run.ui = CEILING;
            run.secs(2);
            run.adc.voltage = garbage;
            // Three garbage samples go, the fourth is a fault, and the
            // light fades from the top a level a tick.
            run.ticks(3 * u32::from(CYCLE_TICKS));
            assert_eq!(run.ticks(1), CEILING, "{garbage}");
            assert_eq!(run.limits.health(), Health::Good, "{garbage}");
            run.secs(3);
            assert_eq!(run.offs, 1, "{garbage}");
            assert_eq!(run.limits.health(), Health::Fault, "{garbage}");
            let lit: Vec<u8> = run.levels.iter().copied().filter(|&l| l > 0).collect();
            // Every level gets its tick, level 1 included, and that's the
            // last lit.
            for level in 1..CEILING {
                assert!(lit.contains(&level), "{garbage}: faded through {level}");
            }
            assert_eq!(lit.last(), Some(&1), "{garbage}");
            // It stays a fault with good readings, on or off.
            run.adc.voltage = voltage_reading(380);
            run.secs(10);
            run.ui = CEILING;
            assert_eq!(run.ticks(1), 0, "{garbage}");
            assert_eq!(run.offs, 2, "{garbage}");
            assert_eq!(run.limits.health(), Health::Fault, "{garbage}");
        }
    }

    /// The UI's ramp takes a level a tick and so does a fading ceiling. On a
    /// tick with both, the light still moves one level and not two.
    #[test]
    fn ramping_down_through_a_fade_still_moves_a_level_a_tick() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = 0;
        while run.limits.health() == Health::Good {
            run.tick();
        }
        let mut last = run.limits.level();
        assert_eq!(last, CEILING - 1, "the fade's first step");
        // The ramp starts on the tick after the fault's decided, and stops
        // half-way, so the ceiling's seen leading, level and trailing.
        while last > 0 {
            if run.ui > CEILING / 2 {
                run.ui -= 1;
            }
            let level = run.tick();
            assert_eq!(level, last - 1, "from {last} with the UI at {}", run.ui);
            last = level;
        }
        assert_eq!(run.offs, 1);
    }

    #[test]
    fn good_readings_during_the_fade_still_see_it_out() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = 0;
        while run.limits.health() == Health::Good {
            run.tick();
        }
        // Back to a full cell straight away, for long enough to lift a
        // ceiling several times over.
        run.adc.voltage = voltage_reading(420);
        run.secs(30);
        assert_eq!(run.offs, 1);
        let out = run.levels.iter().rposition(|&l| l > 0).unwrap();
        assert!(run.levels[out..].len() > 25 * 64, "out within 5 s");
        assert!(run.limits.ceilings().0 <= 1);
    }

    #[test]
    fn garbage_voltages_with_the_light_off_are_a_fault_too() {
        let mut run = Run::new();
        run.adc.voltage = 0;
        // Off but still read, as it is while the switch is held down.
        assert_eq!(run.secs(2), 0);
        assert_eq!(run.limits.health(), Health::Fault);
        run.ui = CEILING;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.offs, 1);
    }

    #[test]
    fn a_glitch_or_three_is_no_fault() {
        let mut run = Run::new();
        run.ui = CEILING;
        for _ in 0..60 {
            run.adc.voltage = 0;
            run.ticks(3 * u32::from(CYCLE_TICKS));
            run.adc.voltage = voltage_reading(380);
            run.ticks(u32::from(CYCLE_TICKS));
        }
        assert_eq!(run.limits.health(), Health::Good);
        assert_eq!(*run.levels.last().unwrap(), CEILING);
        assert_eq!(run.offs, 0);
    }

    #[test]
    fn a_dead_adc_is_a_fault() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(1);
        run.adc.voltage = 0;
        run.adc.temperature = 0;
        assert_eq!(run.secs(30), 0);
        assert_eq!(run.limits.health(), Health::Fault);
        assert_eq!(run.offs, 1);
    }

    /// An ADC stuck on a good-looking 3.8 V for every channel reads ground as
    /// 3.8 V too, which no working ADC does, so it's a fault and the light
    /// fades out.
    #[test]
    fn an_adc_stuck_on_a_plausible_voltage_is_a_fault() {
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ui = 100;
            run.ticks(64 + at);
            let raw = run.adc.voltage;
            run.adc.stuck(raw);
            // Two ground readings a round apart. If it sticks just after a
            // good one started, the second is due 33 ticks on.
            run.ticks(2 * u32::from(CYCLE_TICKS));
            if run.limits.health() == Health::Good {
                run.ticks(1);
            }
            assert_eq!(run.limits.health(), Health::Fault, "at {at}");
            assert_eq!(run.secs(3), 0, "at {at}");
            assert_eq!(run.offs, 1, "at {at}");
            let lit: Vec<u8> = run.levels.iter().copied().filter(|&l| l > 0).collect();
            assert_eq!(lit.last(), Some(&1), "faded out, at {at}");
        }
    }

    #[test]
    fn one_bad_ground_reading_is_no_fault() {
        let mut run = Run::new();
        run.ui = 100;
        for _ in 0..60 {
            run.adc.ground = 6140;
            run.ticks(u32::from(CYCLE_TICKS));
            run.adc.ground = crate::testing::GROUND_READING;
            run.ticks(u32::from(CYCLE_TICKS));
        }
        assert_eq!(run.limits.health(), Health::Good);
        assert_eq!(*run.levels.last().unwrap(), 100);
    }

    #[test]
    fn a_stuck_adc_with_the_light_off_and_the_switch_down_is_a_fault_too() {
        let mut run = Run::new();
        run.adc.stuck(voltage_reading(380));
        // Off but still read, as it is while the switch is held down.
        run.secs(1);
        assert_eq!(run.limits.health(), Health::Fault);
        run.ui = CEILING;
        assert_eq!(run.ticks(1), 0);
    }

    /// A known gap, docs/safety.md H5 and H15. A reading that's steady at a
    /// good-looking 3.8 V, from an ADC that still reads ground as ground, is
    /// the same to the firmware as a good cell, so nothing here ever steps
    /// the light down. That's a cell really sitting at 3.8 V, or an ADC
    /// that's wrong in a way ground can't show, such as a reference that's
    /// drifted. This pins it down so that if the gap is ever closed, this is
    /// the test that has to change.
    #[test]
    fn a_plausible_voltage_that_never_changes_keeps_the_light_on() {
        let mut run = Run::new();
        assert_eq!(battery::centivolts(run.adc.voltage), 380);
        run.ui = 100;
        run.secs(1);
        for minute in 0..60 {
            assert_eq!(run.secs(60), 100, "minute {minute}");
            assert_eq!(run.limits.ceilings().0, CEILING, "minute {minute}");
        }
        assert!(run.levels.iter().skip(1).all(|&l| l == 100));
        assert_eq!(run.offs, 0);
    }

    /// The same gap for the temperature, docs/safety.md H7 and H15. A sensor
    /// stuck at a comfortable 25 °C never asks for a step down, however long
    /// the light has really been cooking.
    #[test]
    fn a_plausible_temperature_that_never_changes_never_steps_down() {
        let mut run = Run::new();
        assert_eq!(run.adc.temperature, celsius(25));
        run.ui = CEILING;
        run.secs(1);
        for minute in 0..60 {
            assert_eq!(run.secs(60), CEILING, "minute {minute}");
            assert_eq!(run.limits.ceilings().1, CEILING, "minute {minute}");
        }
        assert!(run.levels.iter().skip(1).all(|&l| l == CEILING));
        assert_eq!(run.offs, 0);
    }

    #[test]
    fn a_garbage_temperature_alone_never_turns_it_off() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.adc.temperature = 0;
        assert_eq!(run.secs(600), FLOOR);
        assert_eq!(run.offs, 0);
        assert_eq!(run.limits.health(), Health::Good);
    }

    #[test]
    fn no_result_at_all_is_garbage_too() {
        let mut limits = Limits::new(CALIBRATION, Faults::NONE);
        let off = (0..5 * TICKS_PER_SECOND).position(|_| limits.tick(CEILING, None, true).off);
        assert!(off.is_some());
        assert_eq!(limits.health(), Health::Fault);
        assert_eq!(limits.ceilings().1, FLOOR);
    }

    #[test]
    fn the_temperature_is_only_read_while_on() {
        let mut run = Run::new();
        let reads = |run: &Run| {
            run.limits.convert().map(|c| c.channel) == Some(crate::sensors::Channel::Temperature)
        };
        for _ in 0..60 * TICKS_PER_SECOND {
            run.tick();
            assert!(!reads(&run), "off");
        }
        run.ui = 50;
        let mut temperatures = 0;
        for _ in 0..TICKS_PER_SECOND {
            run.tick();
            temperatures += u32::from(reads(&run));
        }
        assert!(temperatures > 0, "on");
    }

    #[test]
    fn turning_on_resets_thermal_regulation() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.adc.temperature = celsius(70);
        run.secs(60);
        assert_eq!(run.limits.ceilings().1, FLOOR);
        run.ui = 0;
        run.adc.temperature = celsius(25);
        run.secs(1);
        run.ui = CEILING;
        // Straight back to the top, since last time's heat doesn't carry over.
        assert_eq!(run.ticks(1), CEILING);
    }

    #[test]
    fn a_rest_starts_no_conversions_and_stays_dark() {
        let mut run = Run::new();
        // A tick of the round that starts a conversion.
        run.ticks(1);
        assert!(run.limits.convert().is_some());
        run.limits.rest();
        assert_eq!(run.limits.convert(), None);
        assert_eq!(run.limits.level(), 0);
    }

    #[test]
    fn heat_doesnt_carry_over_a_rest() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.adc.temperature = celsius(70);
        run.secs(120);
        assert_eq!(run.limits.ceilings().1, FLOOR);
        run.ui = 0;
        run.ticks(1);
        run.limits.rest();
        run.adc.temperature = celsius(25);
        run.ui = CEILING;
        assert_eq!(run.ticks(1), CEILING);
    }

    #[test]
    fn a_rest_keeps_the_battery_ceiling_until_the_cell_proves_itself() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.adc.voltage = voltage_reading(295);
        run.secs(6);
        let ceiling = run.limits.ceilings().0;
        assert!(ceiling < CEILING);
        run.ui = 0;
        run.ticks(1);
        run.limits.rest();
        run.adc.voltage = voltage_reading(380);
        assert_eq!(run.limits.ceilings().0, ceiling);
        run.ui = CEILING;
        assert_eq!(run.ticks(1), ceiling);
        run.secs(30);
        assert_eq!(run.limits.ceilings().0, CEILING);
    }

    #[test]
    fn a_rest_keeps_a_fault_and_a_flat_cell() {
        let mut run = Run::with(FAILED);
        run.limits.rest();
        assert_eq!(run.limits.health(), Health::Fault);
        let mut run = Run::new();
        run.adc.voltage = voltage_reading(280);
        run.ui = 50;
        run.secs(6);
        for _ in 0..battery::FORGET_TICKS {
            run.limits.rest();
        }
        assert_eq!(run.limits.health(), Health::Flat);
    }

    /// Voltage readings come due on the third tick of each round of 16,
    /// counting from 0. A conversion starts before its tick's output is set,
    /// so the reading's only the unloaded cell once the light's been out for
    /// the two ticks before it as well as its own.
    #[test]
    fn an_unloaded_reading_is_one_with_the_light_out_for_two_ticks_before_it() {
        let due = |tick: usize| tick % usize::from(CYCLE_TICKS) == 2;
        // Off from power-on, and every reading counts, garbage included.
        for voltage in [voltage_reading(320), 0, 4 * 4095] {
            let mut limits = Limits::new(CALIBRATION, Faults::NONE);
            let mut adc = Adc::new();
            adc.voltage = voltage;
            for tick in 0..64 {
                let step = limits.tick(0, adc.result(), true);
                adc.start(limits.convert());
                let read = due(tick).then(|| battery::centivolts(voltage));
                assert_eq!(step.unloaded, read, "{voltage}: tick {tick}");
            }
        }
        // Lit, then out at every point in the round, with the switch still
        // down so the readings go on.
        for out_at in 64..80 {
            let mut limits = Limits::new(CALIBRATION, Faults::NONE);
            let mut adc = Adc::new();
            for tick in 0..out_at + 40 {
                let ui = if tick < out_at { 50 } else { 0 };
                let step = limits.tick(ui, adc.result(), true);
                adc.start(limits.convert());
                let read = (due(tick) && tick >= out_at + 2).then_some(380);
                assert_eq!(step.unloaded, read, "out at {out_at}: tick {tick}");
            }
        }
    }

    /// The pair kept for `cargo xtask peek` is only ever two unloaded
    /// readings, with the light going out, or coming on, at every point in
    /// the round. A conversion reads the loaded cell if the light was on on
    /// its tick or the one before, here 3.00 V against 3.80 V at rest, and
    /// VDDIO2 reads as the cell. Out on tick 2, the voltage reading that
    /// comes due then ran lit while VDDIO2's turn starts dark, which is the
    /// pair that mustn't be kept.
    #[test]
    fn the_supplies_kept_are_never_a_loaded_reading() {
        let (rested, loaded) = (voltage_reading(380), voltage_reading(300));
        let pair = Supplies {
            cell: rested,
            vddio2: rested,
        };
        for (lit_from, lit_to) in (64..80)
            .map(|out| (0, out))
            .chain((64..80).map(|on| (on, 200)))
        {
            let mut limits = Limits::new(CALIBRATION, Faults::NONE);
            let mut adc = Adc::new();
            let mut was_lit = false;
            let mut kept_after = None;
            for tick in 0..200 {
                let ui = if (lit_from..lit_to).contains(&tick) {
                    50
                } else {
                    0
                };
                limits.tick(ui, adc.result(), true);
                let lit = limits.level() > 0;
                adc.voltage = if lit || was_lit { loaded } else { rested };
                adc.start(limits.convert());
                was_lit = lit;
                let kept = limits.supplies();
                let case = (lit_from, lit_to, tick);
                assert!(
                    kept == pair || kept == Supplies::default(),
                    "{case:?}: {kept:?}"
                );
                if kept == pair && tick >= lit_to {
                    kept_after.get_or_insert(tick);
                }
            }
            // Out for good, the next round's pair is kept.
            if lit_to < 200 {
                let kept = kept_after.expect("never kept once the light was out");
                assert!(
                    kept <= lit_to + 2 * usize::from(CYCLE_TICKS),
                    "{lit_to}: {kept}"
                );
            }
        }
    }

    /// A check of the cell at rest, five minutes after the last, the way
    /// `torch::Torch` runs one. The rest before it is `battery::FORGET_TICKS`
    /// long, so each check finds the filter forgotten and seeds it afresh,
    /// as a press after a long rest always did. A press within five minutes
    /// of a check blends in with that check's reading instead. A check takes
    /// one reading with the light off, which can't raise a battery ceiling,
    /// since that takes four seconds of readings in a row.
    #[test]
    fn a_check_at_rest_seeds_the_filter_afresh_and_leaves_the_ceiling() {
        use crate::torch::CHECK_TICKS;
        assert_eq!(CHECK_TICKS, battery::FORGET_TICKS);
        let mut run = Run::new();
        run.ui = CEILING;
        run.adc.voltage = voltage_reading(295);
        run.secs(6);
        let ceiling = run.limits.ceilings().0;
        assert!(ceiling < CEILING);
        run.ui = 0;
        run.ticks(1);
        run.limits.rest();
        for cv in [380, 330, 400] {
            // Two ticks of the check, with the rest that follows the
            // reading's own tick counted in with the next.
            for _ in 1..CHECK_TICKS {
                run.limits.rest();
            }
            assert_eq!(run.limits.protection.centivolts(), None, "{cv}");
            run.adc.voltage = voltage_reading(cv);
            run.ticks(3);
            run.limits.rest();
            assert_eq!(run.limits.protection.centivolts(), Some(cv));
            assert_eq!(run.limits.ceilings().0, ceiling, "{cv}");
            assert_eq!(run.limits.health(), Health::Good, "{cv}");
        }
    }

    #[test]
    fn garbage_from_power_on_shows_nothing_from_off() {
        let (voltage, temperature) = (Adc::new().voltage, Adc::new().temperature);
        for (voltage, temperature) in [(0, temperature), (voltage, 0), (0, 0), (4 * 4095, 4 * 4095)]
        {
            let mut run = Run::new();
            run.adc.voltage = voltage;
            run.adc.temperature = temperature;
            run.secs(10);
            assert!(
                run.levels.iter().all(|&l| l == 0),
                "{voltage}, {temperature}"
            );
            assert_eq!(run.offs, 0);
        }
    }

    #[test]
    fn a_failed_self_test_never_lights() {
        let mut run = Run::with(FAILED);
        assert_eq!(run.limits.health(), Health::Fault);
        assert_eq!(run.secs(10), 0);
        for ui in [1, 30, CEILING, LEVELS] {
            run.ui = ui;
            assert_eq!(run.secs(10), 0, "{ui}");
        }
        assert!(run.levels.iter().all(|&l| l == 0));
        assert_eq!(run.offs, 4, "asked for off every time");
    }

    /// VDDIO2 missing is the one fault that doesn't fade. From any level, the
    /// tick that reads it missing is the first with the light out, and it
    /// coming back changes nothing.
    #[test]
    fn vddio2_missing_puts_the_light_out_on_that_tick_from_any_level() {
        for level in 1..=CEILING {
            let mut run = Run::new();
            run.ui = level;
            assert_eq!(run.secs(1), level);
            run.vddio2 = false;
            assert_eq!(run.ticks(1), 0, "from {level}");
            assert_eq!(run.offs, 1, "from {level}");
            assert_eq!(run.limits.health(), Health::Fault, "from {level}");
            run.vddio2 = true;
            assert_eq!(run.secs(1), 0, "from {level}, with VDDIO2 back");
            run.ui = level;
            assert_eq!(run.secs(1), 0, "from {level}, asked again");
            assert_eq!(run.limits.health(), Health::Fault, "from {level}");
        }
    }

    #[test]
    fn vddio2_missing_with_the_light_off_is_a_fault_before_anything_lights() {
        let mut run = Run::new();
        run.secs(1);
        run.vddio2 = false;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.limits.health(), Health::Fault);
        assert_eq!(run.offs, 0, "nothing was on to turn off");
        run.vddio2 = true;
        run.ui = CEILING;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.offs, 1);
    }

    /// The UI asking for light on the very tick VDDIO2 reads missing gets
    /// none, not even for that tick.
    #[test]
    fn turning_on_with_vddio2_missing_never_lights() {
        let mut run = Run::new();
        run.secs(1);
        run.vddio2 = false;
        run.ui = CEILING;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.offs, 1);
        assert_eq!(run.limits.health(), Health::Fault);
        assert!(run.levels.iter().all(|&l| l == 0));
    }

    /// A cell that can't be read fades the light out. VDDIO2 going missing
    /// part-way down doesn't wait for the fade.
    #[test]
    fn vddio2_missing_cuts_another_faults_fade_short() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = 0;
        while run.limits.health() == Health::Good {
            run.tick();
        }
        let fading = run.ticks(10);
        assert!(fading > 1 && fading < CEILING, "{fading}");
        run.vddio2 = false;
        assert_eq!(run.ticks(1), 0, "from {fading}");
        assert_eq!(run.offs, 1);
    }

    /// A flat cell can be charged. VDDIO2 going missing can't be undone, so
    /// a charged cell doesn't bring the light back after it.
    #[test]
    fn vddio2_missing_outranks_a_flat_cell() {
        let mut run = Run::new();
        run.adc.voltage = voltage_reading(280);
        run.ui = 50;
        run.secs(6);
        assert_eq!(run.limits.health(), Health::Flat);
        run.vddio2 = false;
        run.ticks(1);
        assert_eq!(run.limits.health(), Health::Fault);
        run.vddio2 = true;
        run.adc.voltage = voltage_reading(380);
        run.secs(2);
        assert_eq!(run.limits.health(), Health::Fault);
        run.ui = 50;
        assert_eq!(run.ticks(1), 0);
    }

    /// A million ticks of random UI levels and ADC results, checking every
    /// step against `cap` and the UI. With `gone_at`, VDDIO2 reads missing
    /// on that tick alone, and nothing lights from then on.
    fn throw_noise_at(mut limits: Limits, mut cap: u8, seed: u32, gone_at: Option<u32>) {
        let mut rng = crate::testing::Rng::new(seed);
        let mut ticks = 0_u32;
        let mut tick = 0_u32;
        while ticks < 1_000_000 {
            // The UI holds a level for a run of ticks, so the ceilings have
            // time to move. Some runs are off, and some ask for more than
            // the ramp has.
            let ui = match rng.next_u16() % 8 {
                0 => 0,
                1 => 255,
                _ => rng.next_u16() as u8,
            };
            let run = 1 + u32::from(rng.next_u16() % 4000);
            // What the ADC says for this run. Anything at all, a believable
            // reading, or one believable reading over and over.
            let mode = rng.next_u16() % 3;
            let held = rng.next_u16() % (4 * 4095 + 1);
            for _ in 0..run {
                let result = match (rng.next_u16() % 4, mode) {
                    (0, _) => None,
                    (_, 0) => Some(rng.next_u16()),
                    (_, 1) => Some(rng.next_u16() % (4 * 4095 + 1)),
                    _ => Some(held),
                };
                let gone = gone_at == Some(tick);
                if gone {
                    cap = 0;
                }
                tick += 1;
                let step = limits.tick(ui, result, !gone);
                assert!(
                    step.level <= ui.min(cap),
                    "ui {ui}, result {result:?}: level {}",
                    step.level
                );
                assert!(step.level <= CEILING, "level {}", step.level);
                assert!(!step.off || step.level == 0, "off at level {}", step.level);
                assert_eq!(limits.level(), step.level);
                if cap == 0 {
                    assert_eq!(step.off, ui != 0, "ui {ui} with nothing allowed");
                    assert_eq!(limits.health(), Health::Fault);
                }
            }
            ticks += run;
        }
    }

    #[test]
    fn random_readings_never_lift_the_level_over_the_ui_or_the_cap() {
        let limits = Limits::new(CALIBRATION, Faults::NONE);
        throw_noise_at(limits, CEILING, 0x5EED_0001, None);
    }

    #[test]
    fn random_readings_never_light_a_failed_self_test() {
        throw_noise_at(Limits::new(CALIBRATION, FAILED), 0, 0x5EED_0002, None);
    }

    /// The same noise, with VDDIO2 reading missing for one tick part of the
    /// way in, at a few different points. Whatever the readings and the UI
    /// do afterwards, nothing lights again.
    #[test]
    fn random_readings_never_light_it_once_vddio2_has_read_missing() {
        for gone_at in [0, 1, 2, 5_000, 123_456] {
            let limits = Limits::new(CALIBRATION, Faults::NONE);
            throw_noise_at(limits, CEILING, 0x5EED_0007, Some(gone_at));
        }
    }
}
