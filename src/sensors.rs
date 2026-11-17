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
//! While the light's off, the voltage's reading tick starts a turn for VDDIO2
//! instead, a measurement for the programmer to read and nothing more
//! ([`Supplies`]).
//!
//! ```text
//! tick 2   that result is the voltage, as above. Set the ADC up for
//!          VDDIO2 / 10 against 1.024 V, start a conversion
//! tick 3   throw that result away, start another
//! tick 4   that result is VDDIO2
//! ```
//!
//! It starts on tick 2 rather than 3 because the torch rests as soon as no
//! conversion is running, and a check of the cell at rest has to stay awake
//! for it. So a check is four ticks awake rather than two, and the VDDIO2
//! conversion starts two ticks, 31 ms, after the cell's. It's decided on tick
//! 2, and once started it finishes, light or not. A light that comes on
//! partway through gets no quick readings until the next round, since the
//! ADC is no longer on the voltage.
//!
//! After the temperature's turn, lit or not, comes ground's, a check on the
//! ADC itself ([`Ground`]).
//!
//! ```text
//! tick 11  set the ADC up for ground against 1.024 V, start a conversion
//! tick 12  throw that result away, start another
//! tick 13  that result is ground, which reads near zero on a working ADC
//! ```
//!
//! Ticks 14 and 15 start nothing. Ground's turn only comes round while the
//! torch is awake that long, which is while the light's on or a press is
//! being counted. A check of the cell at rest is ticks 0 to 2 and then rests
//! again (`torch::Torch`), so it never gets to tick 11, and that's on
//! purpose. Garbage at rest is never a fault (docs/ui.md), so a ground
//! reading there would have nothing to feed, and it would keep the chip
//! awake three ticks longer every five minutes.
//!
//! The ADC's set up whenever the channel changes, which is at the start of
//! every turn now ground has one, voltage included.
//!
//! A conversion takes a fraction of a millisecond, so it's always done by
//! the next tick, and nothing ever waits on the ADC.
//!
//! The datasheet doesn't ask for the first conversion after a switch to be
//! thrown away. It does say the temperature sensor wants about 40 µs after
//! MUXPOS moves to it, and a reference about 10 µs to start (DS40002413B
//! tables 38-25 and 38-18), and the channels don't all use one reference,
//! 1.024 V for the cell and ground and 2.048 V for the temperature, so the
//! input moves at every switch and the reference at some. It says nothing
//! about settling after a move to GND or VDDDIV10. The stock firmware throws
//! the first one away too ("first result is unstable"). It costs a tick, so
//! it's cheap insurance, and every turn does it, ground's included.
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
    /// The ADC's GND input (MUXPOS 0x40, DS40002413B section 33.5.7) against
    /// the internal 1.024 V reference, as the voltage is, four 12-bit
    /// conversions accumulated, for [`Ground`].
    Ground,
    /// VDDIO2 / 10 (MUXPOS 0x45, DS40002413B section 33.5.7), the supply
    /// PC1 hangs off, against the internal 1.024 V reference, as the voltage
    /// is, four 12-bit conversions accumulated, for [`Supplies`].
    Vddio2,
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
    /// Ground, for [`Ground`].
    Ground(u16),
    /// VDDIO2 / 10, for [`Supplies`].
    Vddio2(u16),
}

/// Where the firmware keeps the last [`Supplies`], a data-space address.
///
/// It's for `cargo xtask peek` to read. It's the start of SRAM, where the
/// linker puts the firmware's only static, and xtask's simulator checks the
/// ELF has it there.
pub const SUPPLIES_AT: usize = crate::regs::mem::INTERNAL_SRAM_START;

/// The last raw reading of the cell (VDD / 10) and of VDDIO2 / 10, both
/// taken with the light off, two ticks apart, against the same 1.024 V
/// reference.
///
/// It's a measurement aid and nothing else. The firmware keeps it at
/// [`SUPPLIES_AT`] for `cargo xtask peek`, so one programmer session shows
/// whether VDDIO2, which PC1 hangs off, follows VDD. Nothing in the
/// firmware compares the two or acts on either. Both zero means nothing's
/// been read since the chip started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Supplies {
    pub cell: u16,
    pub vddio2: u16,
}

impl Supplies {
    /// How [`SUPPLIES_AT`] holds it, each reading low byte first, the cell's
    /// first.
    pub const fn to_bytes(self) -> [u8; 4] {
        let [cell_lo, cell_hi] = self.cell.to_le_bytes();
        let [io_lo, io_hi] = self.vddio2.to_le_bytes();
        [cell_lo, cell_hi, io_lo, io_hi]
    }

    pub const fn from_bytes(bytes: [u8; 4]) -> Self {
        let [cell_lo, cell_hi, io_lo, io_hi] = bytes;
        Self {
            cell: u16::from_le_bytes([cell_lo, cell_hi]),
            vddio2: u16::from_le_bytes([io_lo, io_hi]),
        }
    }
}

/// Ticks in a full round of every channel, and so how many apart each
/// channel's readings come.
pub const CYCLE_TICKS: u8 = 16;
/// Where the temperature's turn starts in the round.
const TEMPERATURE_AT: u8 = CYCLE_TICKS / 2;
/// Where ground's turn starts in the round, straight after the
/// temperature's.
const GROUND_AT: u8 = TEMPERATURE_AT + READING_STEP + 1;

const _: () = assert!(GROUND_AT + READING_STEP < CYCLE_TICKS);

/// The most a ground reading may be, in counts of four accumulated 12-bit
/// conversions, for the ADC to count as working.
///
/// A conversion of ground reads its offset error, which table 38-24 of
/// DS40002413B puts at ±5 LSb (2.5 typical) with VDD and the reference both
/// at 3.0 V. A single-ended result can't go under 0, so ground reads 0 to 20
/// over four. The table says nothing for the 1.024 V reference this reads
/// against, and figure 39-112 has the offset growing as the reference falls,
/// so the limit is three times that, 64 counts, 0.4 % of full scale. That's
/// still under a twentieth of the smallest plausible cell reading
/// (`battery::PLAUSIBLE` starts at 1.00 V, about 1,600 counts), so an ADC
/// that hands back the cell's reading, or anything like it, for ground is far
/// over it. Unmeasured.
pub const GROUND_MAX: u16 = 64;

/// How many ground readings in a row over [`GROUND_MAX`] count as an ADC
/// that's stuck, two, a round apart, a quarter of a second.
///
/// One that far off isn't noise, but it's one conversion, and the second
/// costs a quarter of a second. Unmeasured.
pub const GROUND_MISSES: u8 = 2;

/// Whether the ADC still answers for the input it's asked about.
///
/// An ADC whose result register or input mux has stuck hands back the same
/// reading whatever MUXPOS says. If that reading is a plausible cell, the
/// voltage checks can't tell it from a good one, and it hides a cell that's
/// running flat. Ground can tell, since it reads near zero on a working ADC
/// and the cell never does. So [`GROUND_MISSES`] ground readings in a row
/// over [`GROUND_MAX`] are an ADC the firmware can't trust, which the caller
/// treats as a cell it can't read.
///
/// It catches a reading that's stuck, or one that follows the wrong input.
/// It can't catch one that's wrong and still moving, or a reference that's
/// drifted, since ground reads near zero against any reference.
#[derive(Clone, Debug, Default)]
pub struct Ground {
    misses: u8,
}

impl Ground {
    pub const fn new() -> Self {
        Self { misses: 0 }
    }

    /// One ground reading, raw. Returns whether the ADC's stuck. A missing
    /// result reads as zero and passes, since a dead ADC is the voltage's to
    /// catch.
    pub const fn reading(&mut self, raw: u16) -> bool {
        if raw <= GROUND_MAX {
            self.misses = 0;
            return false;
        }
        self.misses = self.misses.saturating_add(1);
        self.misses >= GROUND_MISSES
    }

    /// Nothing measured for a tick, so the count starts again.
    pub const fn rest(&mut self) {
        self.misses = 0;
    }
}
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
    /// Whether VDDIO2's turn is under way this round.
    vddio2: bool,
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
            vddio2: false,
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
                    Channel::Ground => Reading::Ground(raw),
                    Channel::Vddio2 => Reading::Vddio2(raw),
                };
                (Some(reading), None)
            }
            _ => (None, None),
        };
        if channel == Channel::Voltage {
            // VDDIO2's turn, with the light off, decided on the voltage's
            // reading tick and seen through from there.
            let vddio2 = |setup| {
                Some(Convert {
                    channel: Channel::Vddio2,
                    setup,
                })
            };
            if step == READING_STEP && !lit {
                self.vddio2 = true;
                self.set_up = Some(Channel::Vddio2);
                return (reading.or(quick), vddio2(true));
            }
            if self.vddio2 {
                return match step {
                    3 => (quick, vddio2(false)),
                    _ => {
                        self.vddio2 = false;
                        (Some(Reading::Vddio2(raw)), None)
                    }
                };
            }
        }
        // The voltage's turn carries on while the light's on, if the ADC is
        // still set up for the voltage, so there's no setup and nothing to
        // throw away.
        if channel == Channel::Voltage
            && step >= READING_STEP
            && lit
            && self.set_up == Some(Channel::Voltage)
        {
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

    /// A round with the light off, the voltage, VDDIO2 straight after it,
    /// then ground, as a press reads it.
    fn dark_round() -> [(Option<Reading>, Option<Convert>); 16] {
        let mut expected = [(None, None); 16];
        expected[0].1 = convert(Channel::Voltage, true);
        expected[1].1 = convert(Channel::Voltage, false);
        expected[2] = (Some(Reading::Voltage(7)), convert(Channel::Vddio2, true));
        expected[3].1 = convert(Channel::Vddio2, false);
        expected[4].0 = Some(Reading::Vddio2(7));
        expected[11].1 = convert(Channel::Ground, true);
        expected[12].1 = convert(Channel::Ground, false);
        expected[13].0 = Some(Reading::Ground(7));
        expected
    }

    #[test]
    fn dark_it_reads_the_voltage_then_vddio2_then_ground() {
        let mut schedule = Schedule::new();
        assert_eq!(run(&mut schedule, 16, Some(7), false), dark_round());
    }

    /// A check of the cell at rest is the first five ticks of a round and a
    /// rest, the cell and VDDIO2 with no ground.
    #[test]
    fn a_check_at_rest_reads_the_cell_and_vddio2_and_no_ground() {
        let mut schedule = Schedule::new();
        for _ in 0..10 {
            let ticks = run(&mut schedule, 5, Some(7), false);
            assert_eq!(ticks[..], dark_round()[..5]);
            assert_eq!(ticks[4].1, None, "nothing running, so it rests");
            schedule.rest();
        }
    }

    /// VDDIO2 reads against the voltage's reference, and its conversion
    /// starts two ticks after the cell's, so the two compare.
    #[test]
    fn vddio2_is_read_two_ticks_after_the_cell_and_never_while_lit() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 64, Some(7), false);
        let started = |channel| {
            ticks
                .iter()
                .enumerate()
                .filter(|(_, (_, c))| c.is_some_and(|c| c.channel == channel))
                .map(|(tick, _)| tick)
                .collect::<Vec<_>>()
        };
        for round in 0..4 {
            let at = 16 * round;
            assert!(started(Channel::Voltage).contains(&(at + 1)));
            assert!(started(Channel::Vddio2).contains(&(at + 3)));
        }
        let mut schedule = Schedule::new();
        for (reading, convert) in run(&mut schedule, 160, Some(7), true) {
            assert!(!matches!(reading, Some(Reading::Vddio2(_))));
            assert!(convert.is_none_or(|c| c.channel != Channel::Vddio2));
        }
    }

    #[test]
    fn ground_reads_against_the_voltages_reference_and_is_set_up_every_time() {
        for lit in [false, true] {
            let mut schedule = Schedule::new();
            let ticks = run(&mut schedule, 64, Some(7), lit);
            for round in ticks.chunks(16) {
                assert_eq!(round[11].1, convert(Channel::Ground, true), "lit: {lit}");
                // And the voltage after it.
                assert_eq!(round[0].1, convert(Channel::Voltage, true), "lit: {lit}");
            }
        }
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
            // VDDIO2's turn is decided on tick 2, so it isn't in the rest of
            // this round.
            let rest = run(&mut schedule, 15 - out_at, Some(7), false);
            let mut expected = dark_round();
            expected[4] = (None, None);
            assert_eq!(rest[..], expected[out_at + 1..], "{out_at}");
        }
    }

    /// Off, tick 2 always starts VDDIO2's turn, so a light that comes on
    /// after that waits for the next round's voltage turn for quick
    /// readings. VDDIO2's turn finishes first, light or not.
    #[test]
    fn a_light_that_comes_on_partway_through_the_round_starts_quick_readings_next_round() {
        for on_at in 3..16 {
            let mut schedule = Schedule::new();
            run(&mut schedule, on_at, Some(7), false);
            let lit = run(&mut schedule, 16 - on_at, Some(7), true);
            assert!(
                lit.iter()
                    .all(|(r, c)| !matches!(r, Some(Reading::Quick(_)))
                        && c.is_none_or(|c| c.channel != Channel::Voltage)),
                "on at {on_at}"
            );
            if on_at <= 4 {
                assert_eq!(lit[4 - on_at].0, Some(Reading::Vddio2(7)), "on at {on_at}");
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
    fn sets_the_adc_up_at_the_start_of_every_turn() {
        let mut schedule = Schedule::new();
        let setups = |ticks: &[(Option<Reading>, Option<Convert>)]| -> Vec<Channel> {
            ticks
                .iter()
                .filter_map(|(_, convert)| convert.filter(|c| c.setup).map(|c| c.channel))
                .collect()
        };
        // Off, the voltage, VDDIO2 and ground, each set up every round.
        let off = run(&mut schedule, 64, Some(1), false);
        let round = [Channel::Voltage, Channel::Vddio2, Channel::Ground];
        assert_eq!(setups(&off), round.repeat(4));
        // On, the temperature between them.
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
            // A result on every tick except the three turns' due ones.
            let result = (![2, 10, 13].contains(&phase)).then_some(0x155);
            let (reading, _) = schedule.tick(result, true);
            readings.push((phase, reading));
        }
        for (phase, reading) in readings {
            match phase {
                2 => assert_eq!(reading, Some(Reading::Voltage(0))),
                // Asked for, a conversion a tick while the light's on.
                3..=8 => assert_eq!(reading, Some(Reading::Quick(0x155))),
                10 => assert_eq!(reading, Some(Reading::Temperature(0))),
                13 => assert_eq!(reading, Some(Reading::Ground(0))),
                _ => assert_eq!(reading, None, "phase {phase}"),
            }
        }
        // And with the light off, nothing but the voltage, VDDIO2 and ground
        // is asked for, so every other tick's result is ignored, the
        // temperature's due tick included.
        let mut schedule = Schedule::new();
        for tick in 0..160 {
            let (reading, convert) = schedule.tick(Some(0x155), false);
            match tick % 16 {
                2 => assert_eq!(reading, Some(Reading::Voltage(0x155))),
                4 => assert_eq!(reading, Some(Reading::Vddio2(0x155))),
                13 => assert_eq!(reading, Some(Reading::Ground(0x155))),
                phase => assert_eq!(reading, None, "phase {phase}"),
            }
            if (4..11).contains(&(tick % 16)) || tick % 16 >= 13 {
                assert_eq!(convert, None, "tick {tick}");
            }
        }
    }

    #[test]
    fn supplies_go_to_bytes_low_byte_first_the_cell_first_and_back() {
        let supplies = Supplies {
            cell: 0x1234,
            vddio2: 0x0ABC,
        };
        assert_eq!(supplies.to_bytes(), [0x34, 0x12, 0xBC, 0x0A]);
        assert_eq!(Supplies::from_bytes(supplies.to_bytes()), supplies);
        assert_eq!(Supplies::default().to_bytes(), [0; 4]);
        assert_eq!(SUPPLIES_AT, 0x7000);
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
        // Just over, the cell's own reading, full scale and more.
        for raw in [GROUND_MAX + 1, 6140, 4 * 4095, u16::MAX] {
            let mut ground = Ground::new();
            assert!(!ground.reading(raw), "{raw}: one");
            assert!(ground.reading(raw), "{raw}: two");
        }
    }

    #[test]
    fn a_good_ground_reading_or_a_rest_starts_the_count_again() {
        let mut ground = Ground::new();
        for _ in 0..100 {
            assert!(!ground.reading(6140));
            assert!(!ground.reading(GROUND_MAX));
        }
        for _ in 0..100 {
            assert!(!ground.reading(6140));
            ground.rest();
        }
        // And a missing result, which reads as zero, passes.
        assert!(!ground.reading(6140));
        assert!(!ground.reading(0));
    }

    /// The smallest plausible cell reading is far over ground's limit, so an
    /// ADC that answers ground with anything a cell could read is caught.
    #[test]
    fn every_plausible_cell_reading_is_far_over_grounds_limit() {
        let smallest = (0..=4 * 4095)
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
