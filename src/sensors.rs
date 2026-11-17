//! Sharing the one ADC between the cell's voltage and the temperature
//! sensor.
//!
//! The ADC alternates every 8 ticks, so each gets a reading every 16 ticks
//! (four a second, what `battery::Protection` and `thermal::Thermal` expect),
//! the temperature 8 ticks after the voltage. Each turn takes three ticks.
//!
//! ```text
//! tick 0   set the ADC up for the voltage, start a conversion
//! tick 1   throw that result away, start another
//! tick 2   that result is the voltage
//! tick 8   set it up for the temperature, start a conversion
//! tick 9   throw that result away, start another
//! tick 10  that result is the temperature
//! ```
//!
//! While the light's on, the voltage's turn carries on to the tick before the
//! temperature's, a conversion a tick, for `battery`'s hard floor, which
//! wants to see a cell collapsing under load sooner than four times a second.
//!
//! ```text
//! tick 2   that result is the voltage, as above. Start another
//! tick 3   that result is a quick voltage, start another
//! tick 4   the same
//! tick 5   the same
//! tick 6   the same
//! tick 7   the same, and the last one started
//! tick 8   that result is the last quick voltage
//! ```
//!
//! Those need no setup and nothing thrown away, since neither the input nor
//! the reference has moved since the conversion before. Tick 2's reading
//! keeps its tick and its meaning, lit or not, and is the only one
//! `battery::Protection`'s filter and holds ever see. The quick ones are
//! [`Reading::Quick`], and feed the hard floor and nothing else. Each is
//! decided on the tick it starts, so they stop with the light, and one that's
//! already started still comes back.
//!
//! That's a voltage conversion started on each of ticks 1 to 7, and none on
//! the nine ticks between tick 7 and the next round's tick 1, which belong
//! to the temperature. So with the light on the cell goes unread for ten
//! ticks in every sixteen, 156 ms.
//!
//! The ADC's only set up when the channel changes. While a click's being
//! counted with the light off it reads nothing but the voltage, so after the
//! first round it skips the setup and goes straight to converting.
//!
//! A conversion takes a fraction of a millisecond, so it's always done by
//! the next tick, and nothing ever waits on the ADC.
//!
//! The datasheet doesn't ask for the first conversion after a switch to be
//! thrown away. It does say the temperature sensor wants about 40 µs after
//! MUXPOS moves to it, and a reference about 10 µs to start (DS40002413B
//! tables 38-25 and 38-18), and the two channels use different references,
//! 1.024 V for the cell and 2.048 V for the temperature, so both the input
//! and the reference move at each switch. The stock firmware throws the
//! first one away too ("first result is unstable"). It costs a tick, so it's
//! cheap insurance.
//!
//! The temperature is only read when asked for, which the caller does while
//! the light's on. The voltage is read until [`Schedule::rest`], since the
//! ADC can't run in power-down, and starts again from the top after it.

/// What the ADC is measuring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// VDD / 10, which is the cell, against the internal 1.024 V reference,
    /// four 12-bit conversions accumulated, for `battery::centivolts`.
    Voltage,
    /// The temperature sensor against the internal 2.048 V reference, which
    /// its factory calibration is for (DS40002413B section 33.3.3.8), four
    /// 12-bit conversions accumulated, for `temp::sixteenths`.
    Temperature,
}

/// A conversion for the firmware to start this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Convert {
    pub channel: Channel,
    /// Set the ADC (and the reference) up for `channel` first, turning them
    /// on if they're off. Only the first conversion of a turn on a different
    /// channel from the last turn's needs it, or the first after a rest.
    pub setup: bool,
}

/// A reading that's come due, straight from the ADC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// The voltage, four times a second, lit or not, for all of
    /// `battery::Protection`.
    Voltage(u16),
    Temperature(u16),
    /// The voltage again, from one of the conversions between two
    /// [`Reading::Voltage`]s that only run while the light's on. It's for
    /// `battery::Protection::quick` and nothing else.
    Quick(u16),
}

/// Ticks in a full round of both channels, and so how many apart each
/// channel's readings come.
pub const CYCLE_TICKS: u8 = 16;
/// Where the temperature's turn starts in the round.
const TEMPERATURE_AT: u8 = CYCLE_TICKS / 2;
/// The step of a turn whose result is its reading, after the set-up and the
/// throwaway.
const READING_STEP: u8 = 2;

const _: () = assert!(CYCLE_TICKS.is_power_of_two());

/// The longest the cell goes unread with the light on, in ticks.
///
/// It's from the last voltage conversion of the voltage's turn to the first
/// one after it that isn't thrown away, which is ten ticks, from tick 7 to
/// the next round's tick 1.
pub const UNREAD_TICKS: u8 = CYCLE_TICKS - TEMPERATURE_AT + READING_STEP;

#[derive(Clone, Debug, Default)]
pub struct Schedule {
    phase: u8,
    /// Whether this round reads the temperature.
    temperature: bool,
    /// Whether last tick started a quick voltage conversion, whose result is
    /// due this tick.
    quick: bool,
    /// The channel the ADC was last set up for, `None` before the first.
    /// Its registers keep their values until a reset, which starts this
    /// afresh too. [`Schedule::rest`] puts it back to `None`, since the
    /// firmware turns the ADC off to power down.
    set_up: Option<Channel>,
}

impl Schedule {
    pub const fn new() -> Self {
        Self {
            phase: 0,
            temperature: false,
            quick: false,
            set_up: None,
        }
    }

    /// Back to where it started, for after the firmware's powered down. The
    /// next tick sets the ADC up again, and throws that conversion away as
    /// usual, which gives the reference a tick to start.
    pub const fn rest(&mut self) {
        *self = Self::new();
    }

    /// One tick. `result` is the ADC's result if it has one ready, and `lit`
    /// whether the light's on, which is when the temperature's read and the
    /// quick voltage readings are taken. For the temperature it only matters
    /// on the tick its turn starts, and for a quick reading on the tick its
    /// conversion would start.
    ///
    /// Returns the reading that's due this tick, if any, and the conversion
    /// to start. A reading that's due when the ADC has no result comes out as
    /// zero, which both channels treat as garbage.
    pub fn tick(&mut self, result: Option<u16>, lit: bool) -> (Option<Reading>, Option<Convert>) {
        let phase = self.phase;
        self.phase = phase.wrapping_add(1) & (CYCLE_TICKS - 1);
        let raw = result.unwrap_or(0);
        // A quick conversion started last tick is due now, whichever turn
        // this tick belongs to.
        let quick = self.quick.then_some(Reading::Quick(raw));
        self.quick = false;
        let (channel, step) = match phase.checked_sub(TEMPERATURE_AT) {
            None => (Channel::Voltage, phase),
            Some(step) => (Channel::Temperature, step),
        };
        if step == 0 && channel == Channel::Temperature {
            self.temperature = lit;
        }
        if channel == Channel::Temperature && !self.temperature {
            return (quick, None);
        }
        let convert = |setup| Some(Convert { channel, setup });
        let (reading, start) = match step {
            0 => {
                let setup = self.set_up != Some(channel);
                self.set_up = Some(channel);
                (None, convert(setup))
            }
            1 => (None, convert(false)),
            READING_STEP => {
                let reading = match channel {
                    Channel::Voltage => Reading::Voltage(raw),
                    Channel::Temperature => Reading::Temperature(raw),
                };
                (Some(reading), None)
            }
            _ => (None, None),
        };
        // The voltage's turn carries on while the light's on. The ADC is
        // still set up for the voltage, so there's no setup and nothing to
        // throw away.
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
    use std::vec::Vec;

    /// What `ticks` ticks of the schedule ask for and hand back, with the
    /// ADC's result always `result` and the light on or off.
    fn run(
        schedule: &mut Schedule,
        ticks: usize,
        result: Option<u16>,
        lit: bool,
    ) -> Vec<(Option<Reading>, Option<Convert>)> {
        (0..ticks).map(|_| schedule.tick(result, lit)).collect()
    }

    const fn convert(channel: Channel, setup: bool) -> Option<Convert> {
        Some(Convert { channel, setup })
    }

    /// A round with the light off, the voltage alone, as a press reads it.
    fn dark_round() -> [(Option<Reading>, Option<Convert>); 16] {
        let mut expected = [(None, None); 16];
        expected[0].1 = convert(Channel::Voltage, true);
        expected[1].1 = convert(Channel::Voltage, false);
        expected[2].0 = Some(Reading::Voltage(7));
        expected
    }

    #[test]
    fn dark_it_reads_the_voltage_and_nothing_else() {
        let mut schedule = Schedule::new();
        assert_eq!(run(&mut schedule, 16, Some(7), false), dark_round());
    }

    #[test]
    fn lit_it_reads_the_voltage_every_tick_then_the_temperature() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 16, Some(7), true);
        let mut expected = dark_round();
        // The voltage's turn carries on, a conversion a tick, up to the tick
        // before the temperature's.
        for (_, start) in &mut expected[2..8] {
            *start = convert(Channel::Voltage, false);
        }
        for (reading, _) in &mut expected[3..9] {
            *reading = Some(Reading::Quick(7));
        }
        expected[8].1 = convert(Channel::Temperature, true);
        expected[9].1 = convert(Channel::Temperature, false);
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
            // The four-a-second voltage keeps its tick whatever the light
            // does.
            assert_eq!(
                due(|r| matches!(r, Reading::Voltage(_))),
                every_16_from(2),
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

    /// The ticks a voltage conversion that counts is started on, with the
    /// light on, which is when the cell is looked at. A conversion measures
    /// what's there as it starts.
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
            assert_eq!(lit[out_at - 1].1, convert(Channel::Voltage, false));
            // The tick the light goes out starts nothing, and hands back
            // the conversion the tick before started.
            assert_eq!(
                schedule.tick(Some(9), false),
                (Some(Reading::Quick(9)), None),
                "out at {out_at}"
            );
            let rest = run(&mut schedule, 15 - out_at, Some(7), false);
            assert!(rest.iter().all(|&tick| tick == (None, None)), "{out_at}");
        }
    }

    #[test]
    fn quick_readings_start_with_the_light_partway_through_the_turn() {
        for on_at in 3..8 {
            let mut schedule = Schedule::new();
            run(&mut schedule, on_at, Some(7), false);
            assert_eq!(
                schedule.tick(Some(7), true),
                (None, convert(Channel::Voltage, false)),
                "on at {on_at}"
            );
            assert_eq!(schedule.tick(Some(9), true).0, Some(Reading::Quick(9)));
        }
        // Not once the voltage's turn is over, when the ADC may have moved.
        let mut schedule = Schedule::new();
        run(&mut schedule, 9, Some(7), false);
        let late = run(&mut schedule, 7, Some(7), true);
        assert!(late.iter().all(|&tick| tick == (None, None)));
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
            assert!(convert.is_none_or(|c| c.channel == Channel::Voltage));
        }
    }

    #[test]
    fn decides_on_the_temperature_at_the_start_of_its_turn() {
        let mut schedule = Schedule::new();
        run(&mut schedule, 8, Some(1), false);
        // Asked at the start of its turn, it finishes even if the ask goes.
        assert_eq!(
            schedule.tick(Some(1), true),
            (None, convert(Channel::Temperature, true))
        );
        assert_eq!(
            schedule.tick(Some(1), false),
            (None, convert(Channel::Temperature, false))
        );
        assert_eq!(
            schedule.tick(Some(1), false),
            (Some(Reading::Temperature(1)), None)
        );
        // Not asked at the start, it doesn't start halfway through.
        run(&mut schedule, 13, Some(1), false);
        assert_eq!(schedule.tick(Some(1), false), (None, None));
        assert_eq!(schedule.tick(Some(1), true), (None, None));
    }

    #[test]
    fn only_sets_the_adc_up_when_the_channel_changes() {
        let mut schedule = Schedule::new();
        let setups = |ticks: &[(Option<Reading>, Option<Convert>)]| -> Vec<Channel> {
            ticks
                .iter()
                .filter_map(|(_, convert)| convert.filter(|c| c.setup).map(|c| c.channel))
                .collect()
        };
        // Off, so the first voltage turn sets up and the rest don't.
        let off = run(&mut schedule, 64, Some(1), false);
        assert_eq!(off[0].1, convert(Channel::Voltage, true));
        assert_eq!(off[16].1, convert(Channel::Voltage, false));
        assert_eq!(setups(&off), [Channel::Voltage]);
        // On, so every turn switches channel and every turn sets up.
        let on = run(&mut schedule, 64, Some(1), true);
        let alternating = [Channel::Temperature, Channel::Voltage].repeat(4);
        assert_eq!(setups(&on)[..], alternating[..7]);
        // Off again, so the voltage turn after the last temperature one sets
        // up, and that's all.
        let off = run(&mut schedule, 64, Some(1), false);
        assert_eq!(setups(&off), [Channel::Voltage]);
    }

    #[test]
    fn a_rest_starts_the_round_again_and_sets_the_adc_up() {
        let mut schedule = Schedule::new();
        run(&mut schedule, 21, Some(1), true);
        schedule.rest();
        let ticks = run(&mut schedule, 3, Some(7), false);
        assert_eq!(ticks[0], (None, convert(Channel::Voltage, true)));
        assert_eq!(ticks[1], (None, convert(Channel::Voltage, false)));
        assert_eq!(ticks[2], (Some(Reading::Voltage(7)), None));
    }

    /// `Schedule` doesn't track whether a conversion is pending. It knows
    /// from the phase when a result is due and reads it then, and any other
    /// tick's result is dropped. So a result the ADC volunteers when nothing
    /// was asked for never comes back as a reading.
    #[test]
    fn a_result_with_no_conversion_started_is_ignored() {
        let mut schedule = Schedule::new();
        let mut readings = Vec::new();
        for tick in 0..160 {
            let phase = tick % 16;
            // A result on every tick except the two turns' due ones.
            let result = (phase != 2 && phase != 10).then_some(0x155);
            let (reading, _) = schedule.tick(result, true);
            readings.push((phase, reading));
        }
        for (phase, reading) in readings {
            match phase {
                2 => assert_eq!(reading, Some(Reading::Voltage(0))),
                // Asked for, a conversion a tick while the light's on.
                3..=8 => assert_eq!(reading, Some(Reading::Quick(0x155))),
                10 => assert_eq!(reading, Some(Reading::Temperature(0))),
                _ => assert_eq!(reading, None, "phase {phase}"),
            }
        }
        // And with the light off, nothing but the voltage is asked for, so
        // every other tick's result is ignored, the temperature's due tick
        // included.
        let mut schedule = Schedule::new();
        for tick in 0..160 {
            let (reading, convert) = schedule.tick(Some(0x155), false);
            match tick % 16 {
                2 => assert_eq!(reading, Some(Reading::Voltage(0x155))),
                phase => assert_eq!(reading, None, "phase {phase}"),
            }
            if tick % 16 >= 2 {
                assert_eq!(convert, None, "tick {tick}");
            }
        }
    }

    #[test]
    fn no_result_when_one_is_due_reads_as_zero() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 16, None, true);
        assert_eq!(ticks[2].0, Some(Reading::Voltage(0)));
        assert_eq!(ticks[5].0, Some(Reading::Quick(0)));
        assert_eq!(ticks[10].0, Some(Reading::Temperature(0)));
    }
}
