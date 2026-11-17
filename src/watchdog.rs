//! When to feed the watchdog.
//!
//! The firmware runs the WDT in window mode, as AN2632 §3.2 recommends. A
//! `wdr` in the closed window after the last one resets the chip, and so does
//! no `wdr` by the end of the open window that follows it. So a loop that's
//! stuck fails, and so does a jump into the wrong code that happens to land
//! on the feed.
//!
//! The WDT and the 64 Hz tick both run off the ultra-low-power oscillator, so
//! a tick is always 16 WDT clocks however far that oscillator drifts, and the
//! window can be sized in ticks. Feeding every tick would land on a window
//! edge whatever the setting, so the loop feeds every third tick. That's 48
//! clocks, halfway through a 32-clock open window that starts 32 clocks after
//! the last feed.
//!
//! Window mode only starts with the first `wdr` (DS40002413B §22.3.3.2). Until
//! then the WDT most likely times out after the open window alone (the
//! datasheet implies it rather than saying so), so the first pass feeds it
//! straight away, which also lines the windows up with the ticks.
//!
//! That's all there is. It could prove the watchdog at every power-on with
//! three deliberate resets and a marker in `.noinit`. This firmware doesn't,
//! by choice, because an earlier torch went dark straight after a verified
//! flash of a stack like that, and a firmware hang wasn't ruled out. So
//! nobody's seen this watchdog bite on a real chip. T3 in docs/breakout.md is
//! the build that stops feeding it on purpose, and it waits for a spare AVR
//! DD chip, which there isn't yet.
//!
//! The watchdog can't catch its own oscillator stopping, since the tick stops
//! with it. [`TICK_DEADLINE_PERIODS`] is for that, and only with the light on.
//!
//! At rest the tick slows to 1 Hz and the watchdog goes to normal mode with
//! an 8 s timeout ([`Pacer`]), fed every [`FEED_SLOW_TICKS`] seconds. The lit
//! window is back, with a `wdr` behind it, before the light can come on, and
//! the simulator refuses PC1 rising under anything else. Nobody's seen any
//! of that on a real chip either. The errata, DS80001026D, list nothing for
//! the RTC or the WDT (only the device, NVMCTRL, TCD and USART), which isn't
//! the same as there being nothing.

use crate::TICKS_PER_SECOND;

/// Cycles of the WDT's 1.024 kHz clock in one tick.
pub const CLOCKS_PER_TICK: u16 = 1024 / TICKS_PER_SECOND;
/// The closed window after each feed, in WDT clocks. `hw::start_watchdog` sets
/// `WDT.CTRLA.WINDOW` to match.
pub const CLOSED_CLOCKS: u16 = 32;
/// The open window after the closed one, in WDT clocks. `hw::start_watchdog` sets
/// `WDT.CTRLA.PERIOD` to match.
pub const OPEN_CLOCKS: u16 = 32;
/// How many ticks apart the feeds are.
pub const FEED_TICKS: u8 = 3;

// A feed lands strictly inside the open window, and the first one comes
// before the open window alone runs out, since the first tick arrives within
// a tick of `hw::start_watchdog` turning the WDT on. `wdr` takes two to three WDT clocks
// to sync (§22.3.6), the same for every feed, so it shifts the windows without
// changing the gap between feeds.
#[expect(
    clippy::as_conversions,
    reason = "widening u8 to u16 in a const, where From can't be called"
)]
const _: () = {
    let gap = FEED_TICKS as u16 * CLOCKS_PER_TICK;
    assert!(gap > CLOSED_CLOCKS && gap < CLOSED_CLOCKS + OPEN_CLOCKS);
    assert!(CLOCKS_PER_TICK < OPEN_CLOCKS);
};

/// The main clock in hertz.
///
/// It's the internal high-frequency oscillator at 4 MHz with no prescaler,
/// which is the reset value and which `hw::init` writes again. xtask's
/// simulator holds the firmware to the same number.
pub const CLOCK_HZ: u32 = 4_000_000;

/// Main clocks in one period of TCB0, 1 ms.
///
/// TCB0 is the timer `hw::wait_for_tick` counts while the light's on. Its
/// periodic interrupt mode counts from 0 to CCMP and starts again
/// (DS40002413B section 24.3.3.1.1), so `hw::init` writes one less than this
/// to CCMP.
pub const TIMER_PERIOD_CLOCKS: u16 = 4000;

/// How many TCB0 periods `hw::wait_for_tick` waits for a tick with the light
/// on before it gives up, and `main` turns the light off for good
/// (`hw::halt`).
///
/// The tick and the watchdog both run off the 32 kHz oscillator. If that
/// stopped with the light on, the loop would wait for a tick that never came
/// and the watchdog would never bite, so the driver would stay on with
/// nothing watching the cell or the temperature. While the light's on, TCB0
/// runs off the main clock, the other oscillator, and flags each period. So
/// the wait counts those flags.
///
/// The first period it counts can be part of one, so the deadline is 103 to
/// 104 ms, over six ticks.
///
/// A tick that's only late is the watchdog's to catch. It bites 64 WDT clocks
/// after a feed has synced, which takes up to three more, so 67 in all, a
/// little over four ticks however slow the oscillator runs. DS40002413B gives
/// no accuracy for the 32 kHz oscillator, so this keeps the margin the TS26
/// had from the ATtiny1616's figure of 30 % slow (DS40002204A table 36-13),
/// which is a guess for this chip. The main oscillator is at worst 5 % fast
/// (DS40002413B figure 38-2, over its whole voltage and temperature range).
/// Then those 67 clocks take about 93.5 ms and the deadline at least about
/// 98, so with both oscillators inside those limits the watchdog gets there
/// first, and this is left for an oscillator that's stopped. Nobody's seen
/// either on a real chip.
pub const TICK_DEADLINE_PERIODS: u16 = 104;

#[expect(
    clippy::as_conversions,
    reason = "widening to u32 in a const, where From can't be called"
)]
const _: () = {
    let period = TIMER_PERIOD_CLOCKS as u32;
    let tick = CLOCK_HZ / TICKS_PER_SECOND as u32;
    // The shortest the deadline can be, a partial period and the rest whole,
    // and the longest.
    let shortest = (TICK_DEADLINE_PERIODS as u32 - 1) * period;
    let longest = TICK_DEADLINE_PERIODS as u32 * period;
    // About a tenth of a second.
    assert!(shortest > 6 * tick && longest < 7 * tick);
    // The slowest tick inside those limits, with 5 % more main clocks in the
    // 1 / 0.7 of the time the 32 kHz oscillator takes.
    let slowest = tick * 105 / 70;
    // The longest the watchdog takes to bite, in WDT clocks from the `wdr`.
    // That's three to sync it, then both windows. The deadline counts from the
    // wait after a tick, which is never before the last feed.
    let bite = (3 + CLOSED_CLOCKS + OPEN_CLOCKS) as u32;
    assert!(shortest * CLOCKS_PER_TICK as u32 > bite * slowest);
};

/// Counts ticks between feeds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Feeder {
    /// Ticks until the next feed.
    wait: u8,
    /// Whether the last tick fed it.
    fed: bool,
}

impl Feeder {
    pub const fn new() -> Self {
        Self {
            wait: 0,
            fed: false,
        }
    }

    /// A feeder whose last tick fed the watchdog, for the pass that puts
    /// the lit setting back after a rest ([`Pacer`]), which feeds it itself.
    pub const fn just_fed() -> Self {
        Self {
            wait: FEED_TICKS - 1,
            fed: true,
        }
    }

    /// Call once a tick. True when this pass feeds the watchdog, which is the
    /// first one and then every [`FEED_TICKS`]th.
    pub const fn tick(&mut self) -> bool {
        self.fed = match self.wait.checked_sub(1) {
            None => {
                self.wait = FEED_TICKS - 1;
                true
            }
            Some(wait) => {
                self.wait = wait;
                false
            }
        };
        self.fed
    }

    /// Whether the last [`tick`](Self::tick) fed the watchdog.
    pub const fn fed(&self) -> bool {
        self.fed
    }

    /// Whether the sleep after this pass can be power-down, `resting` being
    /// whether the torch has anything to do. Not straight after a feed, because
    /// a `wdr` takes two or three WDT clocks to sync (§22.3.6), and the
    /// datasheet doesn't say it gets there with the peripheral clock stopped.
    /// If it didn't, the watchdog would reset a resting torch. That costs an
    /// idle tick in [`FEED_TICKS`] while resting.
    pub const fn power_down(&self, resting: bool) -> bool {
        resting && !self.fed
    }
}

/// Ticks one slow tick stands for. `hw::set_pit` slows the PIT from 512
/// cycles of the 32 kHz oscillator to 32,768, 1 Hz.
pub const SLOW_TICKS: u16 = TICKS_PER_SECOND;

/// Quiet passes in a row, a second's worth, before the tick slows down, so
/// a torch that's just gone out, or just been clicked, stays quick for a
/// moment.
pub const SETTLE_TICKS: u8 = 64;

/// Slow ticks between feeds at rest.
pub const FEED_SLOW_TICKS: u8 = 4;

/// The watchdog's timeout at rest, in WDT clocks, normal mode with no window.
/// `hw::set_watchdog` sets `WDT.CTRLA.PERIOD` to match, about 8 s.
pub const REST_CLOCKS: u16 = 8192;

/// The most a pass is allowed to take, in WDT clocks, which is `cargo xtask
/// hex`'s budget of a quarter of a tick.
const PASS_CLOCKS: u16 = CLOCKS_PER_TICK / 4;

/// How long a write to `WDT.CTRLA` is taken to take to sync, in WDT clocks.
/// The datasheet only gives `wdr`'s, two to three (§22.3.6), so this is the
/// simulator's worst case, likely, not stated.
pub const SYNC_CLOCKS: u16 = 3;

/// How far apart two `wdr`s can sync, beyond how far apart they ran, since
/// each takes two to three WDT clocks (§22.3.6).
const SYNC_SPREAD: u16 = 1;

/// Passes after the first one to see a press before the lit setting is back
/// with a `wdr` behind it, at the end of the pass. See [`Pacer`].
pub const EXIT_PASSES: u16 = 2;

#[expect(
    clippy::as_conversions,
    reason = "widening u8 to u16 in a const, where From can't be called"
)]
const _: () = {
    use crate::button::{HOLD_TICKS, RELEASE_TICKS};
    // A press seen on pass P lights nothing before pass P + 1 +
    // RELEASE_TICKS, as a click, or P + HOLD_TICKS, as a hold. The lit
    // setting is back by the end of pass P + EXIT_PASSES.
    assert!(RELEASE_TICKS + 1 > EXIT_PASSES && HOLD_TICKS > EXIT_PASSES);
    // The longest gap between `wdr`s at rest. The last one ends a pass, the
    // pass after it slows the PIT a tick later, and the first slow period
    // can be up to two (the first PIT period after a change isn't stated,
    // §26.5.2.2), then the rest of the slow ticks, then the first fast tick,
    // up to two ticks, and the pass that feeds.
    let slow = SLOW_TICKS * CLOCKS_PER_TICK;
    let rest = CLOCKS_PER_TICK
        + 2 * slow
        + (FEED_SLOW_TICKS as u16 - 1) * slow
        + 2 * CLOCKS_PER_TICK
        + PASS_CLOCKS
        + SYNC_SPREAD;
    assert!(rest < REST_CLOCKS * 2 / 3);
    // Putting the lit setting back. The `wdr` before it ended the pass
    // before, the write starts this one and its `wdr` ends it once the write
    // has synced. If the write doesn't restart the count, which the
    // datasheet doesn't say, that `wdr` has the open window alone to land in,
    // since window mode only starts with it (§22.3.3.2).
    assert!(CLOCKS_PER_TICK + max(PASS_CLOCKS, SYNC_CLOCKS) + SYNC_SPREAD < OPEN_CLOCKS);
    // Going to rest. The write comes a tick after a lit feed, and has to sync
    // before the lit timeout, both windows.
    assert!(
        CLOCKS_PER_TICK + PASS_CLOCKS + SYNC_CLOCKS + SYNC_SPREAD < CLOSED_CLOCKS + OPEN_CLOCKS
    );
};

/// The larger of two, in a const.
const fn max(a: u16, b: u16) -> u16 {
    if a > b { a } else { b }
}

/// What ended the sleep before a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The PIT's tick.
    Tick,
    /// An edge on the switch, which only ends a sleep with the PIT slow.
    Switch,
}

/// The watchdog's two settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// Window mode, [`CLOSED_CLOCKS`] closed and [`OPEN_CLOCKS`] open, fed
    /// every [`FEED_TICKS`]. The only setting PC1 rises under.
    Lit,
    /// Normal mode, [`REST_CLOCKS`], fed every [`FEED_SLOW_TICKS`] slow
    /// ticks.
    Rest,
}

/// The PIT's two periods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pit {
    /// 64 Hz.
    Fast,
    /// 1 Hz, [`SLOW_TICKS`] ticks.
    Slow,
}

/// What a pass does first, before it reads the switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Begin {
    /// Resting ticks the wait just slept through, beyond the pass's own,
    /// for `torch::Torch::rest_for`.
    pub rested: u16,
    /// A setting to write to the watchdog. The pass carries on while it
    /// syncs, and [`End::synced`] waits for it.
    pub watchdog: Option<Setting>,
}

/// What a pass does last, after the output, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct End {
    /// Wait for [`Begin::watchdog`]'s write to sync.
    pub synced: bool,
    /// `wdr`.
    pub feed: bool,
    /// A period to give the PIT.
    pub pit: Option<Pit>,
    /// Sleep in power-down rather than idle.
    pub power_down: bool,
    /// An edge on the switch ends the sleep, as well as the PIT. Only with
    /// the PIT slow.
    pub switch_wakes: bool,
}

impl End {
    /// The sleep before the first pass, which is power-down if the torch
    /// starts out `resting`, as [`Feeder::power_down`] has it.
    pub const fn start(resting: bool) -> Self {
        Self {
            synced: false,
            feed: false,
            pit: None,
            power_down: resting,
            switch_wakes: false,
        }
    }

    const fn fed() -> Self {
        Self {
            synced: false,
            feed: true,
            pit: None,
            power_down: false,
            switch_wakes: false,
        }
    }

    const fn slow() -> Self {
        Self {
            synced: false,
            feed: false,
            pit: Some(Pit::Slow),
            power_down: true,
            switch_wakes: true,
        }
    }
}

/// Where the [`Pacer`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// The first pass after `hw::start_watchdog`, which wrote the lit
    /// setting and the 64 Hz tick, and no `wdr` behind it yet. This pass
    /// feeds, and it's [`State::Fast`] from there.
    First,
    /// The lit setting and the 64 Hz tick, fed by the [`Feeder`], with a
    /// `wdr` since the setting was written. The only state the light can be
    /// on in.
    Fast,
    /// The pass after a lit feed, which writes the rest setting.
    Entering,
    /// The 1 Hz tick at rest, with `since` slow ticks since the last feed.
    Slow { since: u8 },
    /// The PIT's just gone back to 64 Hz with the rest setting, and this pass
    /// feeds.
    Bridge,
    /// Fed last pass with the rest setting. This pass slows the PIT again if
    /// it's still quiet.
    Returning,
    /// This pass writes the lit setting, and feeds once it's synced.
    Restoring,
}

/// Paces the loop, 64 Hz while there's anything to do and 1 Hz at rest.
///
/// That's the 64 Hz tick and the lit watchdog setting whenever there's
/// anything to do, and a 1 Hz tick with a long watchdog timeout at rest, so
/// a torch that's off wakes once a second, not 64 times.
///
/// The torch is quiet when it's [resting](crate::torch::Torch::resting), with
/// nothing timed for more than a slow tick (`torch::Torch::quiet_for`) and
/// the aux settled (`aw2016::Aw2016::settled`). After [`SETTLE_TICKS`] quiet
/// passes in a row it waits for a lit feed, and the pass after that writes
/// [`Setting::Rest`] and slows the PIT. Each slow tick stands for
/// [`SLOW_TICKS`] (`torch::Torch::rest_for`), and an edge on the switch ends
/// a slow sleep too, so the pass that sees a press runs on the press, not up
/// to a second later.
///
/// No `wdr` ever goes before a power-down, so every feed at rest is a short
/// trip back to 64 Hz. Every [`FEED_SLOW_TICKS`] slow ticks, the PIT goes
/// fast with no feed, the first fast tick feeds and idles, and the next slows
/// the PIT again. A pass that isn't quiet, or follows a switch edge, takes the
/// same way back and then writes [`Setting::Lit`] at the start of the next
/// pass and feeds at its end, once the write has synced. That's at most
/// [`EXIT_PASSES`] passes after the press, long before the press can light
/// anything.
///
/// The first PIT period after a change of period could be anything from next
/// to nothing to a whole one, likely on the prescaler's own edge
/// (§26.5.2.2), so a slow tick can stand for a little more or less time than
/// went by. The five-minute check of the cell and `battery::FORGET_TICKS` can
/// drift by up to a second or so for each feed at rest, and nothing counts on
/// either being exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pacer {
    state: State,
    feeder: Feeder,
    /// Quiet passes in a row, up to [`SETTLE_TICKS`].
    settle: u8,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    /// Fast with the lit setting, as `hw::init` and `hw::start_watchdog`
    /// leave the chip, but not [lit](Self::lit) until the first pass has fed
    /// it.
    pub const fn new() -> Self {
        Self {
            state: State::First,
            feeder: Feeder::new(),
            settle: 0,
        }
    }

    /// Whether the lit setting is in force, with a `wdr` since it was
    /// written, so PC1 may rise. `main` hands it to `torch::Torch::tick`,
    /// which keeps the light's level at 0 without it.
    pub const fn lit(&self) -> bool {
        matches!(self.state, State::Fast)
    }

    /// Whether the PIT is slow for the sleep the last pass ended in.
    pub const fn slow(&self) -> bool {
        matches!(self.state, State::Slow { .. })
    }

    /// The start of a pass that `wake` began.
    pub const fn begin(&mut self, wake: Wake) -> Begin {
        if matches!(wake, Wake::Switch) {
            self.settle = 0;
        }
        let rested = match (self.state, wake) {
            (State::Slow { .. }, Wake::Tick) => SLOW_TICKS - 1,
            _ => 0,
        };
        let watchdog = match self.state {
            State::Entering => Some(Setting::Rest),
            State::Restoring => Some(Setting::Lit),
            _ => None,
        };
        Begin { rested, watchdog }
    }

    /// The end of a pass, `resting` being `torch::Torch::resting` and
    /// `quiet` whether the torch has nothing timed for more than a slow tick
    /// and the aux are settled.
    pub const fn end(&mut self, resting: bool, quiet: bool) -> End {
        self.settle = if quiet && self.settle < SETTLE_TICKS {
            self.settle.saturating_add(1)
        } else if quiet {
            SETTLE_TICKS
        } else {
            0
        };
        let settled = self.settle >= SETTLE_TICKS;
        let (next, end) = match self.state {
            State::First | State::Fast => {
                let feed = self.feeder.tick();
                let end = End {
                    synced: false,
                    feed,
                    pit: None,
                    power_down: self.feeder.power_down(resting),
                    switch_wakes: false,
                };
                (
                    if feed && settled {
                        State::Entering
                    } else {
                        State::Fast
                    },
                    end,
                )
            }
            // The rest setting went in at the start of this pass. Still
            // quiet, so slow down. A press since, so feed under it and put
            // the lit setting back.
            State::Entering if settled => (
                State::Slow { since: 0 },
                End {
                    synced: true,
                    ..End::slow()
                },
            ),
            State::Entering => (
                State::Restoring,
                End {
                    synced: true,
                    ..End::fed()
                },
            ),
            State::Slow { since } => {
                let since = since.saturating_add(1);
                if settled && since < FEED_SLOW_TICKS {
                    (
                        State::Slow { since },
                        End {
                            pit: None,
                            ..End::slow()
                        },
                    )
                } else {
                    (
                        State::Bridge,
                        End {
                            synced: false,
                            feed: false,
                            pit: Some(Pit::Fast),
                            power_down: resting,
                            switch_wakes: false,
                        },
                    )
                }
            }
            State::Bridge if settled => (State::Returning, End::fed()),
            State::Returning if settled => (State::Slow { since: 0 }, End::slow()),
            State::Bridge | State::Returning => (State::Restoring, End::fed()),
            State::Restoring => {
                self.feeder = Feeder::just_fed();
                (
                    State::Fast,
                    End {
                        synced: true,
                        ..End::fed()
                    },
                )
            }
        };
        self.state = next;
        end
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn a_tick_is_16_wdt_clocks() {
        assert_eq!(CLOCKS_PER_TICK, 16);
    }

    /// The figures in [`TICK_DEADLINE_PERIODS`]'s comment.
    #[test]
    fn the_tick_deadline_is_103_to_104_ms_against_a_bite_of_93_5_at_worst() {
        let period = u32::from(TIMER_PERIOD_CLOCKS);
        let micros = |clocks: u32| clocks / (CLOCK_HZ / 1_000_000);
        assert_eq!(micros(period), 1000);
        let deadline = u32::from(TICK_DEADLINE_PERIODS);
        assert_eq!(micros((deadline - 1) * period), 103_000);
        assert_eq!(micros(deadline * period), 104_000);
        // 67 WDT clocks of 1 / 1024 s, 30 % slow, in microseconds.
        assert_eq!(67 * 1_000_000_u64 * 100 / (1024 * 70), 93_470);
        // The shortest deadline on a main clock 5 % fast.
        assert_eq!(micros((deadline - 1) * period) * 100 / 105, 98_095);
    }

    #[test]
    fn feeds_on_the_first_tick() {
        assert!(Feeder::new().tick());
    }

    #[test]
    fn feeds_every_third_tick_after_that() {
        let mut feeder = Feeder::new();
        let feeds: [bool; 10] = core::array::from_fn(|_| feeder.tick());
        let (t, f) = (true, false);
        assert_eq!(feeds, [t, f, f, t, f, f, t, f, f, t]);
    }

    #[test]
    fn remembers_whether_the_last_tick_fed_it() {
        let mut feeder = Feeder::new();
        assert!(!feeder.fed(), "nothing's fed it before the first tick");
        for _ in 0..10 {
            let fed = feeder.tick();
            assert_eq!(feeder.fed(), fed);
        }
    }

    #[test]
    fn never_powers_down_straight_after_a_feed() {
        let mut feeder = Feeder::new();
        assert!(feeder.power_down(true), "before the first feed");
        let after: [bool; 6] = core::array::from_fn(|_| {
            feeder.tick();
            feeder.power_down(true)
        });
        let (t, f) = (true, false);
        assert_eq!(after, [f, t, t, f, t, t]);
    }

    #[test]
    fn never_powers_down_with_something_to_do() {
        let mut feeder = Feeder::new();
        for _ in 0..FEED_TICKS * 2 {
            assert!(!feeder.power_down(false));
            feeder.tick();
        }
    }

    /// A pass of a pacer, the way `main` runs one, with the torch's
    /// `resting` and `quiet` as given.
    fn pass(pacer: &mut Pacer, wake: Wake, resting: bool, quiet: bool) -> (Begin, End) {
        let begin = pacer.begin(wake);
        (begin, pacer.end(resting, quiet))
    }

    /// Runs quiet ticks until the pacer's slowed the PIT, and returns how many.
    fn to_slow(pacer: &mut Pacer) -> u16 {
        for n in 1..=200 {
            if pass(pacer, Wake::Tick, true, true).1.pit == Some(Pit::Slow) {
                return n;
            }
        }
        panic!("never slowed down");
    }

    #[test]
    fn slows_down_after_a_quiet_second_on_the_pass_after_a_feed() {
        let mut pacer = Pacer::new();
        let mut feeds = 0;
        for n in 1..=200u16 {
            let (begin, end) = pass(&mut pacer, Wake::Tick, true, true);
            if end.pit == Some(Pit::Slow) {
                assert!(n > u16::from(SETTLE_TICKS), "after {n}");
                assert_eq!(
                    begin.watchdog,
                    Some(Setting::Rest),
                    "writes the rest setting"
                );
                assert!(end.synced && !end.feed && end.power_down && end.switch_wakes);
                assert!(feeds > 0);
                assert!(pacer.slow() && !pacer.lit());
                return;
            }
            assert_eq!(begin.watchdog, None);
            feeds += u32::from(end.feed);
        }
        panic!("never slowed down");
    }

    #[test]
    fn stays_fast_while_anything_is_going_on() {
        let mut pacer = Pacer::new();
        for n in 0..1000 {
            let quiet = n % 50 != 0;
            let (begin, end) = pass(&mut pacer, Wake::Tick, quiet, quiet);
            assert_eq!(end.pit, None, "pass {n}");
            assert_eq!(begin.watchdog, None);
            assert!(pacer.lit());
        }
    }

    #[test]
    fn a_slow_tick_stands_for_a_second_of_ticks_and_a_switch_wake_for_none() {
        let mut pacer = Pacer::new();
        to_slow(&mut pacer);
        let (begin, end) = pass(&mut pacer, Wake::Tick, true, true);
        assert_eq!(begin.rested, SLOW_TICKS - 1);
        assert_eq!(end.pit, None);
        let begin = pacer.begin(Wake::Switch);
        assert_eq!(begin.rested, 0);
    }

    #[test]
    fn feeds_at_rest_by_way_of_the_fast_tick_and_never_before_a_power_down() {
        let mut pacer = Pacer::new();
        to_slow(&mut pacer);
        let mut ends = std::vec::Vec::new();
        for _ in 0..40 {
            ends.push(pass(&mut pacer, Wake::Tick, true, true).1);
        }
        for end in &ends {
            assert!(!(end.feed && end.power_down), "{end:?}");
            assert!(!end.synced, "nothing's written at rest");
        }
        // Four slow ticks, the last of them switching the PIT fast, then a
        // fast tick that feeds, then one that slows the PIT again.
        let pattern: std::vec::Vec<(bool, Option<Pit>)> =
            ends.iter().map(|end| (end.feed, end.pit)).collect();
        let cycle = [
            (false, None),
            (false, None),
            (false, None),
            (false, Some(Pit::Fast)),
            (true, None),
            (false, Some(Pit::Slow)),
        ];
        for (n, chunk) in pattern.chunks(cycle.len()).enumerate() {
            assert_eq!(chunk, &cycle[..chunk.len()], "cycle {n}");
        }
    }

    /// Every way a press can come up at rest, and the passes until the lit
    /// setting is back with a `wdr` behind it.
    #[test]
    fn a_press_at_rest_has_the_lit_setting_back_within_exit_passes() {
        // Slow passes, then the press on each pass of a feed trip in turn.
        for at in 0..12 {
            let mut pacer = Pacer::new();
            to_slow(&mut pacer);
            for _ in 0..at {
                pass(&mut pacer, Wake::Tick, true, true);
            }
            let wake = if pacer.slow() {
                Wake::Switch
            } else {
                Wake::Tick
            };
            let mut lit_at = None;
            for n in 0..=EXIT_PASSES {
                let (begin, end) = pass(
                    &mut pacer,
                    if n == 0 { wake } else { Wake::Tick },
                    false,
                    false,
                );
                if begin.watchdog == Some(Setting::Lit) {
                    assert!(end.synced && end.feed, "{end:?}");
                    lit_at = Some(n);
                    break;
                }
                assert!(!pacer.lit());
            }
            assert!(lit_at.is_some(), "press {at} passes into the rest");
            assert!(pacer.lit());
        }
        // On the pass that goes to rest.
        let mut pacer = Pacer::new();
        for _ in 0..200 {
            if pacer.state == State::Entering {
                break;
            }
            pass(&mut pacer, Wake::Tick, true, true);
        }
        assert_eq!(pacer.state, State::Entering);
        let (_, end) = pass(&mut pacer, Wake::Tick, false, false);
        assert!(end.synced && end.feed && end.pit.is_none() && !end.power_down);
        let (begin, end) = pass(&mut pacer, Wake::Tick, false, false);
        assert_eq!(begin.watchdog, Some(Setting::Lit));
        assert!(end.synced && end.feed);
        assert!(pacer.lit());
    }

    /// A switch edge that the pass finds released, a bounce, still puts the
    /// tick back to 64 Hz for a second.
    #[test]
    fn a_switch_wake_starts_the_quiet_second_again() {
        let mut pacer = Pacer::new();
        to_slow(&mut pacer);
        let (_, end) = pass(&mut pacer, Wake::Switch, true, true);
        assert_eq!(end.pit, Some(Pit::Fast));
        assert!(end.power_down, "nothing to do, and no feed this pass");
        let n = to_slow(&mut pacer);
        assert!(n > u16::from(SETTLE_TICKS), "{n}");
    }

    /// The lit feeds after putting the lit setting back keep to the window
    /// as ever.
    #[test]
    fn lit_feeds_carry_on_every_third_pass_after_a_rest() {
        let mut pacer = Pacer::new();
        to_slow(&mut pacer);
        let mut feeds = std::vec::Vec::new();
        let mut wake = Wake::Switch;
        for _ in 0..20 {
            let (_, end) = pass(&mut pacer, wake, false, false);
            wake = Wake::Tick;
            if pacer.lit() {
                feeds.push(end.feed);
            }
        }
        let (t, f) = (true, false);
        assert_eq!(&feeds[..7], &[t, f, f, t, f, f, t]);
    }

    /// The lit setting is in force in the fast state alone, on every pass
    /// of a rest and a way back from it, and only once the pass that wrote
    /// it has fed it.
    #[test]
    fn the_lit_setting_is_in_force_only_in_the_fast_state() {
        let mut pacer = Pacer::new();
        let mut states = std::vec::Vec::new();
        let mut wake = Wake::Tick;
        for n in 0..400 {
            let quiet = n < 300;
            if n == 300 {
                wake = Wake::Switch;
            }
            let was = pacer.state;
            let (begin, end) = pass(&mut pacer, wake, quiet, quiet);
            wake = Wake::Tick;
            states.push(pacer.state);
            assert_eq!(pacer.lit(), pacer.state == State::Fast, "{:?}", pacer.state);
            assert!(!pacer.lit() || was == State::Fast || end.feed, "{was:?}");
            assert!(begin.watchdog != Some(Setting::Rest) || !pacer.lit());
        }
        for state in [
            State::Fast,
            State::Entering,
            State::Slow { since: 1 },
            State::Bridge,
            State::Returning,
            State::Restoring,
        ] {
            assert!(states.contains(&state), "never in {state:?}");
        }
    }

    #[test]
    fn the_margins_at_rest() {
        let slow = SLOW_TICKS * CLOCKS_PER_TICK;
        assert_eq!(slow, 1024);
        // The longest gap between feeds at rest, against 8,192 clocks.
        let rest = CLOCKS_PER_TICK + 2 * slow + 3 * slow + 2 * CLOCKS_PER_TICK + 4 + 1;
        assert_eq!(rest, 5173);
        assert_eq!(u32::from(rest) * 100 / u32::from(REST_CLOCKS), 63);
        // The gap a write of the lit setting has to fit in, against 32.
        assert_eq!(CLOCKS_PER_TICK + 4 + 1, 21);
    }

    #[test]
    fn every_feed_lands_mid_window() {
        let middle = CLOSED_CLOCKS + OPEN_CLOCKS / 2;
        let mut feeder = Feeder::new();
        let mut last = None;
        for tick in 0..1000u16 {
            if feeder.tick() {
                if let Some(last) = last {
                    assert_eq!((tick - last) * CLOCKS_PER_TICK, middle, "tick {tick}");
                }
                last = Some(tick);
            }
        }
        assert!(last.is_some());
    }
}
