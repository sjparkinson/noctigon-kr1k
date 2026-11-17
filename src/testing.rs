//! Test doubles shared by the library's own tests and xtask's simulators.
//!
//! So there's one fake ADC, one torch wired to it, one way of turning a
//! voltage or a temperature into a raw reading, and one list of what the
//! torch does ([`EXAMPLES`]).
//!
//! The firmware never sees it.
//!
//! # Scripts
//!
//! A script is Anduril's notation, a word at a time, like `1C 2H:1.5s wait:1s`.
//! `NC` is N quick clicks. `NH` is N - 1 clicks then a hold (so `2H` is
//! click, hold), for a second unless it says otherwise (`1H:500ms`).
//! `wait:2s` leaves the switch alone. Every click or hold is followed by
//! enough of a pause for it to count, so `1C 1C` is two single clicks,
//! except a hold that's the script's last word. That one's never let go, so
//! the script ends with the switch still down.
//!
//! Some words set the scene, taking no time. `cell:2.95V` is the cell from
//! there on (3.8 V to start), and `heat:80C` the temperature (25 °C to
//! start). A cell outside `battery::PLAUSIBLE`, such as `cell:0V`, is one
//! the firmware can't read, and 0 V is what a dead ADC reads. `fault` starts
//! a torch whose power-on checks failed, so it has to come before anything
//! that takes time. So does `flash:corrupt`, a byte of flash that doesn't
//! match its CRC. `vddio2:lost` is MVIO saying VDDIO2, the supply PC1 is on,
//! is missing from there on, and `vddio2:back` that it's there again. Lost
//! before anything that takes time, it's missing at power-on, which is a
//! failed check. `adc:stuck` freezes the ADC on the cell's reading as it
//! stands, for every channel, ground included, from there on.

mod examples;

pub use examples::{EXAMPLES, Example, Expect, example};

use core::fmt;
use core::iter;
use core::ops::Deref;
use core::str::FromStr;

use crate::TICKS_PER_SECOND;
use crate::aw2016::Aw2016;
use crate::battery;
use crate::button::{Event, HOLD_TICKS, RELEASE_TICKS};
use crate::ramp::Output;
use crate::selftest::Faults;
use crate::sensors::{Channel, Convert};
use crate::temp::Calibration;
use crate::thermal;
use crate::torch::Torch;
use crate::watchdog::{End, Pacer, Pit, SLOW_TICKS, Wake};

/// The most four accumulated 12-bit conversions can read.
pub const FULL_SCALE: u16 = 4 * 4095;

/// A temperature sensor calibration for tests and the simulated chip's
/// signature row.
///
/// It's the real one from the backup of this torch's
/// signature row (backup/stock/prodsig.hex, `TEMPSENSE0` 0x03EE and
/// `TEMPSENSE1` 0x0D24), at about a quarter of a kelvin a count, so 25 °C
/// reads about 2151 a conversion, 8603 over four.
pub const CALIBRATION: Calibration = Calibration {
    offset: 0x0D24,
    slope: 0x03EE,
};

/// What four accumulated conversions of the ADC's GND input read here.
///
/// It's 20, the most table 38-24 of DS40002413B allows for the offset error,
/// 5 LSb a conversion. A single-ended result can't go under zero, so the
/// worst a working ADC can be is over it, and the most the table allows is
/// the closest that gets to `sensors::GROUND_MAX` while still being in spec.
pub const GROUND_READING: u16 = 4 * 5;

/// The accumulated reading of VDD / 10 for a cell `battery::centivolts`
/// reports as `cell`, the closest, saturating at four full-scale conversions.
pub fn voltage_reading(cell: u16) -> u16 {
    closest(cell, battery::centivolts)
}

/// The temperature sensor reading, four accumulated 12-bit conversions,
/// closest to `kelvin` with [`CALIBRATION`].
///
/// The reading falls as the
/// temperature rises, so this searches it backwards, and of two readings
/// that are as close, the higher wins.
pub fn temperature_reading(kelvin: u16) -> u16 {
    let backwards = |raw: u16| CALIBRATION.sixteenths(FULL_SCALE - raw);
    FULL_SCALE - closest(kelvin.saturating_mul(16), backwards)
}

/// The smallest raw reading whose `read` is closest to `target`. `read` never
/// goes down as the reading goes up, so a binary search finds it.
fn closest(target: u16, read: impl Fn(u16) -> u16) -> u16 {
    // The first reading that reads as at least `value`, or one past full
    // scale if none does.
    let first_at_least = |value: u16| {
        let (mut low, mut high) = (0, FULL_SCALE + 1);
        while low < high {
            let mid = low + (high - low) / 2;
            if read(mid) < value {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    };
    let above = first_at_least(target);
    if above == 0 {
        return 0;
    }
    let below = read(above - 1);
    if above > FULL_SCALE || target - below <= read(above) - target {
        // Below wins ties, and a run of readings that read the same starts
        // further down.
        first_at_least(below)
    } else {
        above
    }
}

/// An ADC that hands back a conversion's result a tick after it started, the
/// way the firmware's loop sees the real one. A conversion measures what
/// was there when it started.
///
/// `voltage`, `temperature` and `ground` are the raw results [`Adc::start`]
/// reads. Anything else that knows what each channel reads can use
/// [`Adc::start_with`] instead.
#[derive(Clone, Debug)]
pub struct Adc {
    pub voltage: u16,
    pub temperature: u16,
    pub ground: u16,
    pending: Option<u16>,
}

impl Default for Adc {
    fn default() -> Self {
        Self::new()
    }
}

impl Adc {
    /// 3.8 V, 25 °C with [`CALIBRATION`], and ground at
    /// [`GROUND_READING`].
    pub const fn new() -> Self {
        Self {
            voltage: 6140,
            temperature: 8603,
            ground: GROUND_READING,
            pending: None,
        }
    }

    /// Stuck on `raw` for every channel, the way an ADC whose result
    /// register or mux has stuck reads.
    pub const fn stuck(&mut self, raw: u16) {
        (self.voltage, self.temperature, self.ground) = (raw, raw, raw);
    }

    /// The result of the conversion started last tick, if there was one.
    pub const fn result(&mut self) -> Option<u16> {
        self.pending.take()
    }

    /// Starts `convert`, if there is one, reading `voltage` or
    /// `temperature`.
    pub fn start(&mut self, convert: Option<Convert>) {
        let (voltage, temperature, ground) = (self.voltage, self.temperature, self.ground);
        self.start_with(convert, |channel| match channel {
            // VDDIO2 reads as the cell here, as if the two were one rail.
            Channel::Voltage | Channel::Vddio2 => voltage,
            Channel::Temperature => temperature,
            Channel::Ground => ground,
        });
    }

    /// Starts `convert`, if there is one, reading whatever `read` says the
    /// channel reads.
    pub fn start_with(&mut self, convert: Option<Convert>, read: impl FnOnce(Channel) -> u16) {
        self.pending = convert.map(|convert| read(convert.channel));
    }
}

/// How long a scripted click holds the switch down (about 60 ms).
const PRESS_TICKS: u16 = 4;
/// The pause after each scripted click, well inside `RELEASE_TICKS`.
const GAP_TICKS: u16 = 4;
/// Released samples after a click or hold before the next action, enough
/// for `button::Clicks` to send the click (see its `RELEASE_TICKS` check).
const SETTLE_TICKS: u16 = RELEASE_TICKS + 1;
/// A hold with no duration lasts a second.
const DEFAULT_HOLD_TICKS: u16 = TICKS_PER_SECOND;

/// One word of a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// This many quick clicks.
    Clicks(u8),
    /// `clicks` quick clicks, then the switch held down for `ticks`.
    Hold { clicks: u8, ticks: u16 },
    /// The switch left alone for this many ticks.
    Wait(u16),
    /// The cell from now on, in centivolts.
    Cell(u16),
    /// The temperature from now on, in kelvin.
    Heat(u16),
    /// Whether MVIO says VDDIO2 is there from now on.
    Vddio2(bool),
    /// A flash byte that doesn't match the CRC. Power-on only.
    CorruptFlash,
    /// The power-on checks fail. Power-on only.
    Fault,
    /// The ADC sticks on the cell's reading as it is now, for every channel
    /// and for good.
    AdcStuck,
}

impl Action {
    /// Whether it takes time, rather than setting the scene.
    const fn takes_time(self) -> bool {
        matches!(self, Self::Clicks(_) | Self::Hold { .. } | Self::Wait(_))
    }
}

/// Why a word isn't an [`Action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No count of 1 or more before the C or H.
    Count,
    /// Not clicks, a hold or anything else a script knows.
    Kind,
    /// A click with a duration.
    TimedClick,
    /// A hold too short to count as one.
    ShortHold,
    /// A duration it can't read.
    Duration,
    /// A duration that rounds to no ticks.
    UnderATick,
    /// A duration of more ticks than a `u16` holds.
    TooLong,
    Voltage,
    Temperature,
    /// `vddio2:` with anything but `lost` or `back`.
    Supply,
    /// `fault` or `flash:corrupt` after something that takes time.
    NotAtPowerOn,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Count => "expected a count of 1 or more, like 1C, 2H or wait:1s",
            Self::Kind => "expected C (clicks), H (hold) or wait:",
            Self::TimedClick => "clicks don't take a duration; holds do (1H:2s)",
            Self::ShortHold => {
                let ms = u32::from(HOLD_TICKS) * 1000 / u32::from(TICKS_PER_SECOND);
                return write!(f, "that's a click; a hold needs longer than {ms} ms");
            }
            Self::Duration => "expected a duration like 500ms, 2s or 1.5s",
            Self::UnderATick => "that's shorter than a tick",
            Self::TooLong => "that's too long",
            Self::Voltage => "expected a voltage like 2.95V",
            Self::Temperature => "expected a temperature like 80C",
            Self::Supply => "expected vddio2:lost or vddio2:back",
            Self::NotAtPowerOn => {
                "the firmware only checks that at power-on, so it goes before anything \
                 that takes time"
            }
        };
        f.write_str(text)
    }
}

impl FromStr for Action {
    type Err = Error;

    /// `3C`, `2H`, `1H:500ms`, `wait:2s`, `cell:2.95V`, `heat:80C`,
    /// `vddio2:lost`, `vddio2:back`, `fault`, `flash:corrupt` or `adc:stuck`.
    fn from_str(word: &str) -> Result<Self, Error> {
        if word == "fault" {
            return Ok(Self::Fault);
        }
        if word == "flash:corrupt" {
            return Ok(Self::CorruptFlash);
        }
        if word == "adc:stuck" {
            return Ok(Self::AdcStuck);
        }
        if let Some(supply) = word.strip_prefix("vddio2:") {
            return match supply {
                "lost" => Ok(Self::Vddio2(false)),
                "back" => Ok(Self::Vddio2(true)),
                _ => Err(Error::Supply),
            };
        }
        if let Some(volts) = word.strip_prefix("cell:") {
            return centivolts(volts).map(Self::Cell);
        }
        if let Some(celsius) = word.strip_prefix("heat:") {
            let degrees: u8 = celsius
                .strip_suffix('C')
                .and_then(|degrees| degrees.parse().ok())
                .ok_or(Error::Temperature)?;
            return Ok(Self::Heat(thermal::celsius(degrees)));
        }
        if let Some(duration) = word.strip_prefix("wait:") {
            return ticks(duration).map(Self::Wait);
        }
        let (head, duration) = match word.split_once(':') {
            Some((head, duration)) => (head, Some(duration)),
            None => (word, None),
        };
        let (count, kind) = head.split_at(head.len().saturating_sub(1));
        let count: u8 = match count.parse() {
            Ok(n) if n > 0 => n,
            _ => return Err(Error::Count),
        };
        match (kind, duration) {
            ("C" | "c", None) => Ok(Self::Clicks(count)),
            ("C" | "c", Some(_)) => Err(Error::TimedClick),
            ("H" | "h", duration) => {
                let ticks = duration.map_or(Ok(DEFAULT_HOLD_TICKS), ticks)?;
                if ticks <= HOLD_TICKS {
                    return Err(Error::ShortHold);
                }
                Ok(Self::Hold {
                    clicks: count - 1,
                    ticks,
                })
            }
            _ => Err(Error::Kind),
        }
    }
}

/// A word of a script and why it isn't one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptError<'a> {
    pub word: &'a str,
    pub error: Error,
}

impl fmt::Display for ScriptError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}`: {}", self.word, self.error)
    }
}

/// Reads a script, word by word.
pub fn parse<'a>(
    words: impl IntoIterator<Item = &'a str>,
) -> impl Iterator<Item = Result<Action, ScriptError<'a>>> {
    let mut started = false;
    words.into_iter().map(move |word| {
        let at = |error| ScriptError { word, error };
        let action: Action = word.parse().map_err(at)?;
        if matches!(action, Action::Fault | Action::CorruptFlash) && started {
            return Err(at(Error::NotAtPowerOn));
        }
        started |= action.takes_time();
        Ok(action)
    })
}

/// `whole.fraction`, times ten to the `places`, with no more than `places`
/// digits after the point.
fn decimal(text: &str, places: u32) -> Option<u32> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if fraction.len() > places as usize || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut unit = 10u32.pow(places);
    let mut value = whole.parse::<u32>().ok()?.checked_mul(unit)?;
    for digit in fraction.bytes() {
        unit /= 10;
        value = value.checked_add(u32::from(digit - b'0') * unit)?;
    }
    Some(value)
}

/// Centivolts from `2.95V`.
fn centivolts(volts: &str) -> Result<u16, Error> {
    volts
        .strip_suffix('V')
        .and_then(|number| decimal(number, 2))
        .and_then(|cv| u16::try_from(cv).ok())
        .ok_or(Error::Voltage)
}

/// Ticks from `500ms`, `2s` or `1.5s`, rounded to the nearest tick.
fn ticks(duration: &str) -> Result<u16, Error> {
    let ms = if let Some(ms) = duration.strip_suffix("ms") {
        ms.parse().ok()
    } else if let Some(s) = duration.strip_suffix('s') {
        decimal(s, 3)
    } else {
        None
    };
    let ms = ms.ok_or(Error::Duration)?;
    let ticks = (u64::from(ms) * u64::from(TICKS_PER_SECOND) + 500) / 1000;
    match u16::try_from(ticks) {
        Ok(0) => Err(Error::UnderATick),
        Ok(ticks) => Ok(ticks),
        Err(_) => Err(Error::TooLong),
    }
}

/// What the torch sees, as far as a script says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scene {
    /// The cell, in centivolts, as `battery::centivolts` would report it.
    pub cell: u16,
    pub kelvin: u16,
    /// The power-on checks fail.
    pub fault: bool,
    /// A flash byte doesn't match its CRC.
    pub corrupt_flash: bool,
    /// The ADC never finishes a conversion, so the firmware's loop never sees
    /// a result. No script word sets it.
    pub adc_dead: bool,
    /// Whether MVIO says VDDIO2, the supply PC1 is on, is there.
    pub vddio2: bool,
    /// The ADC's stuck on this raw reading for every channel, ground
    /// included, whatever the cell does. `adc:stuck` sets it to the cell's
    /// reading as it stands.
    pub adc_stuck: Option<u16>,
    /// VDDIO2's voltage in centivolts, as `battery::centivolts` would report
    /// its VDDIO2 / 10 reading, or `None` for the same as the cell, a board
    /// with the two tied together. It's what the ADC reads, where
    /// [`Scene::vddio2`] is MVIO's bit. No script word sets it.
    pub vddio2_level: Option<u16>,
}

impl Scene {
    /// A cell at 3.8 V, a room at 25 °C and nothing wrong.
    pub const NORMAL: Self = Self {
        cell: 380,
        kelvin: thermal::celsius(25),
        fault: false,
        corrupt_flash: false,
        adc_dead: false,
        vddio2: true,
        adc_stuck: None,
        vddio2_level: None,
    };

    /// What the power-on checks find, with `fault` standing for a fuse that
    /// fails its read-back.
    pub const fn faults(&self) -> Faults {
        Faults {
            fuses: self.fault,
            flash: self.corrupt_flash,
            supply: !self.vddio2,
            ..Faults::NONE
        }
    }

    /// The raw reading a conversion on `channel` gives.
    pub fn reading(&self, channel: Channel) -> u16 {
        if let Some(raw) = self.adc_stuck {
            return raw;
        }
        match channel {
            Channel::Voltage => voltage_reading(self.cell),
            // With MVIO saying VDDIO2 is missing, it reads 0. DS40002413B
            // doesn't say what VDDIO2 / 10 reads then (sections 19.3.2.2 and
            // 33.5.7), so this is the reading furthest from a rail that's
            // there.
            Channel::Vddio2 if !self.vddio2 => 0,
            Channel::Vddio2 => voltage_reading(self.vddio2_level.unwrap_or(self.cell)),
            Channel::Temperature => temperature_reading(self.kelvin),
            Channel::Ground => GROUND_READING,
        }
    }
}

/// One tick of a script, with the switch and what the torch sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub pressed: bool,
    pub scene: Scene,
}

/// The switch and the scene, one sample per tick, for a whole script.
pub fn samples(actions: impl IntoIterator<Item = Action>) -> impl Iterator<Item = Sample> {
    let mut actions = actions.into_iter().peekable();
    let words = iter::from_fn(move || {
        let action = actions.next()?;
        Some((action, actions.peek().is_none()))
    });
    let mut scene = Scene::NORMAL;
    words.flat_map(move |(action, last)| {
        let (quick, held, released) = match action {
            Action::Clicks(n) => (n, 0, SETTLE_TICKS),
            // The last word's hold is never let go.
            Action::Hold { clicks, ticks } => (clicks, ticks, if last { 0 } else { SETTLE_TICKS }),
            Action::Wait(ticks) => (0, 0, ticks),
            Action::Cell(cell) => {
                scene.cell = cell;
                (0, 0, 0)
            }
            Action::Heat(kelvin) => {
                scene.kelvin = kelvin;
                (0, 0, 0)
            }
            Action::Vddio2(there) => {
                scene.vddio2 = there;
                (0, 0, 0)
            }
            Action::CorruptFlash => {
                scene.corrupt_flash = true;
                (0, 0, 0)
            }
            Action::Fault => {
                scene.fault = true;
                (0, 0, 0)
            }
            Action::AdcStuck => {
                scene.adc_stuck = Some(voltage_reading(scene.cell));
                (0, 0, 0)
            }
        };
        let scene = scene;
        clicks(quick)
            .chain(span(true, held))
            .chain(span(false, released))
            .map(move |pressed| Sample { pressed, scene })
    })
}

/// `n` quick clicks, each a press then a short gap, one sample per tick.
pub fn clicks(n: u8) -> impl Iterator<Item = bool> {
    (0..n).flat_map(|_| span(true, PRESS_TICKS).chain(span(false, GAP_TICKS)))
}

fn span(pressed: bool, ticks: u16) -> impl Iterator<Item = bool> {
    iter::repeat_n(pressed, ticks.into())
}

/// The library's torch wired to an [`Adc`], the way `src/main.rs` wires it to
/// the real one, with the result of last tick's conversion in and this tick's
/// conversion out.
#[derive(Clone, Debug)]
pub struct Rig {
    pub torch: Torch,
    /// What [`Rig::tick`] reads. [`Rig::tick_in`] reads a scene instead.
    pub adc: Adc,
    /// Whether [`Rig::tick`] tells the torch VDDIO2 is there. A scene says
    /// for [`Rig::tick_in`].
    pub vddio2: bool,
    /// Whether both tell the torch the watchdog's lit setting is in force
    /// (`Torch::tick`'s `lit_window`). Always, but for [`Paced`], which
    /// asks its pacer.
    pub lit_window: bool,
}

impl Default for Rig {
    /// A torch whose power-on checks passed, with a good cell at room
    /// temperature.
    fn default() -> Self {
        Self::new(Faults::NONE)
    }
}

impl Rig {
    /// A torch that's just had its cell put in, `faults` being what its
    /// power-on checks found.
    pub const fn new(faults: Faults) -> Self {
        Self {
            torch: Torch::new(CALIBRATION, faults),
            adc: Adc::new(),
            vddio2: true,
            lit_window: true,
        }
    }

    /// One tick, the ADC reading `adc`'s `voltage` and `temperature`, and
    /// MVIO saying what `vddio2` does.
    pub fn tick(&mut self, pressed: bool) -> Option<Event> {
        let event = self
            .torch
            .tick(pressed, self.adc.result(), self.vddio2, self.lit_window);
        self.adc.start(self.torch.convert());
        event
    }

    /// One tick, the ADC and MVIO reading what `scene` says.
    pub fn tick_in(&mut self, pressed: bool, scene: &Scene) -> Option<Event> {
        let event = self
            .torch
            .tick(pressed, self.adc.result(), scene.vddio2, self.lit_window);
        // A dead ADC is started on but never answers, so nothing's pending.
        let convert = self.torch.convert().filter(|_| !scene.adc_dead);
        self.adc
            .start_with(convert, |channel| scene.reading(channel));
        event
    }

    /// Plays a script on a fresh torch, whose power-on checks find what the
    /// first tick's scene says.
    pub fn play(actions: impl IntoIterator<Item = Action>) -> Self {
        Self::play_with(actions, |_, _| {})
    }

    /// [`Rig::play`], calling `each` after every tick with the click or hold
    /// it finished.
    pub fn play_with(
        actions: impl IntoIterator<Item = Action>,
        mut each: impl FnMut(&Self, Option<Event>),
    ) -> Self {
        let mut samples = samples(actions).peekable();
        let power_on = samples.peek().map_or(Scene::NORMAL, |s| s.scene);
        let mut rig = Self::new(power_on.faults());
        for Sample { pressed, scene } in samples {
            let event = rig.tick_in(pressed, &scene);
            each(&rig, event);
        }
        rig
    }
}

impl Deref for Rig {
    type Target = Torch;

    fn deref(&self) -> &Torch {
        &self.torch
    }
}

/// When the PIT's first period after a change of period ends.
///
/// The datasheet only says the first after enabling is anything up to a
/// whole period, from the prescaler's phase (§26.5.2.2), so the simulator
/// runs all three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PitPhase {
    /// On the prescaler's next edge for the new period, so a slow tick lands
    /// on every 64th point of the 64 Hz grid, counted from when the PIT
    /// started, and a fast one on the next.
    #[default]
    Aligned,
    /// On the next point of the 64 Hz grid, slow or fast, as soon as the
    /// model goes.
    Early,
    /// A whole period after it would be aligned, as late as the model goes.
    Late,
}

impl PitPhase {
    /// The grid point the PIT ticks on first, after a change to `pit` during
    /// the pass at grid point `at`.
    pub const fn first(self, at: u32, pit: Pit) -> u32 {
        match (self, pit) {
            (Self::Aligned | Self::Early, Pit::Fast) | (Self::Early, Pit::Slow) => at + 1,
            (Self::Late, Pit::Fast) => at + 2,
            (Self::Aligned, Pit::Slow) => next_slow(at),
            (Self::Late, Pit::Slow) => next_slow(at) + SLOW_GRID,
        }
    }
}

/// Points of the 64 Hz grid in a slow tick.
const SLOW_GRID: u32 = SLOW_TICKS as u32;

/// The first slow tick of the prescaler's own after grid point `at`.
pub const fn next_slow(at: u32) -> u32 {
    (at / SLOW_GRID + 1) * SLOW_GRID
}

/// The firmware's whole loop, src/main.rs's, on the 64 Hz grid of samples.
///
/// That's the [`Rig`], the AW2016's steps and the [`Pacer`], with the PIT
/// ticking as [`PitPhase`] says. Each sample is a point of the grid, and a
/// pass runs on it if the PIT ticks there, or if the switch moved since the
/// last sample with the PIT slow. With the PIT slow most samples run no pass.
#[derive(Clone, Debug)]
pub struct Paced {
    pub rig: Rig,
    pub aux: Aw2016,
    pub pacer: Pacer,
    pub phase: PitPhase,
    /// Every AW2016 update fails, as on a bus with a fault on it.
    pub bus_fails: bool,
    /// The end of the last pass.
    pub end: End,
    /// The grid point the next sample is on, counted from when the PIT
    /// started.
    grid: u32,
    /// The grid point the PIT next ticks on.
    next_tick: u32,
    /// The PIT's period.
    pit: Pit,
    /// The switch at the last sample.
    pressed: bool,
}

impl Paced {
    /// A torch whose power-on checks found `faults`, as `main` starts it,
    /// with the first sample on grid point `grid`, where the PIT ticks.
    pub const fn new(faults: Faults, grid: u32) -> Self {
        let rig = Rig::new(faults);
        Self {
            end: End::start(rig.torch.resting()),
            rig,
            aux: Aw2016::new(),
            pacer: Pacer::new(),
            phase: PitPhase::Aligned,
            bus_fails: false,
            grid,
            next_tick: grid,
            pit: Pit::Fast,
            pressed: false,
        }
    }

    /// What `main` sends to the driver, the torch's output.
    pub fn output(&self) -> Output {
        self.rig.torch.output()
    }

    /// The PIT's period as the last pass left it.
    pub const fn pit(&self) -> Pit {
        self.pit
    }

    /// One sample of the grid, and the pass it ran, if any.
    pub fn sample(&mut self, pressed: bool, scene: &Scene) -> Option<End> {
        let at = self.grid;
        self.grid += 1;
        let moved = pressed != self.pressed;
        self.pressed = pressed;
        let wake = if at == self.next_tick {
            self.next_tick = match self.pit {
                Pit::Fast => at + 1,
                Pit::Slow => next_slow(at),
            };
            Wake::Tick
        } else if moved && self.end.switch_wakes {
            Wake::Switch
        } else {
            return None;
        };
        Some(self.pass(wake, at, scene))
    }

    /// A pass that a switch edge with no switch movement ran, a bounce, on
    /// the last sample's grid point, if the last pass left the switch able
    /// to wake it.
    pub fn bounce(&mut self, scene: &Scene) -> Option<End> {
        self.end
            .switch_wakes
            .then(|| self.pass(Wake::Switch, self.grid - 1, scene))
    }

    fn pass(&mut self, wake: Wake, at: u32, scene: &Scene) -> End {
        let begin = self.pacer.begin(wake);
        // `Torch::rest_for` is only exact with this, which the pacer only
        // asks for after a pass the torch called quiet. Every host test and
        // every simulator run comes through here.
        assert!(
            begin.rested == 0
                || self
                    .rig
                    .torch
                    .quiet_for()
                    .is_some_and(|quiet| quiet >= begin.rested),
            "a slow tick with the torch not quiet for it: {:?}",
            self.rig.torch.quiet_for()
        );
        self.rig.torch.rest_for(begin.rested);
        self.rig.lit_window = self.pacer.lit();
        self.rig.tick_in(self.pressed, scene);
        if self.aux.step(self.rig.torch.aux()).is_some() && self.bus_fails {
            self.aux.failed();
        }
        let quiet = self.rig.torch.can_slow() && self.aux.settled(self.rig.torch.aux());
        let end = self.pacer.end(self.rig.torch.resting(), quiet);
        if let Some(pit) = end.pit {
            self.pit = pit;
            self.next_tick = self.phase.first(at, pit);
        }
        self.end = end;
        end
    }
}

impl Deref for Paced {
    type Target = Torch;

    fn deref(&self) -> &Torch {
        &self.rig.torch
    }
}

/// A small, fixed-seed random number generator (xorshift32), so tests can
/// throw varied readings at the firmware's logic and still fail the same way
/// every time. It's no good for anything else.
#[derive(Clone, Debug)]
pub struct Rng(u32);

impl Rng {
    /// Starts from `seed`. Xorshift never leaves zero, so a seed of zero
    /// becomes 1.
    pub const fn new(seed: u32) -> Self {
        Self(if seed == 0 { 1 } else { seed })
    }

    /// The next 16 bits, from the top of the state, which is the better end.
    pub const fn next_u16(&mut self) -> u16 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 16) as u16
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_rng_doesnt_get_stuck_at_zero() {
        for seed in [0, 1, 0xDEAD_BEEF] {
            let mut rng = Rng::new(seed);
            let values: BTreeSet<u16> = (0..1000).map(|_| rng.next_u16()).collect();
            assert!(values.len() > 900, "seed {seed}: {} values", values.len());
            assert!(values.iter().any(|&value| value > 0xF000));
        }
    }

    fn convert(channel: Channel) -> Option<Convert> {
        Some(Convert {
            channel,
            setup: false,
        })
    }

    #[test]
    fn answers_for_the_channel_a_tick_later() {
        let mut adc = Adc::new();
        assert_eq!(adc.result(), None);
        adc.start(convert(Channel::Temperature));
        assert_eq!(adc.result(), Some(8603));
        assert_eq!(adc.result(), None);
        adc.start(convert(Channel::Voltage));
        assert_eq!(adc.result(), Some(6140));
        adc.start(None);
        assert_eq!(adc.result(), None);
    }

    #[test]
    fn measures_what_was_there_when_it_started() {
        let mut adc = Adc::new();
        adc.start(convert(Channel::Voltage));
        adc.voltage = 7;
        assert_eq!(adc.result(), Some(6140));
    }

    #[test]
    fn the_defaults_are_a_good_cell_at_room_temperature() {
        assert_eq!(battery::centivolts(Adc::new().voltage), 380);
        let room = crate::thermal::celsius(25);
        assert_eq!(temperature_reading(room), 8603);
        assert_eq!(CALIBRATION.sixteenths(8603), room * 16);
    }

    #[test]
    fn a_stuck_adc_is_garbage_to_thermal_regulation() {
        use crate::thermal::plausible;
        for raw in [0, 1, 7, 4095, FULL_SCALE] {
            assert!(!plausible(CALIBRATION.sixteenths(raw)), "{raw}");
        }
        assert!(plausible(CALIBRATION.sixteenths(Adc::new().temperature)));
    }

    #[test]
    fn readings_come_back_as_what_was_asked_for() {
        for cell in [250, 290, 295, 380, 420] {
            assert_eq!(battery::centivolts(voltage_reading(cell)), cell, "{cell}");
        }
        for kelvin in [273, 298, 323, 343, 373] {
            let got = CALIBRATION.sixteenths(temperature_reading(kelvin));
            // A count of the sum is about a sixteenth.
            assert!(got.abs_diff(kelvin * 16) <= 2, "{kelvin} K reads as {got}");
        }
    }

    /// The slow way, with every raw reading tried, the closest winning and the
    /// smallest winning a tie.
    fn linear(target: u16, read: impl Fn(u16) -> u16) -> u16 {
        (0..=FULL_SCALE)
            .min_by_key(|&raw| (read(raw).abs_diff(target), raw))
            .unwrap_or(0)
    }

    #[test]
    fn the_binary_search_finds_what_the_linear_one_does() {
        let top = battery::centivolts(FULL_SCALE);
        for cell in 0..=top.saturating_add(10) {
            assert_eq!(
                voltage_reading(cell),
                linear(cell, battery::centivolts),
                "{cell} cV"
            );
        }
        // Searched from the top down, so the highest reading wins a tie.
        let backwards = |raw| CALIBRATION.sixteenths(FULL_SCALE - raw);
        for kelvin in 0..=CALIBRATION.sixteenths(0) / 16 + 10 {
            assert_eq!(
                temperature_reading(kelvin),
                FULL_SCALE - linear(kelvin * 16, backwards),
                "{kelvin} K"
            );
        }
    }

    fn word(word: &str) -> Result<Action, Error> {
        word.parse()
    }

    #[test]
    fn parses_clicks_holds_and_waits() {
        let script = "1C 3c 1H 2H:500ms wait:1.5s wait:2s".split_whitespace();
        let expected = [
            Action::Clicks(1),
            Action::Clicks(3),
            Action::Hold {
                clicks: 0,
                ticks: 64,
            },
            Action::Hold {
                clicks: 1,
                ticks: 32,
            },
            Action::Wait(96),
            Action::Wait(128),
        ];
        assert!(parse(script).eq(expected.map(Ok)));
    }

    #[test]
    fn parses_the_words_that_set_the_scene() {
        let script = ["cell:2.95V", "heat:80C", "fault", "flash:corrupt"];
        let expected = [
            Action::Cell(295),
            Action::Heat(353),
            Action::Fault,
            Action::CorruptFlash,
        ];
        assert!(parse(script).eq(expected.map(Ok)));
        for bad in ["cell:3", "cell:2.955V", "heat:80", "heat:hotC", "heat:999C"] {
            assert!(word(bad).is_err(), "{bad:?} parsed");
        }
        assert!(word("flash:fine").is_err());
    }

    #[test]
    fn a_fault_only_goes_before_anything_that_takes_time() {
        assert!(parse(["cell:3.1V", "fault", "flash:corrupt", "1C"]).all(|a| a.is_ok()));
        for late in ["fault", "flash:corrupt"] {
            let error = ScriptError {
                word: late,
                error: Error::NotAtPowerOn,
            };
            assert_eq!(parse(["wait:1s", late]).find_map(Result::err), Some(error));
        }
    }

    #[test]
    fn rejects_what_it_cant_read() {
        for bad in [
            "0C",
            "C",
            "1X",
            "",
            "1C:1s",
            "1H:fast",
            "1H:1.2345s",
            "1H:100ms",
            "wait:0ms",
            "wait:2",
            "wait:1.-5s",
            "wait:9999s",
            "256C",
        ] {
            assert!(word(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn rounds_durations_to_the_nearest_tick() {
        assert_eq!(ticks("8ms"), Ok(1));
        assert_eq!(ticks("15ms"), Ok(1));
        assert_eq!(ticks("24ms"), Ok(2));
        assert_eq!(ticks("1s"), Ok(64));
        assert_eq!(ticks("0.25s"), Ok(16));
    }

    #[test]
    fn a_script_that_ends_in_a_hold_ends_held() {
        let played = |script: &'static [&'static str]| {
            samples(parse(script.iter().copied()).map(Result::unwrap))
        };
        let held = |script| played(script).last().map(|sample| sample.pressed);
        assert_eq!(held(&["1H"]), Some(true));
        assert_eq!(held(&["2C", "1H:2s"]), Some(true));
        assert_eq!(held(&["1H", "1C"]), Some(false));
        assert_eq!(held(&["1H", "wait:1s"]), Some(false));
        let ticks = |script| played(script).count();
        assert_eq!(ticks(&["1H"]), usize::from(DEFAULT_HOLD_TICKS));
        assert_eq!(
            ticks(&["1H", "1H"]),
            usize::from(2 * DEFAULT_HOLD_TICKS + SETTLE_TICKS)
        );
    }

    #[test]
    fn a_change_of_scene_takes_no_time_and_lasts() {
        let script = parse(["wait:1s", "cell:2.95V", "wait:1s"]).map(Result::unwrap);
        let cells = samples(script).map(|sample| sample.scene.cell);
        assert!(cells.eq(iter::repeat_n(380, 64).chain(iter::repeat_n(295, 64))));
    }

    /// The torch tests used to read `cell:3.8V` as 0.38 V, dropping the dot.
    #[test]
    fn a_cell_keeps_its_decimal_point() {
        assert_eq!(word("cell:3.8V"), Ok(Action::Cell(380)));
        assert_eq!(word("cell:3.05V"), Ok(Action::Cell(305)));
        assert_eq!(word("cell:4V"), Ok(Action::Cell(400)));
    }

    /// docs/ui.md's rows for a cell the firmware can't read lean on this.
    /// There's no word for a broken reading, only a cell that can't be real.
    #[test]
    fn a_cell_outside_the_plausible_range_reads_as_one_the_firmware_cant_believe() {
        assert_eq!(word("cell:0V"), Ok(Action::Cell(0)));
        // What a dead ADC gives.
        assert_eq!(voltage_reading(0), 0);
        // What the firmware makes of the cell a script sets.
        let reads = |cell: &'static str| {
            let script = parse([cell, "wait:1s"]).map(Result::unwrap);
            let scene = samples(script).last().unwrap().scene;
            battery::centivolts(scene.reading(Channel::Voltage))
        };
        for cell in ["cell:0V", "cell:0.99V", "cell:4.41V", "cell:5V"] {
            assert!(!battery::plausible(reads(cell)), "{cell}");
        }
        for cell in ["cell:1V", "cell:3.8V", "cell:4.4V"] {
            assert!(battery::plausible(reads(cell)), "{cell}");
        }
    }

    /// The torch tests used to start a fresh bench for `fault`, forgetting the
    /// cell and the heat set before it.
    #[test]
    fn a_fault_keeps_the_cell_and_heat_before_it() {
        let script = parse(["cell:2.95V", "heat:80C", "fault", "1C"]).map(Result::unwrap);
        let expected = Scene {
            cell: 295,
            kelvin: thermal::celsius(80),
            fault: true,
            corrupt_flash: false,
            adc_dead: false,
            vddio2: true,
            adc_stuck: None,
            vddio2_level: None,
        };
        let mut samples = samples(script).peekable();
        assert!(samples.peek().is_some());
        assert!(samples.all(|sample| sample.scene == expected));
        assert!(expected.faults().fuses);
    }

    /// The torch tests used to work out `wait:1500ms` as 1500 × 64 in a `u16`,
    /// which overflows, and couldn't read `1.5s` or a lower-case `c` or `h`.
    #[test]
    fn durations_and_lower_case_read_as_simulate_reads_them() {
        assert_eq!(word("wait:1500ms"), Ok(Action::Wait(96)));
        assert_eq!(word("wait:1.5s"), Ok(Action::Wait(96)));
        assert_eq!(word("2c"), Ok(Action::Clicks(2)));
        assert_eq!(
            word("2h:1.5s"),
            Ok(Action::Hold {
                clicks: 1,
                ticks: 96
            })
        );
    }

    #[test]
    fn a_rig_starts_off_and_plays_a_script() {
        assert_eq!(Rig::default().level(), 0);
        let script = parse(["1C"]).map(Result::unwrap);
        assert_eq!(Rig::play(script).level(), crate::ui::ON_LEVEL);
        let mut ticks = 0;
        let mut clicks = 0;
        Rig::play_with(parse(["2C", "wait:1s"]).map(Result::unwrap), |_, event| {
            ticks += 1;
            clicks += usize::from(event == Some(Event::Click(2)));
        });
        assert_eq!(clicks, 1);
        assert_eq!(ticks, 2 * 8 + SETTLE_TICKS + TICKS_PER_SECOND);
    }

    #[test]
    fn a_scripts_fault_is_a_power_on_fault() {
        let rig = Rig::play(parse(["fault", "1C"]).map(Result::unwrap));
        assert_eq!(rig.level(), 0);
        assert_eq!(rig.torch.aux(), crate::ui::Aux::White);
    }
}
