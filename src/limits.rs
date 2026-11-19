//! What the cell and the temperature allow, with the UI's level in and the
//! level to run at out.

use crate::battery::{self, Protection, Verdict};
use crate::gradual::{Ceiling, Fade};
use crate::ramp::CEILING;
use crate::selftest::Faults;
use crate::sensors::{Convert, Ground, Reading, Schedule};
use crate::temp::Calibration;
use crate::thermal::Thermal;

/// The first ticks of a light start conversions that run across the load
/// landing, so `battery::Protection::quick` never sees them. Unmeasured.
pub const LANDING_TICKS: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Good,
    Flat,
    /// Only a reset clears it.
    Fault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub level: u8,
    /// The UI should turn off.
    pub off: bool,
    /// Centivolts of a voltage reading that came due with the light off on
    /// this tick and the two before.
    pub unloaded: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct Limits {
    schedule: Schedule,
    calibration: Calibration,
    protection: Protection,
    thermal: Thermal,
    ground: Ground,
    /// What the UI asked for last tick.
    asked: u8,
    /// As of the last tick.
    level: u8,
    /// Lit on the tick before the last. A conversion starts before its
    /// tick's output is set, so a reading due now ran across the two.
    lit_before: bool,
    /// Ticks in a row the light's been lit, as of the last tick, up to 255.
    lit_for: u8,
    convert: Option<Convert>,
    fault: bool,
    failing: Ceiling,
    fade: Fade,
}

impl Limits {
    pub const fn new(calibration: Calibration, faults: Faults) -> Self {
        Self {
            schedule: Schedule::new(),
            calibration,
            protection: Protection::new(),
            thermal: Thermal::new(),
            ground: Ground::new(),
            asked: 0,
            level: 0,
            lit_before: false,
            lit_for: 0,
            convert: None,
            fault: faults.any(),
            failing: Ceiling::new(battery::DOWN_EVERY, battery::DOWN_EVERY),
            fade: Fade::new(),
        }
    }

    pub const fn convert(&self) -> Option<Convert> {
        self.convert
    }

    pub const fn level(&self) -> u8 {
        self.level
    }

    pub const fn starting(&self) -> bool {
        self.lit_for == 1
    }

    pub const fn position(&self) -> u16 {
        self.fade.position()
    }

    /// The lower of the battery's ceiling and a fault's, then the thermal one.
    #[cfg(test)]
    pub fn ceilings(&self) -> (u8, u8) {
        (
            self.protection.ceiling().min(self.failing.level()),
            self.thermal.ceiling(),
        )
    }

    pub const fn health(&self) -> Health {
        if self.fault {
            Health::Fault
        } else if self.protection.flat() {
            Health::Flat
        } else {
            Health::Good
        }
    }

    /// A tick at rest in place of [`Limits::tick`]. Nothing's measured, since
    /// a conversion in power-down comes back invalid (DS40002413B §33.3.8).
    pub const fn rest(&mut self) {
        self.rest_for(1);
    }

    pub const fn rest_for(&mut self, ticks: u16) {
        if ticks == 0 {
            return;
        }
        self.schedule.rest();
        self.protection.rest_for(ticks);
        self.ground.rest();
        self.convert = None;
        self.asked = 0;
        self.level = 0;
        self.lit_for = 0;
        self.fade = Fade::new();
    }

    /// `vddio2` is `MVIO.STATUS.VDDIO2S` (DS40002413B section 19.5.3). PC1 is
    /// tri-stated without VDDIO2 (section 19.3.2.1), so one reading of it
    /// missing is off with no fade.
    ///
    /// Never inlined, since inlined into the firmware's loop it costs flash
    /// and a bigger stack frame for `main`.
    #[inline(never)]
    pub fn tick(&mut self, ui: u8, result: Option<u16>, vddio2: bool) -> Step {
        let on = ui != 0;
        if on && self.asked == 0 {
            self.thermal.reset();
        }
        let steady = ui == self.asked;
        self.asked = ui;
        let ui = ui.min(CEILING);
        // The level the light's at this tick, before the ceilings move.
        let level = match on {
            true => ui
                .min(self.thermal.ceiling())
                .min(self.protection.ceiling())
                .min(self.failing.level())
                .max(self.level),
            false => 0,
        };

        let (reading, convert) = self.schedule.tick(result, on);
        self.convert = convert;
        let settled = on && self.lit_for > LANDING_TICKS;
        if !settled {
            self.protection.dark();
        }
        let quiet = level == 0 && self.level == 0 && !self.lit_before;
        let mut unloaded = None;
        let mut collapsed = false;
        match reading {
            Some(Reading::Voltage(raw) | Reading::Quick(raw)) => {
                let centivolts = battery::centivolts(raw);
                if matches!(reading, Some(Reading::Voltage(_))) {
                    if quiet && battery::plausible(centivolts) {
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
            Some(Reading::Ground(raw)) => {
                if self.ground.reading(raw) {
                    self.fault = true;
                }
            }
            None => {}
        }
        let cut = !vddio2;
        if cut {
            self.fault = true;
        }
        if self.fault {
            self.failing.set(1, level);
        }

        let thermal = self.thermal.tick();
        let battery = self.protection.tick().min(self.failing.tick());
        let fault = self.fault && (cut || self.level <= 1);
        let flat = self.protection.flat();
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
        let gentle = steady && thermal <= battery;
        self.fade.tick(self.level, gentle);
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
    use crate::sensors::{CYCLE_TICKS, FULL_SCALE, UNREAD_TICKS};
    use crate::testing::{Adc, CALIBRATION, CELL_READING, HUNG, celsius_reading, voltage_reading};
    use crate::thermal::FLOOR;
    use std::vec::Vec;

    struct Run {
        limits: Limits,
        adc: Adc,
        ui: u8,
        vddio2: bool,
        uis: Vec<u8>,
        levels: Vec<u8>,
        offs: usize,
    }

    impl Run {
        fn new() -> Self {
            Self::with(Faults::NONE)
        }

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
            self.level()
        }

        fn secs(&mut self, seconds: u32) -> u8 {
            self.ticks(seconds * u32::from(TICKS_PER_SECOND))
        }

        fn until_fault(&mut self) {
            while self.limits.health() == Health::Good {
                self.tick();
            }
        }

        fn lit(&self) -> Vec<u8> {
            self.levels.iter().copied().filter(|&l| l > 0).collect()
        }

        fn level(&self) -> u8 {
            *self.levels.last().unwrap()
        }
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
        run.adc.temperature = celsius_reading(70);
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
        run.adc.temperature = celsius_reading(80);
        assert_eq!(run.secs(120), FLOOR);
        assert!(run.levels.iter().all(|&l| l == 0 || l >= FLOOR));
        run.adc.temperature = celsius_reading(25);
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
        run.adc.temperature = celsius_reading(80);
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
        for level in 35..50 {
            assert!(run.levels.contains(&level), "{level}");
        }
        run.secs(30);
        assert_eq!(run.offs, 1);
        assert_eq!(run.level(), 0);
        assert_eq!(run.limits.health(), Health::Flat);
    }

    #[test]
    fn a_flat_cell_fades_down_the_ramp_before_it_turns_off() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(1);
        run.adc.voltage = voltage_reading(280);
        assert_eq!(run.secs(4), CEILING);
        assert_eq!(run.offs, 0);
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
        run.ui = 50;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.offs, 2);
        run.adc.voltage = voltage_reading(battery::CHARGED_AT - 1);
        run.secs(10);
        assert_eq!(run.limits.health(), Health::Flat);
        run.adc.voltage = voltage_reading(380);
        run.ticks(u32::from(CYCLE_TICKS));
        assert_eq!(run.limits.health(), Health::Good);
        run.ui = 50;
        assert_eq!(run.ticks(1), 50, "as if new, with no ceiling");
        assert_eq!(run.secs(30), 50);
    }

    /// The most ticks from the one the cell falls under the floor on to the
    /// one the light's out on, counting both.
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
            assert!(run.levels[fell..fell + lit].iter().all(|&l| l == CEILING));
            assert!(run.levels[fell + lit..].iter().all(|&l| l == 0), "at {at}");
            assert_eq!(run.offs, 1, "at {at}");
            assert_eq!(run.limits.health(), Health::Flat, "at {at}");
            took.push(lit + 1);
        }
        // Two readings a tick apart at best.
        assert_eq!(took.iter().min(), Some(&3));
        assert_eq!(took.iter().max(), Some(&COLLAPSE_TICKS));
    }

    #[test]
    fn one_reading_under_the_floor_does_nothing() {
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ui = 100;
            run.ticks(64 + at);
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
        let lit = run.lit();
        assert_eq!(lit.last(), Some(&1));
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = voltage_reading(battery::COLLAPSE_BELOW - 1);
        run.secs(2);
        let lit = run.lit();
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
        run.ui = CEILING;
        assert_eq!(run.ticks(1), 0);
        assert_eq!(run.offs, 2);
        run.adc.voltage = voltage_reading(battery::CHARGED_AT - 1);
        run.secs(10);
        assert_eq!(run.limits.health(), Health::Flat);
        run.adc.voltage = voltage_reading(battery::CHARGED_AT);
        run.ticks(u32::from(CYCLE_TICKS));
        assert_eq!(run.limits.health(), Health::Good);
        run.ui = 50;
        assert_eq!(run.ticks(1), 50);
        assert_eq!(run.limits.ceilings().0, CEILING);
    }

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
            run.ticks(u32::from(CYCLE_TICKS) + COLLAPSE_TICKS as u32);
            let lit = run.levels[on..].iter().take_while(|&&l| l > 0).count();
            assert_eq!(run.level(), 0, "on at {at}");
            assert_eq!(run.limits.health(), Health::Flat, "on at {at}");
            took.push(lit);
        }
        // The third and fourth ticks' conversions at best. At worst the
        // voltage's turn ends before two have counted, and the next round's do.
        assert_eq!(took.iter().min(), Some(&4));
        assert_eq!(took.iter().max(), Some(&13), "{took:?}");
    }

    #[test]
    fn a_collapse_does_not_wait_for_a_faults_fade() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = 0;
        run.until_fault();
        run.adc.voltage = voltage_reading(270);
        run.ticks(COLLAPSE_TICKS as u32);
        assert_eq!(run.offs, 1);
        assert_eq!(run.level(), 0);
        let lit = run.lit();
        let last = *lit.last().unwrap();
        assert!(
            last > CEILING - 20,
            "out from {last}, well short of the bottom"
        );
        assert_eq!(run.limits.health(), Health::Fault);
        run.adc.voltage = voltage_reading(400);
        run.secs(10);
        assert_eq!(run.limits.health(), Health::Fault);
    }

    #[test]
    fn vddio2_missing_outranks_a_collapse_and_both_are_out_at_once() {
        for gap in 0..3 {
            let mut run = Run::new();
            run.ui = CEILING;
            run.secs(2);
            run.adc.voltage = voltage_reading(270);
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
        assert_eq!(run.secs(60), 0);
        assert_eq!(run.limits.health(), Health::Good);
        assert_eq!(run.offs, 0);
    }

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
        // The four-a-second conversion starts on the round's second tick.
        assert_eq!(moved, [1]);
    }

    #[test]
    fn a_low_cell_during_a_faults_fade_still_fades_a_level_a_tick() {
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
        let lit = run.lit();
        for level in 1..CEILING {
            assert!(lit.contains(&level), "faded through {level}");
        }
        assert_eq!(lit.last(), Some(&1));
    }

    #[test]
    fn garbage_voltages_fade_it_out_and_latch_a_fault() {
        for garbage in [0, FULL_SCALE, 1] {
            let mut run = Run::new();
            run.ui = CEILING;
            run.secs(2);
            run.adc.voltage = garbage;
            run.ticks(3 * u32::from(CYCLE_TICKS));
            assert_eq!(run.ticks(1), CEILING, "{garbage}");
            assert_eq!(run.limits.health(), Health::Good, "{garbage}");
            run.secs(3);
            assert_eq!(run.offs, 1, "{garbage}");
            assert_eq!(run.limits.health(), Health::Fault, "{garbage}");
            let lit = run.lit();
            for level in 1..CEILING {
                assert!(lit.contains(&level), "{garbage}: faded through {level}");
            }
            assert_eq!(lit.last(), Some(&1), "{garbage}");
            run.adc.voltage = voltage_reading(380);
            run.secs(10);
            run.ui = CEILING;
            assert_eq!(run.ticks(1), 0, "{garbage}");
            assert_eq!(run.offs, 2, "{garbage}");
            assert_eq!(run.limits.health(), Health::Fault, "{garbage}");
        }
    }

    #[test]
    fn ramping_down_through_a_fade_still_moves_a_level_a_tick() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = 0;
        run.until_fault();
        let mut last = run.limits.level();
        assert_eq!(last, CEILING - 1, "the fade's first step");
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
        run.until_fault();
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
        assert_eq!(run.level(), CEILING);
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

    #[test]
    fn an_adc_stuck_on_a_plausible_voltage_is_a_fault() {
        for at in 0..u32::from(CYCLE_TICKS) {
            let mut run = Run::new();
            run.ui = 100;
            run.ticks(64 + at);
            let raw = run.adc.voltage;
            run.adc.stuck(raw);
            // Two ground readings a round apart, the second due 33 ticks on
            // at worst.
            run.ticks(2 * u32::from(CYCLE_TICKS));
            if run.limits.health() == Health::Good {
                run.ticks(1);
            }
            assert_eq!(run.limits.health(), Health::Fault, "at {at}");
            assert_eq!(run.secs(3), 0, "at {at}");
            assert_eq!(run.offs, 1, "at {at}");
            let lit = run.lit();
            assert_eq!(lit.last(), Some(&1), "faded out, at {at}");
        }
    }

    #[test]
    fn one_bad_ground_reading_is_no_fault() {
        let mut run = Run::new();
        run.ui = 100;
        for _ in 0..60 {
            run.adc.ground = CELL_READING;
            run.ticks(u32::from(CYCLE_TICKS));
            run.adc.ground = crate::testing::GROUND_READING;
            run.ticks(u32::from(CYCLE_TICKS));
        }
        assert_eq!(run.limits.health(), Health::Good);
        assert_eq!(run.level(), 100);
    }

    #[test]
    fn a_stuck_adc_with_the_light_off_and_the_switch_down_is_a_fault_too() {
        let mut run = Run::new();
        run.adc.stuck(voltage_reading(380));
        run.secs(1);
        assert_eq!(run.limits.health(), Health::Fault);
        run.ui = CEILING;
        assert_eq!(run.ticks(1), 0);
    }

    /// A known gap, pinned so that closing it has to change this test.
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

    /// The same known gap, for the temperature.
    #[test]
    fn a_plausible_temperature_that_never_changes_never_steps_down() {
        let mut run = Run::new();
        assert_eq!(run.adc.temperature, celsius_reading(25));
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
        run.adc.temperature = celsius_reading(70);
        run.secs(60);
        assert_eq!(run.limits.ceilings().1, FLOOR);
        run.ui = 0;
        run.adc.temperature = celsius_reading(25);
        run.secs(1);
        run.ui = CEILING;
        assert_eq!(run.ticks(1), CEILING);
    }

    #[test]
    fn a_rest_starts_no_conversions_and_stays_dark() {
        let mut run = Run::new();
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
        run.adc.temperature = celsius_reading(70);
        run.secs(120);
        assert_eq!(run.limits.ceilings().1, FLOOR);
        run.ui = 0;
        run.ticks(1);
        run.limits.rest();
        run.adc.temperature = celsius_reading(25);
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
        let mut run = Run::with(HUNG);
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

    #[test]
    fn an_unloaded_reading_is_one_with_the_light_out_for_two_ticks_before_it() {
        let due = |tick: usize| tick % usize::from(CYCLE_TICKS) == 2;
        for voltage in [voltage_reading(320), 0, FULL_SCALE] {
            let mut limits = Limits::new(CALIBRATION, Faults::NONE);
            let mut adc = Adc::new();
            adc.voltage = voltage;
            for tick in 0..64 {
                let step = limits.tick(0, adc.result(), true);
                adc.start(limits.convert());
                let read = due(tick)
                    .then(|| battery::centivolts(voltage))
                    .filter(|&volts| battery::plausible(volts));
                assert_eq!(step.unloaded, read, "{voltage}: tick {tick}");
            }
        }
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

    #[test]
    fn a_check_at_rest_seeds_the_filter_afresh_and_leaves_the_ceiling() {
        use crate::torch::CHECK_TICKS;
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
        for (voltage, temperature) in [
            (0, temperature),
            (voltage, 0),
            (0, 0),
            (FULL_SCALE, FULL_SCALE),
        ] {
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
        let mut run = Run::with(HUNG);
        assert_eq!(run.limits.health(), Health::Fault);
        assert_eq!(run.secs(10), 0);
        for ui in [1, 30, CEILING, LEVELS] {
            run.ui = ui;
            assert_eq!(run.secs(10), 0, "{ui}");
        }
        assert!(run.levels.iter().all(|&l| l == 0));
        assert_eq!(run.offs, 4, "asked for off every time");
    }

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

    #[test]
    fn vddio2_missing_cuts_another_faults_fade_short() {
        let mut run = Run::new();
        run.ui = CEILING;
        run.secs(2);
        run.adc.voltage = 0;
        run.until_fault();
        let fading = run.ticks(10);
        assert!(fading > 1 && fading < CEILING, "{fading}");
        run.vddio2 = false;
        assert_eq!(run.ticks(1), 0, "from {fading}");
        assert_eq!(run.offs, 1);
    }

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

    fn throw_noise_at(mut limits: Limits, mut cap: u8, seed: u32, gone_at: Option<u32>) {
        let mut rng = crate::testing::Rng::new(seed);
        let mut ticks = 0_u32;
        let mut tick = 0_u32;
        while ticks < 1_000_000 {
            let ui = match rng.next_u16() % 8 {
                0 => 0,
                1 => 255,
                _ => rng.next_u16() as u8,
            };
            let run = 1 + u32::from(rng.next_u16() % 4000);
            let mode = rng.next_u16() % 3;
            let held = rng.next_u16() % (FULL_SCALE + 1);
            for _ in 0..run {
                let result = match (rng.next_u16() % 4, mode) {
                    (0, _) => None,
                    (_, 0) => Some(rng.next_u16()),
                    (_, 1) => Some(rng.next_u16() % (FULL_SCALE + 1)),
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
        throw_noise_at(Limits::new(CALIBRATION, HUNG), 0, 0x5EED_0002, None);
    }

    #[test]
    fn random_readings_never_light_it_once_vddio2_has_read_missing() {
        for gone_at in [0, 1, 2, 5_000, 123_456] {
            let limits = Limits::new(CALIBRATION, Faults::NONE);
            throw_noise_at(limits, CEILING, 0x5EED_0007, Some(gone_at));
        }
    }
}
