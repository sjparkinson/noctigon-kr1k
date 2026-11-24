//! When to feed the watchdog, which runs in window mode while lit (AN2632 §3.2),
//! and how the loop is paced. Nobody has seen this watchdog bite on a real chip.

use crate::{TICKS_PER_SECOND, u16_from, u32_from};

/// Cycles of the WDT's 1.024 kHz clock in one tick.
pub const CLOCKS_PER_TICK: u16 = 1024 / TICKS_PER_SECOND;
/// In WDT clocks. `hw::start_watchdog` sets `WDT.CTRLA.WINDOW` to match.
pub const CLOSED_CLOCKS: u16 = 32;
/// In WDT clocks. `hw::start_watchdog` sets `WDT.CTRLA.PERIOD` to match.
pub const OPEN_CLOCKS: u16 = 32;
pub const FEED_TICKS: u8 = 3;

// Window mode only starts with the first `wdr` (DS40002413B §22.3.3.2), so the
// first feed likely has the open window alone, which the datasheet only implies.
const _: () = {
    let gap = u16_from(FEED_TICKS) * CLOCKS_PER_TICK;
    assert!(gap > CLOSED_CLOCKS && gap < CLOSED_CLOCKS + OPEN_CLOCKS);
    assert!(CLOCKS_PER_TICK < OPEN_CLOCKS);
};

pub const CLOCK_HZ: u32 = 4_000_000;

/// Main clocks in one period of TCB0. `hw::init` writes one less than this to
/// CCMP, since TCB0 counts from 0 to CCMP (DS40002413B §24.3.3.1.1).
pub const TIMER_PERIOD_CLOCKS: u16 = 4000;

/// TCB0 periods `hw::wait_for_tick` waits for a tick, with the light on, before `main` halts.
pub const TICK_DEADLINE_PERIODS: u16 = 104;

const _: () = {
    let period = u32_from(TIMER_PERIOD_CLOCKS);
    let tick = CLOCK_HZ / u32_from(TICKS_PER_SECOND);
    let shortest = (u32_from(TICK_DEADLINE_PERIODS) - 1) * period;
    let longest = u32_from(TICK_DEADLINE_PERIODS) * period;
    assert!(shortest > 6 * tick && longest < 7 * tick);
    // The main oscillator at most 5 % fast (DS40002413B figure 38-2) and the
    // 32 kHz one 30 % slow, which is the ATtiny1616's figure (DS40002204A table
    // 36-13) and a guess for this chip.
    let slowest = tick * 105 / 70;
    let bite = u32_from(3 + CLOSED_CLOCKS + OPEN_CLOCKS);
    assert!(shortest * u32_from(CLOCKS_PER_TICK) > bite * slowest);
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Feeder {
    wait: u8,
    fed: bool,
}

impl Feeder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wait: 0,
            fed: false,
        }
    }

    #[must_use]
    pub const fn just_fed() -> Self {
        Self {
            wait: FEED_TICKS - 1,
            fed: true,
        }
    }

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

    #[cfg(test)]
    const fn fed(self) -> bool {
        self.fed
    }

    /// Never straight after a feed. A `wdr` takes two or three WDT clocks to sync
    /// (§22.3.6), and the datasheet doesn't say it does with the peripheral clock stopped.
    #[must_use]
    pub const fn power_down(&self, resting: bool) -> bool {
        resting && !self.fed
    }
}

/// Ticks one slow tick stands for. `hw::set_pit` sets the PIT's period to match.
pub const SLOW_TICKS: u16 = TICKS_PER_SECOND;

pub const SETTLE_TICKS: u8 = 64;

pub const FEED_SLOW_TICKS: u8 = 4;

/// In WDT clocks, with no window. `hw::set_watchdog` sets `WDT.CTRLA.PERIOD` to match.
pub const REST_CLOCKS: u16 = 8192;

/// The most a pass may take, in WDT clocks, which is `cargo xtask hex`'s budget.
const PASS_CLOCKS: u16 = CLOCKS_PER_TICK / 4;

/// WDT clocks a write to `WDT.CTRLA` is taken to need to sync. The datasheet
/// only gives `wdr`'s, two to three (§22.3.6), so this is likely, not stated.
pub const SYNC_CLOCKS: u16 = 3;

/// How much further apart two `wdr`s can sync than they ran, in WDT clocks (§22.3.6).
const SYNC_SPREAD: u16 = 1;

/// Passes after the one that sees a press before the lit setting is back and fed.
pub const EXIT_PASSES: u16 = 2;

const _: () = {
    use crate::button::{HOLD_TICKS, RELEASE_TICKS};
    assert!(RELEASE_TICKS + 1 > EXIT_PASSES && HOLD_TICKS > EXIT_PASSES);
    // The first PIT period after a change isn't stated (§26.5.2.2), so the
    // first slow one counts as up to two.
    let slow = SLOW_TICKS * CLOCKS_PER_TICK;
    let rest = CLOCKS_PER_TICK
        + 2 * slow
        + (u16_from(FEED_SLOW_TICKS) - 1) * slow
        + 2 * CLOCKS_PER_TICK
        + PASS_CLOCKS
        + SYNC_SPREAD;
    assert!(rest < REST_CLOCKS * 2 / 3);
    // The datasheet doesn't say a write of the lit setting restarts the count.
    // If it doesn't, the `wdr` after it has the open window alone (§22.3.3.2).
    assert!(CLOCKS_PER_TICK + max(PASS_CLOCKS, SYNC_CLOCKS) + SYNC_SPREAD < OPEN_CLOCKS);
    assert!(
        CLOCKS_PER_TICK + PASS_CLOCKS + SYNC_CLOCKS + SYNC_SPREAD < CLOSED_CLOCKS + OPEN_CLOCKS
    );
};

const fn max(a: u16, b: u16) -> u16 {
    if a > b { a } else { b }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Tick,
    Switch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// Window mode, and the only setting PC1 may rise under.
    Lit,
    Rest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pit {
    Fast,
    Slow,
}

/// What a pass does first, before it reads the switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Begin {
    /// Resting ticks the wait slept through, beyond the pass's own.
    pub rested: u16,
    pub watchdog: Option<Setting>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sleep {
    Idle,
    PowerDown,
    /// A power-down the switch wakes too. Only with the PIT slow.
    SwitchWakes,
}

impl Sleep {
    const fn power_down_if(resting: bool) -> Self {
        if resting { Self::PowerDown } else { Self::Idle }
    }
}

/// What a pass does last, after the output, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct End {
    /// Wait for [`Begin::watchdog`]'s write to sync.
    pub synced: bool,
    pub feed: bool,
    pub pit: Option<Pit>,
    pub sleep: Sleep,
}

impl End {
    #[must_use]
    pub const fn start(resting: bool) -> Self {
        Self {
            synced: false,
            feed: false,
            pit: None,
            sleep: Sleep::power_down_if(resting),
        }
    }

    const fn fed() -> Self {
        Self {
            synced: false,
            feed: true,
            pit: None,
            sleep: Sleep::Idle,
        }
    }

    const fn slow() -> Self {
        Self {
            synced: false,
            feed: false,
            pit: Some(Pit::Slow),
            sleep: Sleep::SwitchWakes,
        }
    }
}

/// `Fast` is the only state the light can be on in. `Bridge` and `Returning` are
/// the trip back to 64 Hz that every feed at rest takes, since no `wdr` goes
/// before a power-down. `since` is slow ticks since the last feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    First,
    Fast,
    Entering,
    Slow { since: u8 },
    Bridge,
    Returning,
    Restoring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pacer {
    state: State,
    feeder: Feeder,
    settle: u8,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::First,
            feeder: Feeder::new(),
            settle: 0,
        }
    }

    #[must_use]
    pub const fn lit(&self) -> bool {
        matches!(self.state, State::Fast)
    }

    #[cfg(test)]
    #[must_use]
    pub const fn slow(&self) -> bool {
        matches!(self.state, State::Slow { .. })
    }

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

    /// `quiet` is nothing timed for more than a slow tick and the aux settled.
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
                    sleep: Sleep::power_down_if(self.feeder.power_down(resting)),
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
                            sleep: Sleep::power_down_if(resting),
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

    #[test]
    fn the_tick_deadline_is_103_to_104_ms_against_a_bite_of_93_5_at_worst() {
        let period = u32::from(TIMER_PERIOD_CLOCKS);
        let micros = |clocks: u32| clocks / (CLOCK_HZ / 1_000_000);
        assert_eq!(micros(period), 1000);
        let deadline = u32::from(TICK_DEADLINE_PERIODS);
        assert_eq!(micros((deadline - 1) * period), 103_000);
        assert_eq!(micros(deadline * period), 104_000);
        assert_eq!(67 * 1_000_000_u64 * 100 / (1024 * 70), 93_470);
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

    fn pass(pacer: &mut Pacer, wake: Wake, resting: bool, quiet: bool) -> (Begin, End) {
        let begin = pacer.begin(wake);
        (begin, pacer.end(resting, quiet))
    }

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
                assert!(end.synced && !end.feed && end.sleep == Sleep::SwitchWakes);
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
            assert!(!end.feed || end.sleep == Sleep::Idle, "{end:?}");
            assert!(!end.synced, "nothing's written at rest");
        }
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

    #[test]
    fn a_press_at_rest_has_the_lit_setting_back_within_exit_passes() {
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
        let mut pacer = Pacer::new();
        for _ in 0..200 {
            if pacer.state == State::Entering {
                break;
            }
            pass(&mut pacer, Wake::Tick, true, true);
        }
        assert_eq!(pacer.state, State::Entering);
        let (_, end) = pass(&mut pacer, Wake::Tick, false, false);
        assert!(end.synced && end.feed && end.pit.is_none() && end.sleep == Sleep::Idle);
        let (begin, end) = pass(&mut pacer, Wake::Tick, false, false);
        assert_eq!(begin.watchdog, Some(Setting::Lit));
        assert!(end.synced && end.feed);
        assert!(pacer.lit());
    }

    #[test]
    fn a_switch_wake_starts_the_quiet_second_again() {
        let mut pacer = Pacer::new();
        to_slow(&mut pacer);
        let (_, end) = pass(&mut pacer, Wake::Switch, true, true);
        assert_eq!(end.pit, Some(Pit::Fast));
        assert_eq!(
            end.sleep,
            Sleep::PowerDown,
            "nothing to do, and no feed this pass"
        );
        let n = to_slow(&mut pacer);
        assert!(n > u16::from(SETTLE_TICKS), "{n}");
    }

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
        let rest = CLOCKS_PER_TICK + 2 * slow + 3 * slow + 2 * CLOCKS_PER_TICK + 4 + 1;
        assert_eq!(rest, 5173);
        assert_eq!(u32::from(rest) * 100 / u32::from(REST_CLOCKS), 63);
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
