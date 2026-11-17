//! The AW2016, the I2C LED driver the KR1K's RGB aux hang off, and what to
//! write to it for each [`Aux`].
//!
//! The facts are the AW2016 datasheet's (Awinic, V1.3, October 2023), which
//! V1.1 agrees with. Its 7-bit address is 0x64, fixed, and a write
//! auto-increments the register address after each byte, so one transfer
//! can fill a run of registers. It comes up in standby, with every register
//! at its reset value and every output off. In standby only RSTR and GCR1
//! take a write, and its oscillator is off. GCR1.CHIPEN wakes it, and
//! clearing CHIPEN puts it back in standby, which the datasheet gives as
//! 2 µA typical and 10 µA at most, against 100 µA typical (150 µA at most)
//! awake with every output off. Shutdown is lower, 2 µA at most, but it only
//! comes from holding SCL low for over 130 ms, which the board's pull-ups
//! would fight, so off here is standby.
//!
//! The datasheet gives no wait after CHIPEN rises. A driver for it in
//! PebbleOS says writes in the first millisecond after are acknowledged and
//! dropped, measured rather than from the datasheet, and the Linux driver
//! waits 2 ms. So waking it is a step of its own, and its settings go in a
//! tick later, 15.6 ms on. [`Aw2016`] is that order. It sends nothing else
//! the tick it wakes the chip, the way `torch::Torch` raises PC1 a tick before
//! the DAC.
//!
//! An output's current is `Imax × CUR / 15` (the datasheet's LED current
//! setting), gated by its 8-bit PWM at 250 Hz. The PWM's curve from code to
//! duty is "exponential", and the datasheet gives no formula, so a code's duty
//! is unknown. It's at most the code over 255 if the curve is what the word
//! suggests, and every estimate here takes that ceiling.
//!
//! Which AW2016 output each colour is on comes from the stock firmware (red
//! on output 2, green on 1 and blue on 3, LED1 being output 1), in
//! [`crate::board`]. Blue and red showed right on this torch on 2026-11-16,
//! and green is likely right too, unseen.

use crate::board::{AUX_BLUE, AUX_GREEN, AUX_RED};
use crate::ui::Aux;

/// The AW2016's 7-bit I2C address. It has no address pins.
pub const ADDRESS: u8 = 0x64;

/// RSTR. Reads the chip ID, 0x09, and writing [`RESET`] resets every
/// register.
pub const RSTR: u8 = 0x00;
/// GCR1, with CHIPEN and CHGDIS. It and RSTR are the only registers that
/// take a write in standby.
pub const GCR1: u8 = 0x01;
/// GCR2, with the maximum current IMAX in bits 1:0. The rest of it is the
/// under-voltage and over-temperature protection, which stays as it resets,
/// on.
pub const GCR2: u8 = 0x04;
/// LCTR, with an enable bit per output. Bit 3 (EXP) and bit 5 (FREQ, 250 Hz
/// or 125 Hz) stay as they reset.
pub const LCTR: u8 = 0x30;
/// LCFG1, the first of three configuration registers, one per output, with
/// the current setting CUR in bits 3:0 and manual mode (MD clear). PWM1 to
/// PWM3 follow straight after it at 0x34 to 0x36.
pub const LCFG1: u8 = 0x31;
/// PWM1, the first output's PWM code.
pub const PWM1: u8 = 0x34;

/// What RSTR takes to reset the chip.
pub const RESET: u8 = 0x55;
/// GCR1.CHIPEN, awake when set and in standby when clear.
pub const CHIPEN: u8 = 1 << 0;
/// GCR1.CHGDIS, set to stop the chip's own charging indication.
///
/// With it clear, the CHRG pin going high wakes the chip and breathes LED1 by itself.
/// What CHRG is wired to on this board is unknown, and the datasheet says to
/// set this once the host drives the chip.
pub const CHGDIS: u8 = 1 << 1;
/// GCR2.IMAX at 5 mA, the lowest of the four. The codes don't go in order
/// (0 is 15 mA, 1 is 30 mA, 2 is 5 mA and 3 is 10 mA).
pub const IMAX_5MA: u8 = 0b10;
/// The highest current setting, 15 of 15.
pub const CUR_MAX: u8 = 15;
/// The highest PWM code, on all the time.
pub const PWM_MAX: u8 = 255;

/// How much of each colour's channel a setting asks for, as the AW2016's
/// current setting and PWM code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub cur: u8,
    pub pwm: u8,
}

impl Level {
    const OFF: Self = Self { cur: 0, pwm: 0 };
}

/// Every dim colour's PWM code.
///
/// The lowest global current with PWM is what
/// keeps dim at a fraction of a milliamp, since the lowest setting with no
/// PWM, 5 mA at a current setting of 1 in 15, is already a third of a
/// milliamp a channel. A code of 64 is a quarter of the time at most. How dim
/// that looks, and whether a lower code would still show, nobody has seen.
pub const DIM_PWM: u8 = 64;
/// Dim blue or green, a current setting of 1, which is 0.33 mA while the PWM
/// is on.
pub const DIM: Level = Level {
    cur: 1,
    pwm: DIM_PWM,
};
/// Dim red, at twice the current setting of the others, since a red LED
/// usually gives less light for its current. Unmeasured on these LEDs.
pub const DIM_RED: Level = Level {
    cur: 2,
    pwm: DIM_PWM,
};
/// Bright red, 5 mA with no PWM, the most the 5 mA setting gives.
pub const BRIGHT_RED: Level = Level {
    cur: CUR_MAX,
    pwm: PWM_MAX,
};

/// One I2C write transfer, the register it starts at and the bytes that go
/// in, one register each, auto-incrementing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Write {
    pub register: u8,
    pub data: &'static [u8],
}

/// The transfers for one step, in order.
pub type Update = &'static [Write];

/// Wakes the chip, CHIPEN with the charging indication off. Its outputs stay
/// as LCTR has them, which every way into standby here leaves off.
pub const WAKE: Update = &[Write {
    register: GCR1,
    data: &[CHGDIS | CHIPEN],
}];

/// Every output off, then standby. LCTR goes first, since it only takes a
/// write while the chip's awake and keeps its value through standby, so a
/// later wake can't bring back the old colour.
pub const STANDBY: Update = &[
    Write {
        register: LCTR,
        data: &[0],
    },
    Write {
        register: GCR1,
        data: &[CHGDIS],
    },
];

/// Resets every register, which leaves the chip in standby with every output
/// off (the datasheet's software reset).
///
/// For when nobody knows what state
/// it's in, at power-on or after a transfer that failed, since in standby
/// LCTR won't take the write [`STANDBY`] starts with. CHGDIS goes back to
/// clear, and the next [`WAKE`] sets it.
pub const FROM_UNKNOWN: Update = &[Write {
    register: RSTR,
    data: &[RESET],
}];

/// An array of three with `value` at `channel`, or `array` as it was if
/// `channel` isn't one.
const fn put(array: [u8; 3], channel: u8, value: u8) -> [u8; 3] {
    let [a, b, c] = array;
    match channel {
        0 => [value, b, c],
        1 => [a, value, c],
        2 => [a, b, value],
        _ => array,
    }
}

/// LCFG1 to LCFG3 then PWM1 to PWM3 for a red, green and blue level, each
/// in its own output.
const fn settings(red: Level, green: Level, blue: Level) -> [u8; 6] {
    let cur = put(
        put(put([0; 3], AUX_RED, red.cur), AUX_GREEN, green.cur),
        AUX_BLUE,
        blue.cur,
    );
    let pwm = put(
        put(put([0; 3], AUX_RED, red.pwm), AUX_GREEN, green.pwm),
        AUX_BLUE,
        blue.pwm,
    );
    let [c1, c2, c3] = cur;
    let [p1, p2, p3] = pwm;
    [c1, c2, c3, p1, p2, p3]
}

/// LCTR's enable bit for `channel` if `level` lights it.
const fn enable(level: Level, channel: u8) -> u8 {
    if level.cur > 0 && level.pwm > 0 && channel < 3 {
        1 << channel
    } else {
        0
    }
}

/// LCTR's enable bits for the outputs a level lights.
const fn enables(red: Level, green: Level, blue: Level) -> u8 {
    enable(red, AUX_RED) | enable(green, AUX_GREEN) | enable(blue, AUX_BLUE)
}

/// Whether two colours are the same, in a `const fn`, where `==` can't be.
const fn same(a: Aux, b: Aux) -> bool {
    matches!(
        (a, b),
        (Aux::Off, Aux::Off)
            | (Aux::Blue, Aux::Blue)
            | (Aux::Red, Aux::Red)
            | (Aux::Yellow, Aux::Yellow)
            | (Aux::White, Aux::White)
    )
}

/// The transfers that show a colour on a chip that's awake. The 5 mA setting
/// first, then every output's current and PWM in one transfer, and the
/// enables last, so nothing lights at a setting it isn't meant to have.
macro_rules! show {
    ($red:expr, $green:expr, $blue:expr) => {{
        const SETTINGS: [u8; 6] = settings($red, $green, $blue);
        const ENABLES: [u8; 1] = [enables($red, $green, $blue)];
        const UPDATE: [Write; 3] = [
            Write {
                register: GCR2,
                data: &[IMAX_5MA],
            },
            Write {
                register: LCFG1,
                data: &SETTINGS,
            },
            Write {
                register: LCTR,
                data: &ENABLES,
            },
        ];
        &UPDATE
    }};
}

/// The transfers that show `colour` on a chip that's awake. [`Aux::Off`]
/// is [`STANDBY`], which works awake too.
pub const fn show(colour: Aux) -> Update {
    match colour {
        Aux::Off => STANDBY,
        Aux::Blue => show!(Level::OFF, Level::OFF, DIM),
        Aux::Red => show!(BRIGHT_RED, Level::OFF, Level::OFF),
        Aux::Yellow => show!(DIM_RED, DIM, Level::OFF),
        Aux::White => show!(DIM_RED, DIM, DIM),
    }
}

/// What the firmware last left the chip doing, as far as it knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Nobody knows, which is how it starts.
    Unknown,
    /// In standby with every output off.
    Standby,
    /// Awake, with every output off, waiting a tick for its oscillator.
    Awake,
    /// Showing this colour.
    Showing(Aux),
    /// A transfer for this colour failed, so it's given up on until the
    /// colour changes, and nobody knows what the chip's doing.
    Failed(Aux),
}

/// Steps the AW2016 towards the colour the torch wants, at most one step a
/// tick.
///
/// From a state nobody knows, it resets the chip first. To show a colour from
/// standby it wakes it, and the colour goes in on a later tick. For
/// [`Aux::Off`] it puts it in standby. Once it's showing what's wanted it
/// sends nothing.
///
/// A step that fails gives that colour up. Nothing more goes to the chip
/// until the torch wants something else, and then it starts from a reset,
/// since nobody knows how far the failed step got. So a chip that never
/// answers costs one try for each change of colour, never one a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aw2016 {
    state: State,
    /// What the last step was for.
    want: Aux,
}

impl Default for Aw2016 {
    fn default() -> Self {
        Self::new()
    }
}

impl Aw2016 {
    pub const fn new() -> Self {
        Self {
            state: State::Unknown,
            want: Aux::Off,
        }
    }

    /// The transfers to send this tick to get towards `want`, if there are
    /// any. Send them before the driver's output changes, so the aux go dark
    /// before the light comes on, and only light a tick after it's gone out.
    pub const fn step(&mut self, want: Aux) -> Option<Update> {
        self.want = want;
        let (update, next) = match (self.state, want) {
            (State::Failed(gave_up), _) if same(gave_up, want) => return None,
            (State::Showing(shown), _) if same(shown, want) => return None,
            (State::Standby, Aux::Off) => return None,
            (State::Unknown | State::Failed(_), _) => (FROM_UNKNOWN, State::Standby),
            (State::Awake | State::Showing(_), Aux::Off) => (STANDBY, State::Standby),
            (State::Standby, _) => (WAKE, State::Awake),
            (State::Awake | State::Showing(_), colour) => (show(colour), State::Showing(colour)),
        };
        self.state = next;
        Some(update)
    }

    /// Whether [`Aw2016::step`] would send nothing for `want`, so the aux
    /// need no more ticks. That's showing it already, standby for
    /// [`Aux::Off`], or having given up on it after a failed step, which
    /// [`Aw2016::shown`] alone wouldn't count. Written out rather than by
    /// calling `step`, which is bigger, and a test holds the two to each
    /// other in every state.
    pub const fn settled(&self, want: Aux) -> bool {
        match self.state {
            State::Standby => matches!(want, Aux::Off),
            State::Showing(colour) | State::Failed(colour) => same(colour, want),
            State::Unknown | State::Awake => false,
        }
    }

    /// The last step's transfers didn't all go through, so its colour is
    /// given up on.
    pub const fn failed(&mut self) {
        self.state = State::Failed(self.want);
    }

    /// The colour the chip's showing, if it's settled on one, with
    /// [`Aux::Off`] for standby.
    pub const fn shown(&self) -> Option<Aux> {
        match self.state {
            State::Standby => Some(Aux::Off),
            State::Showing(colour) => Some(colour),
            State::Unknown | State::Awake | State::Failed(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rig, examples};

    const UI_MD: &str = include_str!("../docs/ui.md");
    const COLOURS: [Aux; 5] = [Aux::Off, Aux::Blue, Aux::Red, Aux::Yellow, Aux::White];

    /// A fake AW2016's registers, from 0x00 to 0x3F, written as the
    /// datasheet says, refusing what it says a chip in standby drops.
    struct Chip {
        regs: [u8; 0x40],
    }

    impl Chip {
        fn new() -> Self {
            Self { regs: [0; 0x40] }
        }

        fn awake(&self) -> bool {
            self.regs[usize::from(GCR1)] & CHIPEN != 0
        }

        fn send(&mut self, update: Update) {
            for write in update {
                for (offset, &byte) in write.data.iter().enumerate() {
                    let at = usize::from(write.register) + offset;
                    assert!(
                        self.awake() || at == usize::from(RSTR) || at == usize::from(GCR1),
                        "writes {at:#04x} in standby, which drops it"
                    );
                    if at == usize::from(RSTR) {
                        assert_eq!(byte, RESET);
                        self.regs = [0; 0x40];
                    } else {
                        self.regs[at] = byte;
                    }
                }
            }
        }

        /// The red, green and blue levels the outputs are at, and the IMAX
        /// code, with an output that's off or disabled at [`Level::OFF`].
        fn lit(&self) -> ([Level; 3], u8) {
            let level = |channel: u8| {
                let n = usize::from(channel);
                let on = self.awake() && self.regs[usize::from(LCTR)] & 1 << channel != 0;
                let level = Level {
                    cur: self.regs[usize::from(LCFG1) + n] & 0x0F,
                    pwm: self.regs[usize::from(PWM1) + n],
                };
                if on && level.cur > 0 && level.pwm > 0 {
                    level
                } else {
                    Level::OFF
                }
            };
            (
                [level(AUX_RED), level(AUX_GREEN), level(AUX_BLUE)],
                self.regs[usize::from(GCR2)] & 0b11,
            )
        }
    }

    /// Steps `aw` to `want` on `chip` for as many ticks as it takes, and
    /// how many that was.
    fn settle(aw: &mut Aw2016, chip: &mut Chip, want: Aux) -> usize {
        let mut ticks = 0;
        while let Some(update) = aw.step(want) {
            chip.send(update);
            ticks += 1;
            assert!(ticks <= 3, "{want:?} takes more than three steps");
        }
        assert_eq!(aw.shown(), Some(want));
        ticks
    }

    /// `settled` is exactly whether `step` would send nothing, in every state
    /// the driver can be in and for every colour.
    #[test]
    fn settled_is_step_sending_nothing_in_every_state() {
        let colours = COLOURS
            .into_iter()
            .flat_map(|colour| [State::Showing(colour), State::Failed(colour)]);
        let states = [State::Unknown, State::Standby, State::Awake]
            .into_iter()
            .chain(colours);
        for state in states {
            for want in COLOURS {
                let aw = Aw2016 {
                    state,
                    want: Aux::Off,
                };
                let mut next = aw;
                assert_eq!(
                    aw.settled(want),
                    next.step(want).is_none(),
                    "{state:?} wanting {want:?}"
                );
            }
        }
    }

    #[test]
    fn each_colour_lights_its_channels_and_only_those() {
        for colour in COLOURS {
            let mut aw = Aw2016::new();
            let mut chip = Chip::new();
            settle(&mut aw, &mut chip, colour);
            let (rgb, imax) = chip.lit();
            let want = match colour {
                Aux::Off => [Level::OFF; 3],
                Aux::Red => [BRIGHT_RED, Level::OFF, Level::OFF],
                Aux::Blue => [Level::OFF, Level::OFF, DIM],
                Aux::Yellow => [DIM_RED, DIM, Level::OFF],
                Aux::White => [DIM_RED, DIM, DIM],
            };
            assert_eq!(rgb, want, "{colour:?}");
            if colour != Aux::Off {
                assert_eq!(imax, IMAX_5MA, "{colour:?} at the lowest global current");
            }
        }
    }

    #[test]
    fn dim_uses_pwm_at_the_lowest_current_and_only_lockouts_red_is_bright() {
        for colour in COLOURS {
            let mut aw = Aw2016::new();
            let mut chip = Chip::new();
            settle(&mut aw, &mut chip, colour);
            let (rgb, imax) = chip.lit();
            for level in rgb.iter().filter(|level| **level != Level::OFF) {
                assert_eq!(imax, IMAX_5MA);
                if colour.bright() {
                    assert_eq!(*level, BRIGHT_RED);
                } else {
                    assert!(level.pwm < PWM_MAX && level.cur <= 2, "{colour:?}");
                }
            }
        }
    }

    #[test]
    fn the_channel_map_is_the_stock_firmwares() {
        assert_eq!((AUX_RED, AUX_GREEN, AUX_BLUE), (1, 0, 2));
        // Blue is LED3, so its output and settings are the third of each.
        let blue = show(Aux::Blue);
        assert_eq!(blue[1].data, &[0, 0, 1, 0, 0, DIM_PWM]);
        assert_eq!(blue[2].data, &[1 << 2]);
    }

    #[test]
    fn from_power_on_it_resets_then_wakes_then_shows() {
        let mut aw = Aw2016::new();
        assert_eq!(aw.step(Aux::Blue), Some(FROM_UNKNOWN));
        assert_eq!(aw.step(Aux::Blue), Some(WAKE));
        assert_eq!(aw.step(Aux::Blue), Some(show(Aux::Blue)));
        assert_eq!(aw.step(Aux::Blue), None);
        assert_eq!(aw.shown(), Some(Aux::Blue));
    }

    #[test]
    fn off_is_standby_and_sends_nothing_once_its_there() {
        let mut aw = Aw2016::new();
        let mut chip = Chip::new();
        assert_eq!(settle(&mut aw, &mut chip, Aux::Off), 1);
        assert!(!chip.awake());
        settle(&mut aw, &mut chip, Aux::Red);
        assert_eq!(aw.step(Aux::Off), Some(STANDBY));
        chip.send(STANDBY);
        assert!(!chip.awake(), "standby");
        assert_eq!(chip.regs[usize::from(LCTR)], 0, "every output off");
        assert_eq!(aw.step(Aux::Off), None);
    }

    #[test]
    fn a_change_of_colour_while_awake_goes_straight_in() {
        let mut aw = Aw2016::new();
        let mut chip = Chip::new();
        settle(&mut aw, &mut chip, Aux::Blue);
        assert_eq!(settle(&mut aw, &mut chip, Aux::Red), 1);
        assert_eq!(chip.lit().0, [BRIGHT_RED, Level::OFF, Level::OFF]);
    }

    #[test]
    fn a_wake_never_carries_a_colour_and_one_waiting_follows_the_new_want() {
        let mut aw = Aw2016::new();
        let mut chip = Chip::new();
        settle(&mut aw, &mut chip, Aux::Off);
        assert_eq!(aw.step(Aux::Blue), Some(WAKE));
        chip.send(WAKE);
        assert_eq!(chip.lit().0, [Level::OFF; 3], "dark the tick it wakes");
        // The torch changes its mind while the chip's waking.
        assert_eq!(aw.step(Aux::Red), Some(show(Aux::Red)));
        let mut aw2 = Aw2016::new();
        settle(&mut aw2, &mut Chip::new(), Aux::Off);
        aw2.step(Aux::Blue);
        assert_eq!(aw2.step(Aux::Off), Some(STANDBY), "or back to off");
    }

    #[test]
    fn every_way_from_any_colour_to_any_other_settles_within_three_ticks() {
        for from in COLOURS {
            for to in COLOURS {
                let mut aw = Aw2016::new();
                let mut chip = Chip::new();
                settle(&mut aw, &mut chip, from);
                let ticks = settle(&mut aw, &mut chip, to);
                let shown = Chip::lit(&chip).0;
                let mut fresh = Chip::new();
                settle(&mut Aw2016::new(), &mut fresh, to);
                assert_eq!(shown, fresh.lit().0, "{from:?} to {to:?}");
                assert!(ticks <= 2, "{from:?} to {to:?} took {ticks}");
            }
        }
    }

    #[test]
    fn a_failed_step_gives_up_until_the_colour_changes_then_resets() {
        let mut aw = Aw2016::new();
        assert!(aw.step(Aux::Blue).is_some());
        aw.failed();
        for _ in 0..1000 {
            assert_eq!(aw.step(Aux::Blue), None, "never tries again at 64 Hz");
        }
        assert_eq!(aw.shown(), None);
        assert_eq!(aw.step(Aux::Off), Some(FROM_UNKNOWN));
        assert_eq!(aw.step(Aux::Off), None);
        // One failing mid-wake gives up too.
        aw.step(Aux::Red);
        aw.failed();
        assert_eq!(aw.step(Aux::Red), None);
        assert_eq!(aw.step(Aux::Yellow), Some(FROM_UNKNOWN));
    }

    #[test]
    fn standby_and_reset_are_the_only_writes_a_chip_in_standby_gets() {
        for update in [WAKE, FROM_UNKNOWN] {
            for write in update {
                assert!(write.register == RSTR || write.register == GCR1);
                assert_eq!(write.data.len(), 1);
            }
        }
        assert_eq!(STANDBY.last().map(|w| w.register), Some(GCR1));
        assert_eq!(STANDBY.last().map(|w| w.data), Some(&[CHGDIS][..]));
    }

    #[test]
    fn every_transfer_stays_inside_the_registers_the_datasheet_lists() {
        let listed = |at: usize| matches!(at, 0x00 | 0x01 | 0x04 | 0x30..=0x36);
        for colour in COLOURS {
            for update in [show(colour), WAKE, STANDBY, FROM_UNKNOWN] {
                for write in update {
                    for offset in 0..write.data.len() {
                        let at = usize::from(write.register) + offset;
                        assert!(listed(at), "{colour:?} writes {at:#04x}");
                    }
                }
            }
        }
    }

    /// Every docs/ui.md example, ticked through a torch and this, with the
    /// transfers sent to a fake chip each tick before the output changes, as
    /// the firmware does. The aux are dark whenever the driver's on, and
    /// whatever shows matches what the torch wants within two ticks of it
    /// changing.
    #[test]
    fn through_every_ui_md_example_the_aux_are_dark_while_lit_and_catch_up() {
        for example in examples(UI_MD) {
            let example = example.unwrap();
            let mut rig = Rig::default();
            let mut aw = Aw2016::new();
            let mut chip = Chip::new();
            let mut behind = 0;
            for sample in crate::testing::samples(example.actions()) {
                rig.tick_in(sample.pressed, &sample.scene);
                let want = rig.torch.aux();
                if let Some(update) = aw.step(want) {
                    chip.send(update);
                }
                let lit = chip.lit().0 != [Level::OFF; 3];
                if rig.torch.output().enable {
                    assert!(!lit, "{}: aux lit with the light on", example.row);
                }
                let mut fresh = Chip::new();
                settle(&mut Aw2016::new(), &mut fresh, want);
                if chip.lit() == fresh.lit() || !lit && want == Aux::Off {
                    behind = 0;
                } else {
                    assert!(!lit, "{}: the wrong colour", example.row);
                    behind += 1;
                    assert!(behind <= 2, "{}: {want:?} never shows", example.row);
                }
            }
        }
    }
}
