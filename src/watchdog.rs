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
#[derive(Clone, Copy, Debug, Default)]
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

#[cfg(test)]
mod tests {
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
