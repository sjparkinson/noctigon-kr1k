//! Sharing the one ADC between the cell's voltage, the temperature sensor and
//! a ground check, in a round of 16 ticks.

/// Every reading is four 12-bit conversions accumulated, the temperature's
/// against 2.048 V, which its factory calibration is for (DS40002413B
/// §33.3.3.8), and the rest against 1.024 V.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Voltage,
    Temperature,
    Ground,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Convert {
    pub channel: Channel,
    /// Set the ADC and the reference up for `channel` first.
    pub setup: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Voltage(u16),
    Temperature(u16),
    /// A voltage conversion between two [`Reading::Voltage`]s, lit only. It's
    /// for `battery::Protection::quick` and nothing else.
    Quick(u16),
    Ground(u16),
}

pub const FULL_SCALE: u16 = 4 * 4095;

/// Each channel reads four times a second, which `battery` and `thermal` expect.
pub const CYCLE_TICKS: u8 = 16;
const TEMPERATURE_AT: u8 = CYCLE_TICKS / 2;
/// A check of the cell at rest never gets this far, on purpose.
const GROUND_AT: u8 = TEMPERATURE_AT + READING_STEP + 1;

const _: () = assert!(GROUND_AT + READING_STEP < CYCLE_TICKS);

/// In counts of four accumulated conversions. Three times the ±5 LSb offset
/// error in DS40002413B table 38-24, since figure 39-112 has it growing as
/// the reference falls. Unmeasured.
pub const GROUND_MAX: u16 = 64;

/// Ground readings in a row over [`GROUND_MAX`] that count as a stuck ADC. Unmeasured.
pub const GROUND_MISSES: u8 = 2;

/// Catches an ADC that's stuck or follows the wrong input. It can't catch a
/// reading that's wrong and still moving, or a reference that's drifted.
#[derive(Clone, Debug, Default)]
pub struct Ground {
    misses: u8,
}

impl Ground {
    pub const fn new() -> Self {
        Self { misses: 0 }
    }

    /// A missing result reads as zero and passes, since a dead ADC is the
    /// voltage's to catch.
    pub const fn reading(&mut self, raw: u16) -> bool {
        if raw <= GROUND_MAX {
            self.misses = 0;
            return false;
        }
        self.misses = self.misses.saturating_add(1);
        self.misses >= GROUND_MISSES
    }

    pub const fn rest(&mut self) {
        self.misses = 0;
    }
}
/// The first conversion of a turn is thrown away. The datasheet doesn't ask for
/// that, only gives settling times (DS40002413B tables 38-25 and 38-18), and
/// the stock firmware does it too.
const READING_STEP: u8 = 2;

const _: () = assert!(CYCLE_TICKS.is_power_of_two());

/// The longest the cell goes unread with the light on, in ticks.
pub const UNREAD_TICKS: u8 = CYCLE_TICKS - TEMPERATURE_AT + READING_STEP;

#[derive(Clone, Debug, Default)]
pub struct Schedule {
    phase: u8,
    temperature: bool,
    quick: bool,
}

impl Schedule {
    pub const fn new() -> Self {
        Self {
            phase: 0,
            temperature: false,
            quick: false,
        }
    }

    pub const fn rest(&mut self) {
        *self = Self::new();
    }

    pub fn tick(&mut self, result: Option<u16>, lit: bool) -> (Option<Reading>, Option<Convert>) {
        let phase = self.phase;
        self.phase = phase.wrapping_add(1) & (CYCLE_TICKS - 1);
        let raw = result.unwrap_or(0);
        let quick = self.quick.then_some(Reading::Quick(raw));
        self.quick = false;
        let (channel, step) = match (
            phase.checked_sub(GROUND_AT),
            phase.checked_sub(TEMPERATURE_AT),
        ) {
            (Some(step), _) => (Channel::Ground, step),
            (None, Some(step)) => (Channel::Temperature, step),
            (None, None) => (Channel::Voltage, phase),
        };
        if step == 0 && channel == Channel::Temperature {
            self.temperature = lit;
        }
        if channel == Channel::Temperature && !self.temperature {
            return (quick, None);
        }
        let convert = |setup| Some(Convert { channel, setup });
        let (reading, start) = match step {
            0 => (None, convert(true)),
            1 => (None, convert(false)),
            READING_STEP => {
                let reading = match channel {
                    Channel::Voltage => Reading::Voltage(raw),
                    Channel::Temperature => Reading::Temperature(raw),
                    Channel::Ground => Reading::Ground(raw),
                };
                (Some(reading), None)
            }
            _ => (None, None),
        };
        if channel == Channel::Voltage && step >= READING_STEP && lit {
            self.quick = true;
            return (reading.or(quick), convert(false));
        }
        (reading.or(quick), start)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::testing::CELL_READING;
    use std::vec::Vec;

    fn run(
        schedule: &mut Schedule,
        ticks: usize,
        result: Option<u16>,
        lit: bool,
    ) -> Vec<(Option<Reading>, Option<Convert>)> {
        (0..ticks).map(|_| schedule.tick(result, lit)).collect()
    }

    const fn convert(channel: Channel, setup: bool) -> Convert {
        Convert { channel, setup }
    }

    fn dark_round() -> [(Option<Reading>, Option<Convert>); 16] {
        let mut expected = [(None, None); 16];
        expected[0].1 = Some(convert(Channel::Voltage, true));
        expected[1].1 = Some(convert(Channel::Voltage, false));
        expected[2].0 = Some(Reading::Voltage(7));
        expected[11].1 = Some(convert(Channel::Ground, true));
        expected[12].1 = Some(convert(Channel::Ground, false));
        expected[13].0 = Some(Reading::Ground(7));
        expected
    }

    #[test]
    fn dark_it_reads_the_voltage_then_ground() {
        let mut schedule = Schedule::new();
        assert_eq!(run(&mut schedule, 16, Some(7), false), dark_round());
    }

    #[test]
    fn a_check_at_rest_reads_the_cell_and_no_ground() {
        let mut schedule = Schedule::new();
        for _ in 0..10 {
            let ticks = run(&mut schedule, 3, Some(7), false);
            assert_eq!(ticks[..], dark_round()[..3]);
            assert_eq!(ticks[2].1, None, "nothing running, so it rests");
            schedule.rest();
        }
    }

    #[test]
    fn ground_reads_against_the_voltages_reference_and_is_set_up_every_time() {
        for lit in [false, true] {
            let mut schedule = Schedule::new();
            let ticks = run(&mut schedule, 64, Some(7), lit);
            for round in ticks.chunks(16) {
                assert_eq!(
                    round[11].1,
                    Some(convert(Channel::Ground, true)),
                    "lit: {lit}"
                );
                assert_eq!(
                    round[0].1,
                    Some(convert(Channel::Voltage, true)),
                    "lit: {lit}"
                );
            }
        }
    }

    #[test]
    fn lit_it_reads_the_voltage_every_tick_then_the_temperature() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 16, Some(7), true);
        let mut expected = dark_round();
        for (_, start) in &mut expected[2..8] {
            *start = Some(convert(Channel::Voltage, false));
        }
        for (reading, _) in &mut expected[3..9] {
            *reading = Some(Reading::Quick(7));
        }
        expected[8].1 = Some(convert(Channel::Temperature, true));
        expected[9].1 = Some(convert(Channel::Temperature, false));
        expected[10].0 = Some(Reading::Temperature(7));
        assert_eq!(ticks, expected);
    }

    #[test]
    fn each_channel_comes_round_every_16_ticks() {
        for lit in [false, true] {
            let mut schedule = Schedule::new();
            let ticks = run(&mut schedule, 160, Some(1), lit);
            let due = |wanted: fn(&Reading) -> bool| -> Vec<usize> {
                ticks
                    .iter()
                    .enumerate()
                    .filter(|(_, (reading, _))| reading.as_ref().is_some_and(wanted))
                    .map(|(tick, _)| tick)
                    .collect()
            };
            let every_16_from =
                |at: usize| -> Vec<usize> { (0..10).map(|n| at + 16 * n).collect() };
            assert_eq!(
                due(|r| matches!(r, Reading::Voltage(_))),
                every_16_from(2),
                "lit: {lit}"
            );
            assert_eq!(
                due(|r| matches!(r, Reading::Ground(_))),
                every_16_from(13),
                "lit: {lit}"
            );
            let temperatures = due(|r| matches!(r, Reading::Temperature(_)));
            let quick = due(|r| matches!(r, Reading::Quick(_)));
            if lit {
                assert_eq!(temperatures, every_16_from(10));
                assert_eq!(quick.len(), 6 * 10);
            } else {
                assert_eq!((temperatures, quick), (Vec::new(), Vec::new()));
            }
        }
    }

    #[test]
    fn lit_the_cell_goes_unread_for_ten_ticks_in_every_sixteen() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 160, Some(1), true);
        let read: Vec<usize> = (1..ticks.len())
            .filter(|&tick| matches!(ticks[tick].0, Some(Reading::Voltage(_) | Reading::Quick(_))))
            .map(|tick| tick - 1)
            .collect();
        assert_eq!(read[..7], [1, 2, 3, 4, 5, 6, 7]);
        let longest = read.windows(2).map(|pair| pair[1] - pair[0]).max();
        assert_eq!(longest, Some(usize::from(UNREAD_TICKS)));
        assert_eq!(UNREAD_TICKS, 10);
    }

    #[test]
    fn quick_readings_stop_with_the_light_and_one_already_started_comes_back() {
        for out_at in 3..9 {
            let mut schedule = Schedule::new();
            let lit = run(&mut schedule, out_at, Some(7), true);
            assert_eq!(lit[out_at - 1].1, Some(convert(Channel::Voltage, false)));
            assert_eq!(
                schedule.tick(Some(9), false),
                (Some(Reading::Quick(9)), None),
                "out at {out_at}"
            );
            let rest = run(&mut schedule, 15 - out_at, Some(7), false);
            assert_eq!(rest[..], dark_round()[out_at + 1..], "{out_at}");
        }
    }

    #[test]
    fn a_light_that_comes_on_starts_quick_readings_in_what_is_left_of_the_voltages_turn() {
        for on_at in 3..16 {
            let mut schedule = Schedule::new();
            run(&mut schedule, on_at, Some(7), false);
            let lit = run(&mut schedule, 16 - on_at, Some(7), true);
            for (phase, (reading, start)) in (on_at..16).zip(&lit) {
                let quick = (on_at + 1..=8).contains(&phase);
                let voltage = (phase < 8).then_some(Channel::Voltage);
                assert_eq!(
                    (
                        matches!(reading, Some(Reading::Quick(7))),
                        start.map(|c| c.channel).filter(|&c| c == Channel::Voltage),
                        start.is_some_and(|c| c.setup && c.channel == Channel::Voltage),
                    ),
                    (quick, voltage, false),
                    "on at {on_at}, phase {phase}"
                );
            }
            let next = run(&mut schedule, 16, Some(9), true);
            assert_eq!(next[3].0, Some(Reading::Quick(9)), "on at {on_at}");
        }
    }

    #[test]
    fn the_first_conversion_after_a_setup_is_never_a_reading() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 64, Some(1), true);
        for (tick, (reading, _)) in ticks.iter().enumerate().skip(1) {
            if ticks[tick - 1].1.is_some_and(|c| c.setup) {
                assert_eq!(*reading, None, "tick {tick}");
            }
        }
    }

    #[test]
    fn leaves_the_temperature_alone_when_not_asked() {
        let mut schedule = Schedule::new();
        for (reading, convert) in run(&mut schedule, 160, Some(1), false) {
            assert!(!matches!(reading, Some(Reading::Temperature(_))));
            assert!(convert.is_none_or(|c| c.channel != Channel::Temperature));
        }
    }

    #[test]
    fn decides_on_the_temperature_at_the_start_of_its_turn() {
        let mut schedule = Schedule::new();
        run(&mut schedule, 8, Some(1), false);
        assert_eq!(
            schedule.tick(Some(1), true),
            (None, Some(convert(Channel::Temperature, true)))
        );
        assert_eq!(
            schedule.tick(Some(1), false),
            (None, Some(convert(Channel::Temperature, false)))
        );
        assert_eq!(
            schedule.tick(Some(1), false),
            (Some(Reading::Temperature(1)), None)
        );
        run(&mut schedule, 13, Some(1), false);
        assert_eq!(schedule.tick(Some(1), false), (None, None));
        assert_eq!(schedule.tick(Some(1), true), (None, None));
    }

    #[test]
    fn sets_the_adc_up_at_the_start_of_every_turn() {
        let mut schedule = Schedule::new();
        let setups = |ticks: &[(Option<Reading>, Option<Convert>)]| -> Vec<Channel> {
            ticks
                .iter()
                .filter_map(|(_, convert)| convert.filter(|c| c.setup).map(|c| c.channel))
                .collect()
        };
        let off = run(&mut schedule, 64, Some(1), false);
        let round = [Channel::Voltage, Channel::Ground];
        assert_eq!(setups(&off), round.repeat(4));
        let on = run(&mut schedule, 64, Some(1), true);
        let round = [Channel::Voltage, Channel::Temperature, Channel::Ground];
        assert_eq!(setups(&on), round.repeat(4));
    }

    #[test]
    fn a_rest_starts_the_round_again_and_sets_the_adc_up() {
        let mut schedule = Schedule::new();
        run(&mut schedule, 21, Some(1), true);
        schedule.rest();
        let ticks = run(&mut schedule, 3, Some(7), false);
        assert_eq!(ticks[..], dark_round()[..3]);
    }

    #[test]
    fn a_result_with_no_conversion_started_is_ignored() {
        let mut schedule = Schedule::new();
        let mut readings = Vec::new();
        for tick in 0..160 {
            let phase = tick % 16;
            let result = (![2, 10, 13].contains(&phase)).then_some(0x155);
            let (reading, _) = schedule.tick(result, true);
            readings.push((phase, reading));
        }
        for (phase, reading) in readings {
            match phase {
                2 => assert_eq!(reading, Some(Reading::Voltage(0))),
                3..=8 => assert_eq!(reading, Some(Reading::Quick(0x155))),
                10 => assert_eq!(reading, Some(Reading::Temperature(0))),
                13 => assert_eq!(reading, Some(Reading::Ground(0))),
                _ => assert_eq!(reading, None, "phase {phase}"),
            }
        }
        let mut schedule = Schedule::new();
        for tick in 0..160 {
            let (reading, convert) = schedule.tick(Some(0x155), false);
            match tick % 16 {
                2 => assert_eq!(reading, Some(Reading::Voltage(0x155))),
                13 => assert_eq!(reading, Some(Reading::Ground(0x155))),
                phase => assert_eq!(reading, None, "phase {phase}"),
            }
            if (2..11).contains(&(tick % 16)) || tick % 16 >= 13 {
                assert_eq!(convert, None, "tick {tick}");
            }
        }
    }

    #[test]
    fn ground_near_zero_is_a_working_adc() {
        let mut ground = Ground::new();
        for raw in 0..=GROUND_MAX {
            for _ in 0..10 {
                assert!(!ground.reading(raw), "{raw}");
            }
        }
    }

    #[test]
    fn two_ground_readings_in_a_row_well_off_zero_are_a_stuck_adc() {
        assert_eq!((GROUND_MAX, GROUND_MISSES), (64, 2));
        for raw in [GROUND_MAX + 1, CELL_READING, FULL_SCALE, u16::MAX] {
            let mut ground = Ground::new();
            assert!(!ground.reading(raw), "{raw}: one");
            assert!(ground.reading(raw), "{raw}: two");
        }
    }

    #[test]
    fn a_good_ground_reading_or_a_rest_starts_the_count_again() {
        let mut ground = Ground::new();
        for _ in 0..100 {
            assert!(!ground.reading(CELL_READING));
            assert!(!ground.reading(GROUND_MAX));
        }
        for _ in 0..100 {
            assert!(!ground.reading(CELL_READING));
            ground.rest();
        }
        assert!(!ground.reading(CELL_READING));
        assert!(!ground.reading(0));
    }

    #[test]
    fn every_plausible_cell_reading_is_far_over_grounds_limit() {
        let smallest = (0..=FULL_SCALE)
            .find(|&raw| crate::battery::plausible(crate::battery::centivolts(raw)))
            .unwrap();
        assert!(smallest > 20 * GROUND_MAX, "{smallest}");
    }

    #[test]
    fn no_result_when_one_is_due_reads_as_zero() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 16, None, true);
        assert_eq!(ticks[2].0, Some(Reading::Voltage(0)));
        assert_eq!(ticks[5].0, Some(Reading::Quick(0)));
        assert_eq!(ticks[10].0, Some(Reading::Temperature(0)));
        assert_eq!(ticks[13].0, Some(Reading::Ground(0)));
    }
}
