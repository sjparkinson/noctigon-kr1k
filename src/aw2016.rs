//! The AW2016, the I2C LED driver the KR1K's RGB aux hang off, and what to
//! write to it for each [`Aux`]. The facts are its datasheet's (Awinic, V1.3).

use crate::board::{AUX_BLUE, AUX_GREEN, AUX_RED};
use crate::ui::Aux;

/// The 7-bit I2C address.
pub const ADDRESS: u8 = 0x64;

pub const RSTR: u8 = 0x00;
pub const GCR1: u8 = 0x01;
pub const GCR2: u8 = 0x04;
pub const LCTR: u8 = 0x30;
pub const LCFG1: u8 = 0x31;
pub const PWM1: u8 = 0x34;

pub const RESET: u8 = 0x55;
pub const CHIPEN: u8 = 1 << 0;
/// Set so the CHRG pin can't wake the chip and breathe LED1 by itself. What
/// CHRG is wired to on this board is unknown.
pub const CHGDIS: u8 = 1 << 1;
/// The IMAX codes don't go in order (0 is 15 mA, 1 is 30 mA, 2 is 5 mA and 3 is 10 mA).
pub const IMAX_5MA: u8 = 0b10;
pub const CUR_MAX: u8 = 15;
pub const PWM_MAX: u8 = 255;

/// An output's current is IMAX × `cur` / 15 while its PWM is on. The datasheet
/// gives no formula for the duty a `pwm` code gives, so that is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub cur: u8,
    pub pwm: u8,
}

impl Level {
    const OFF: Self = Self { cur: 0, pwm: 0 };
}

/// How dim this looks, and whether a lower code would still show, nobody has seen.
pub const DIM_PWM: u8 = 64;
pub const DIM: Level = Level {
    cur: 1,
    pwm: DIM_PWM,
};
/// Twice the current setting of the others, since a red LED usually gives
/// less light for its current. Unmeasured on these LEDs.
pub const DIM_RED: Level = Level {
    cur: 2,
    pwm: DIM_PWM,
};
pub const BRIGHT_RED: Level = Level {
    cur: CUR_MAX,
    pwm: PWM_MAX,
};

/// One I2C write transfer. The register address goes up one after each byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Write {
    pub register: u8,
    pub data: &'static [u8],
}

pub type Update = &'static [Write];

/// A step of its own, with the settings a tick later. The datasheet gives no
/// wait after CHIPEN rises, PebbleOS's driver found writes in the first
/// millisecond are dropped, and Linux's waits 2 ms.
pub const WAKE: Update = &[Write {
    register: GCR1,
    data: &[CHGDIS | CHIPEN],
}];

/// LCTR goes first, since it only takes a write while awake and keeps its value
/// through standby. Off is standby, as shutdown needs SCL held low for over 130 ms.
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

/// The datasheet's software reset, which leaves standby with every output
/// off. [`STANDBY`] won't do from an unknown state, since a chip already in
/// standby drops its LCTR write.
pub const FROM_UNKNOWN: Update = &[Write {
    register: RSTR,
    data: &[RESET],
}];

const fn put(array: [u8; 3], channel: u8, value: u8) -> [u8; 3] {
    let [a, b, c] = array;
    match channel {
        0 => [value, b, c],
        1 => [a, value, c],
        2 => [a, b, value],
        _ => array,
    }
}

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

const fn enable(level: Level, channel: u8) -> u8 {
    if level.cur > 0 && level.pwm > 0 && channel < 3 {
        1 << channel
    } else {
        0
    }
}

const fn enables(red: Level, green: Level, blue: Level) -> u8 {
    enable(red, AUX_RED) | enable(green, AUX_GREEN) | enable(blue, AUX_BLUE)
}

/// `==` for a `const fn`. A new colour needs an arm here.
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

/// The enables go last, so nothing lights at a setting it isn't meant to have.
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

/// Only for a chip that's awake.
pub const fn show(colour: Aux) -> Update {
    match colour {
        Aux::Off => STANDBY,
        Aux::Blue => show!(Level::OFF, Level::OFF, DIM),
        Aux::Red => show!(BRIGHT_RED, Level::OFF, Level::OFF),
        Aux::Yellow => show!(DIM_RED, DIM, Level::OFF),
        Aux::White => show!(DIM_RED, DIM, DIM),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Unknown,
    Standby,
    Awake,
    Showing(Aux),
    /// Given up on until the colour changes.
    Failed(Aux),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aw2016 {
    state: State,
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

    /// At most one step a tick. Send the transfers before the driver's output
    /// changes, so the aux go dark before the light comes on.
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

    /// Whether [`Aw2016::step`] would send nothing for `want`. Written out
    /// rather than by calling `step`, which is bigger.
    pub const fn settled(&self, want: Aux) -> bool {
        match self.state {
            State::Standby => matches!(want, Aux::Off),
            State::Showing(colour) | State::Failed(colour) => same(colour, want),
            State::Unknown | State::Awake => false,
        }
    }

    /// Gives the last step's colour up. Nothing is retried, and nothing more
    /// goes to the chip until the torch wants another colour.
    pub const fn failed(&mut self) {
        self.state = State::Failed(self.want);
    }

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
    use crate::testing::{EXAMPLES, Rig};

    const COLOURS: [Aux; 5] = [Aux::Off, Aux::Blue, Aux::Red, Aux::Yellow, Aux::White];

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

    #[test]
    fn through_every_example_the_aux_are_dark_while_lit_and_catch_up() {
        for example in EXAMPLES {
            let mut aw = Aw2016::new();
            let mut chip = Chip::new();
            let mut behind = 0;
            Rig::play_with(example.actions(), |rig, _| {
                let want = rig.torch.aux();
                if let Some(update) = aw.step(want) {
                    chip.send(update);
                }
                let lit = chip.lit().0 != [Level::OFF; 3];
                if rig.torch.output().enable {
                    assert!(!lit, "{}: aux lit with the light on", example.name);
                }
                let mut fresh = Chip::new();
                settle(&mut Aw2016::new(), &mut fresh, want);
                if chip.lit() == fresh.lit() || !lit && want == Aux::Off {
                    behind = 0;
                } else {
                    assert!(!lit, "{}: the wrong colour", example.name);
                    behind += 1;
                    assert!(behind <= 2, "{}: {want:?} never shows", example.name);
                }
            });
        }
    }
}
