//! One pass of the firmware's loop, minus the hardware. A switch sample and
//! an ADC result in, a level and the next conversion out.

use crate::TICKS_PER_SECOND;
use crate::battery;
use crate::button::{Clicks, Event};
use crate::gradual::Fade;
use crate::limits::{Health, Limits};
use crate::ramp::{self, Output};
use crate::selftest::Faults;
use crate::sensors::Convert;
use crate::temp::Calibration;
use crate::ui::{Aux, Mode, Ui};
use crate::watchdog;

/// Ticks the aux say why the light won't come on. Unmeasured.
pub const SHOW_TICKS: u16 = 3 * TICKS_PER_SECOND;

/// Ticks the aux shows red after a press that leaves it locked. Unmeasured.
pub const LOCKED_TICKS: u16 = 5 * TICKS_PER_SECOND;

/// Ticks of rest between readings of the cell. The 32 kHz oscillator that
/// times it has no accuracy in the datasheet. Unmeasured.
pub const CHECK_TICKS: u16 = 5 * 60 * TICKS_PER_SECOND;

/// Centivolts, read with the light off. Unmeasured.
pub const AUX_DARK_BELOW: u16 = 330;

/// Centivolts. The gap over [`AUX_DARK_BELOW`] is the hysteresis. Unmeasured.
pub const AUX_LIT_FROM: u16 = 340;

const _: () = assert!(AUX_DARK_BELOW < AUX_LIT_FROM);

#[derive(Clone, Debug)]
pub struct Torch {
    clicks: Clicks,
    ui: Ui,
    limits: Limits,
    /// Ticks left of [`SHOW_TICKS`] or [`LOCKED_TICKS`].
    showing: u16,
    /// Whether the light was on last tick.
    warm: bool,
    /// Ticks of rest left until the next check of the cell, `0` meaning due.
    check_in: u16,
    /// Reading the cell with the switch up and the light off.
    checking: bool,
    cell_low: bool,
    fade: Fade,
    /// What the UI asked of `limits` on its last tick.
    asked: u8,
}

impl Torch {
    pub const fn new(calibration: Calibration, faults: Faults) -> Self {
        Self {
            clicks: Clicks::new(),
            ui: Ui::new(),
            limits: Limits::new(calibration, faults),
            showing: if faults.any() { SHOW_TICKS } else { 0 },
            warm: false,
            check_in: 0,
            checking: false,
            cell_low: false,
            fade: Fade::new(),
            asked: 0,
        }
    }

    /// `lit_window` is `watchdog::Pacer::lit`, the only setting PC1 may rise under.
    pub fn tick(
        &mut self,
        pressed: bool,
        adc: Option<u16>,
        vddio2: bool,
        lit_window: bool,
    ) -> Option<Event> {
        self.warm = self.level() > 0;
        self.showing = self.showing.saturating_sub(1);
        let event = self.clicks.tick_up_to(pressed, self.ui.most_clicks());
        if let Some(event) = event {
            let press = matches!(event, Event::Click(_) | Event::Hold { ticks: 0, .. });
            if self.limits.health() == Health::Good || self.level() > 0 {
                self.ui.handle(event);
                if press && self.ui.mode() == Mode::Lockout {
                    self.showing = LOCKED_TICKS;
                }
            } else if press {
                self.showing = SHOW_TICKS;
            }
        }
        let before = self.limits.health();
        let lit = self.level() > 0;
        if !self.quiet() {
            self.checking = false;
        } else if self.limits.convert().is_some() || (self.check_in == 0 && before != Health::Fault)
        {
            self.checking = true;
        }
        let mut gentle = false;
        if !self.resting() {
            let asked = if lit_window {
                self.ui.output_level()
            } else {
                0
            };
            let step = self.limits.tick(asked, adc, vddio2);
            let (battery, thermal) = self.limits.ceilings();
            gentle = asked == self.asked && thermal <= battery;
            self.asked = asked;
            if step.off {
                self.ui.off();
            }
            match step.unloaded {
                Some(volts) if !battery::plausible(volts) => {}
                Some(volts) if volts < AUX_DARK_BELOW => self.cell_low = true,
                Some(volts) if volts >= AUX_LIT_FROM => self.cell_low = false,
                _ => {}
            }
            if self.limits.convert().is_none() {
                self.checking = false;
            }
        }
        let health = self.limits.health();
        if health != Health::Good && (before == Health::Good || lit) {
            self.showing = SHOW_TICKS;
        }
        // Checked again, since the tick above may have turned the light off.
        if self.resting() {
            self.limits.rest();
            self.check_in = self.check_in.saturating_sub(1);
        } else {
            self.check_in = CHECK_TICKS;
        }
        self.fade.tick(self.level(), gentle);
        event
    }

    const fn quiet(&self) -> bool {
        self.clicks.idle() && self.ui.output_level() == 0 && self.limits.level() == 0
    }

    /// The firmware powers down between ticks while this holds.
    pub const fn resting(&self) -> bool {
        self.quiet() && !self.checking
    }

    /// How many resting ticks in a row would change nothing but counters.
    pub const fn quiet_for(&self) -> Option<u16> {
        if !self.resting() || self.showing > 0 {
            None
        } else if matches!(self.limits.health(), Health::Fault) {
            Some(u16::MAX)
        } else {
            Some(self.check_in)
        }
    }

    pub fn can_slow(&self) -> bool {
        self.quiet_for()
            .is_some_and(|ticks| ticks > watchdog::SLOW_TICKS)
    }

    pub fn rest_for(&mut self, ticks: u16) {
        let ticks = match self.quiet_for() {
            Some(quiet) => ticks.min(quiet),
            None => 0,
        };
        if ticks == 0 {
            return;
        }
        self.warm = false;
        self.clicks.wait(ticks);
        self.limits.rest_for(ticks);
        self.check_in = self.check_in.saturating_sub(ticks);
    }

    pub const fn level(&self) -> u8 {
        self.limits.level()
    }

    pub const fn health(&self) -> Health {
        self.limits.health()
    }

    pub const fn convert(&self) -> Option<Convert> {
        self.limits.convert()
    }

    /// PC1 up with the DAC at 0 for a tick first is the stock firmware's
    /// order. Whether the other order would be safer is unknown.
    pub fn output(&self) -> Output {
        match self.level() {
            0 => Output::OFF,
            _ if !self.warm => Output::STARTING,
            _ => ramp::output_between(self.fade.position()),
        }
    }

    pub const fn aux(&self) -> Aux {
        match self.limits.health() {
            Health::Good => match self.ui.aux() {
                Aux::Red if self.showing == 0 => Aux::Off,
                Aux::Blue if self.cell_low => Aux::Off,
                colour => colour,
            },
            _ if self.level() > 0 || self.showing == 0 => Aux::Off,
            Health::Flat => Aux::Yellow,
            Health::Fault => Aux::White,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Changes<T> {
    last: Option<T>,
}

impl<T: Copy + PartialEq> Changes<T> {
    pub const fn new() -> Self {
        Self { last: None }
    }

    pub fn update(&mut self, value: T) -> Option<T> {
        if self.last == Some(value) {
            return None;
        }
        self.last = Some(value);
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::battery;
    use crate::button::{HOLD_TICKS, RELEASE_TICKS};
    use crate::ramp::CEILING;
    use crate::sensors::Channel;
    use crate::testing::{
        EXAMPLES, GROUND_READING, Rig, Rng, example, temperature_reading, voltage_reading,
    };
    use crate::thermal;
    use crate::ui::{self, Mode, ON_LEVEL};
    use std::vec::Vec;

    /// The cell's two conversions.
    const CHECK_AWAKE: u16 = 2;

    fn dim() -> Output {
        ramp::output(ON_LEVEL)
    }

    fn run(torch: &mut Rig, pressed: bool, ticks: u16) {
        for _ in 0..ticks {
            torch.tick(pressed);
        }
    }

    fn clicks(torch: &mut Rig, n: u8) {
        for _ in 0..n {
            run(torch, true, 4);
            run(torch, false, 4);
        }
    }

    fn click(torch: &mut Rig) {
        clicks(torch, 1);
        run(torch, false, RELEASE_TICKS + 1);
    }

    fn at_the_top(torch: &mut Rig) {
        click(torch);
        run(torch, true, 3 * TICKS_PER_SECOND);
        run(torch, false, RELEASE_TICKS + 1);
        assert_eq!(torch.level(), CEILING);
    }

    #[test]
    fn every_example_holds() {
        for example in EXAMPLES {
            let level = Rig::play(example.actions()).level();
            assert!(
                example.expect.holds(level),
                "{}: expected {:?}, but it ends at level {level}",
                example.name,
                example.expect
            );
        }
    }

    #[test]
    fn every_example_holds_paced() {
        use crate::testing::{Paced, PitPhase, Scene, samples};
        for phase in [PitPhase::Aligned, PitPhase::Early, PitPhase::Late] {
            for example in EXAMPLES {
                let mut script = samples(example.actions()).peekable();
                let power_on = script.peek().map_or(Scene::NORMAL, |s| s.scene);
                let mut paced = Paced::new(power_on.faults(), 0);
                paced.phase = phase;
                for sample in script {
                    paced.sample(sample.pressed, &sample.scene);
                }
                assert!(
                    example.expect.holds(paced.level()),
                    "{} {phase:?}: ends at level {}",
                    example.name,
                    paced.level()
                );
            }
        }
    }

    #[test]
    fn pacing_never_changes_the_light_and_only_lights_it_lit() {
        use crate::testing::{Paced, PitPhase, Scene};
        for (seed, phase) in [
            (1, PitPhase::Aligned),
            (2, PitPhase::Early),
            (3, PitPhase::Late),
        ] {
            let mut rng = Rng::new(seed);
            let mut plain = Rig::default();
            let mut paced = Paced::new(Faults::NONE, 0);
            paced.phase = phase;
            let (mut slow, mut passes, mut lit) = (0u32, 0u32, 0u32);
            for _ in 0..400 {
                let (pressed, ticks) = match rng.next_u16() % 4 {
                    0 => (true, 2 + rng.next_u16() % 6),
                    1 => (true, 20 + rng.next_u16() % 100),
                    2 => (false, 2 + rng.next_u16() % 20),
                    _ => (false, rng.next_u16() % 600),
                };
                for _ in 0..ticks {
                    plain.tick_in(pressed, &Scene::NORMAL);
                    if paced.sample(pressed, &Scene::NORMAL).is_some() {
                        passes += 1;
                    }
                    slow += u32::from(paced.pacer.slow());
                    assert!(
                        paced.rig.lit_window || paced.rig.torch.ui.output_level() == 0,
                        "{phase:?}: the UI wants light outside the lit window"
                    );
                    let shown = paced.output();
                    if phase == PitPhase::Aligned {
                        assert_eq!(shown, plain.output());
                    }
                    lit += u32::from(shown.enable);
                    assert!(!shown.enable || paced.pacer.lit(), "{phase:?}");
                }
            }
            assert!(slow > 10_000, "{phase:?} hardly ever slowed: {slow}");
            assert!(passes > 0 && lit > 1000, "{phase:?}: lit for {lit}");
        }
    }

    #[test]
    fn resting_for_a_while_is_resting_tick_by_tick() {
        let mut rng = Rng::new(9);
        let mut torch = Rig::default();
        let mut exact = 0u32;
        for round in 0..3000 {
            let pressed = rng.next_u16() % 3 == 0;
            for _ in 0..rng.next_u16() % 40 {
                torch.tick(pressed);
            }
            if round % 7 == 0 {
                run(&mut torch, false, rng.next_u16() % 20_000);
            }
            let ticks = rng.next_u16() % 200;
            let quiet = torch.quiet_for().map_or(0, |quiet| quiet.min(ticks));
            let mut by_tick = torch.torch.clone();
            for _ in 0..quiet {
                by_tick.tick(false, None, true, true);
            }
            let mut at_once = torch.torch.clone();
            at_once.rest_for(ticks);
            assert_eq!(
                std::format!("{at_once:?}"),
                std::format!("{by_tick:?}"),
                "round {round}, {ticks} ticks, quiet for {quiet}"
            );
            exact += u32::from(quiet == ticks && ticks > 0);
            torch.torch = at_once;
        }
        assert!(exact > 300, "only {exact} rounds rested in full");
    }

    #[test]
    fn resting_for_more_than_its_quiet_for_never_skips_a_check() {
        let mut torch = Rig::default();
        run(&mut torch, false, 8);
        let quiet = torch.quiet_for().unwrap();
        run(&mut torch, false, quiet - 10);
        torch.torch.rest_for(watchdog::SLOW_TICKS);
        assert_eq!(torch.quiet_for(), Some(0), "rested only to the check");
        torch.tick(false);
        assert!(!torch.resting(), "the check's on");
        let mut torch = Rig::default();
        tap(&mut torch, 3, Event::Click(3));
        let before = std::format!("{:?}", torch.torch);
        torch.torch.rest_for(watchdog::SLOW_TICKS);
        assert_eq!(std::format!("{:?}", torch.torch), before, "red's showing");
    }

    #[test]
    fn quiet_only_at_rest_with_nothing_showing() {
        let mut torch = Rig::default();
        assert_eq!(torch.quiet_for(), Some(0), "the cell's due a check");
        run(&mut torch, false, 8);
        assert!(torch.quiet_for() > Some(CHECK_TICKS - 8), "checked");
        assert!(torch.can_slow());
        torch.tick(true);
        assert_eq!(torch.quiet_for(), None, "pressed");
        let mut torch = Rig::default();
        tap(&mut torch, 3, Event::Click(3));
        run(&mut torch, false, 4);
        assert_eq!(torch.quiet_for(), None, "red's showing");
        run(&mut torch, false, LOCKED_TICKS);
        assert!(torch.can_slow());
        let mut faulty = Rig::new(crate::selftest::Faults {
            hang: true,
            ..Faults::NONE
        });
        run(&mut faulty, false, SHOW_TICKS + 4);
        assert_eq!(
            faulty.quiet_for(),
            Some(u16::MAX),
            "a fault skips the checks"
        );
    }

    #[test]
    fn starts_off() {
        let mut torch = Rig::default();
        assert_eq!(torch.tick(false), None);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.output(), Output::OFF);
    }

    #[test]
    fn a_click_turns_it_on_once_the_click_counts() {
        let mut torch = Rig::default();
        run(&mut torch, true, 4);
        run(&mut torch, false, RELEASE_TICKS);
        assert_eq!(torch.output(), Output::OFF);
        assert_eq!(torch.tick(false), Some(Event::Click(1)));
        assert_eq!(torch.output(), Output::STARTING);
        torch.tick(false);
        assert_eq!(torch.output(), dim());
    }

    #[test]
    fn the_driver_starts_with_the_dac_at_zero_for_a_tick_every_time() {
        let mut torch = Rig::default();
        for _ in 0..3 {
            run(&mut torch, true, 4);
            run(&mut torch, false, RELEASE_TICKS);
            torch.tick(false);
            assert_eq!(torch.level(), ON_LEVEL);
            assert_eq!(torch.output(), Output::STARTING);
            torch.tick(false);
            assert_eq!(torch.output(), dim());
            run(&mut torch, true, 4);
            run(&mut torch, false, RELEASE_TICKS);
            torch.tick(false);
            assert_eq!(torch.output(), Output::OFF);
        }
    }

    #[test]
    fn outside_the_lit_window_it_stays_dark_and_starts_afresh_after() {
        let mut torch = Rig {
            lit_window: false,
            ..Rig::default()
        };
        click(&mut torch);
        assert_eq!(torch.ui.output_level(), ON_LEVEL, "the UI's on");
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.output(), Output::OFF);
        run(&mut torch, false, 10);
        assert_eq!(torch.output(), Output::OFF);
        assert!(!torch.resting(), "awake, so the pacer puts the window back");
        torch.lit_window = true;
        torch.tick(false);
        assert_eq!(torch.output(), Output::STARTING);
        torch.tick(false);
        assert_eq!(torch.output(), dim());
        torch.lit_window = false;
        torch.tick(false);
        assert_eq!((torch.level(), torch.output()), (0, Output::OFF));
        torch.lit_window = true;
        torch.tick(false);
        assert_eq!(torch.output(), Output::STARTING);
        torch.tick(false);
        assert_eq!(torch.output(), dim());
    }

    #[test]
    fn a_second_click_turns_it_off() {
        let mut torch = Rig::default();
        click(&mut torch);
        click(&mut torch);
        assert_eq!(torch.output(), Output::OFF);
    }

    #[test]
    fn lockout_never_lights() {
        let mut torch = Rig::default();
        clicks(&mut torch, 3);
        run(&mut torch, false, RELEASE_TICKS + 1);
        run(&mut torch, true, 2 * TICKS_PER_SECOND);
        assert_eq!(torch.level(), 0);
        run(&mut torch, false, RELEASE_TICKS + 1);
        click(&mut torch);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.aux(), Aux::Red);
    }

    fn tap(torch: &mut Rig, n: u8, event: Event) {
        for _ in 0..n {
            run(torch, true, 4);
            for _ in 0..4 {
                if torch.tick(false) == Some(event) {
                    return;
                }
            }
        }
        for _ in 0..TICKS_PER_SECOND {
            if torch.tick(false) == Some(event) {
                return;
            }
        }
        panic!("no {event:?}");
    }

    #[test]
    fn a_click_counts_as_it_is_let_go_when_no_more_would_mean_anything() {
        let mut torch = Rig::default();
        click(&mut torch);
        run(&mut torch, true, 4);
        assert_eq!(torch.tick(false), Some(Event::Click(1)), "off, from on");
        assert_eq!(torch.level(), 0);
        clicks(&mut torch, 2);
        run(&mut torch, true, 4);
        assert_eq!(torch.tick(false), Some(Event::Click(3)), "3C, from off");
        assert_eq!(torch.torch.ui.mode(), Mode::Lockout);
    }

    #[test]
    fn four_clicks_in_lockout_neither_unlock_it_nor_light_it() {
        let mut torch = Rig::default();
        tap(&mut torch, 3, Event::Click(3));
        run(&mut torch, false, RELEASE_TICKS + 1);
        tap(&mut torch, 4, Event::Click(4));
        assert_eq!(torch.torch.ui.mode(), Mode::Lockout);
        run(&mut torch, false, TICKS_PER_SECOND);
        assert_eq!(torch.level(), 0);
    }

    #[test]
    fn lockout_shows_bright_red_for_five_seconds_after_each_press() {
        let mut torch = Rig::default();
        tap(&mut torch, 3, Event::Click(3));
        assert_eq!(torch.aux(), Aux::Red, "as 3C locks it");
        for tick in 1..LOCKED_TICKS {
            torch.tick(false);
            assert_eq!(torch.aux(), Aux::Red, "tick {tick}");
        }
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off);
        tap(&mut torch, 1, Event::Click(1));
        assert_eq!(torch.aux(), Aux::Red, "a press that does nothing");
        run(&mut torch, false, LOCKED_TICKS);
        assert_eq!(torch.aux(), Aux::Off);
        tap(&mut torch, 3, Event::Click(3));
        assert_eq!(torch.aux(), Aux::Blue, "3C unlocks it");
    }

    #[test]
    fn a_hold_in_lockout_cannot_keep_the_red_lit() {
        let mut torch = Rig::default();
        tap(&mut torch, 3, Event::Click(3));
        run(&mut torch, false, LOCKED_TICKS);
        assert_eq!(torch.aux(), Aux::Off);
        run(&mut torch, true, HOLD_TICKS + 1);
        assert_eq!(torch.aux(), Aux::Red, "from the start of the hold");
        run(&mut torch, true, LOCKED_TICKS);
        assert_eq!(torch.aux(), Aux::Off, "still held");
    }

    fn next_check(torch: &mut Rig) -> u32 {
        let mut ticks = 0;
        while torch.resting() {
            torch.tick(false);
            ticks += 1;
            assert!(ticks <= u32::from(CHECK_TICKS) + 1, "no check");
        }
        while !torch.resting() {
            torch.tick(false);
            ticks += 1;
        }
        ticks
    }

    fn assert_a_check(torch: &mut Rig, why: &str) {
        let convert = |channel, setup| Some(Convert { channel, setup });
        for (tick, started) in [
            convert(Channel::Voltage, true),
            convert(Channel::Voltage, false),
        ]
        .into_iter()
        .enumerate()
        {
            torch.tick(false);
            assert!(!torch.resting(), "{why}: the check's tick {tick}");
            assert_eq!(torch.convert(), started, "{why}: tick {tick}");
        }
        torch.tick(false);
        assert!(torch.resting(), "{why}: the readings are in");
        assert_eq!(torch.convert(), None, "{why}");
    }

    fn rest(torch: &mut Rig, ticks: u16, why: &str) {
        for tick in 0..ticks {
            torch.tick(false);
            assert!(torch.resting(), "{why}: tick {tick} of rest");
        }
    }

    #[test]
    fn the_cell_is_read_on_the_first_tick_and_after_every_five_minutes_of_rest() {
        assert_eq!(CHECK_TICKS, 5 * 60 * 64);
        let mut torch = Rig::default();
        assert!(torch.resting(), "before the first tick");
        assert_a_check(&mut torch, "power-on");
        for why in ["the first after", "the second", "the third"] {
            // The tick the reading came in was the first of the rest.
            rest(&mut torch, CHECK_TICKS - 1, why);
            assert_a_check(&mut torch, why);
        }
    }

    #[test]
    fn a_press_or_the_light_starts_the_five_minutes_again() {
        let mut torch = Rig::default();
        run(&mut torch, false, CHECK_AWAKE + 1);
        rest(&mut torch, CHECK_TICKS / 2, "half way");
        tap(&mut torch, 2, Event::Click(2));
        while !torch.resting() {
            torch.tick(false);
        }
        rest(&mut torch, CHECK_TICKS - 1, "after the press");
        assert_a_check(&mut torch, "five minutes after the press");
        rest(&mut torch, CHECK_TICKS / 2, "half way");
        click(&mut torch);
        run(&mut torch, false, 10 * TICKS_PER_SECOND);
        tap(&mut torch, 1, Event::Click(1));
        assert_eq!(torch.level(), 0);
        assert!(torch.resting(), "out as 1C's let go");
        rest(&mut torch, CHECK_TICKS - 1, "after the light");
        assert_a_check(&mut torch, "five minutes after the light");
    }

    #[test]
    fn blue_stays_lit_through_three_days_of_rest_with_a_good_cell() {
        let mut torch = Rig::default();
        let (mut checks, mut resting) = (0, true);
        for tick in 0..3 * 24 * 60 * 60 * u32::from(TICKS_PER_SECOND) {
            torch.tick(false);
            assert_eq!(torch.aux(), Aux::Blue, "tick {tick}");
            assert_eq!(torch.level(), 0, "tick {tick}");
            checks += u32::from(resting && !torch.resting());
            resting = torch.resting();
        }
        // One at power-on and one every CHECK_TICKS + CHECK_AWAKE after.
        assert_eq!(checks, 864);
    }

    #[test]
    fn a_low_cell_at_power_on_darkens_blue_from_the_first_check() {
        let mut torch = Rig::default();
        torch.adc.voltage = voltage_reading(320);
        run(&mut torch, false, 2);
        assert_eq!(torch.aux(), Aux::Blue, "before the reading's in");
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off, "the third tick, as it comes in");
        for _ in 0..3 {
            next_check(&mut torch);
            assert_eq!(torch.aux(), Aux::Off);
        }
        click(&mut torch);
        assert_eq!(torch.level(), ON_LEVEL);
        click(&mut torch);
        assert_eq!(torch.aux(), Aux::Off);
    }

    #[test]
    fn blue_goes_dark_under_3_30_volts_and_comes_back_at_3_40() {
        for cv in [329, 330, 335, 339, 340] {
            assert_eq!(battery::centivolts(voltage_reading(cv)), cv);
        }
        let mut torch = Rig::default();
        for (cv, aux, why) in [
            (330, Aux::Blue, "3.30 V is still lit"),
            (329, Aux::Off, "under 3.30 V"),
            (335, Aux::Off, "3.35 V doesn't light it"),
            (339, Aux::Off, "nor 3.39 V"),
            (340, Aux::Blue, "3.40 V does"),
            (335, Aux::Blue, "3.35 V doesn't darken it"),
            (329, Aux::Off, "under 3.30 V again"),
        ] {
            torch.adc.voltage = voltage_reading(cv);
            next_check(&mut torch);
            assert_eq!(torch.aux(), aux, "{why}");
            torch.adc.voltage = voltage_reading(if aux == Aux::Off { 420 } else { 300 });
            rest(&mut torch, CHECK_TICKS - 1, why);
            assert_eq!(torch.aux(), aux, "{why}, till the next check");
            torch.adc.voltage = voltage_reading(cv);
        }
    }

    #[test]
    fn charging_a_low_cell_at_rest_brings_blue_back_at_the_next_check() {
        let mut torch = Rig::default();
        torch.adc.voltage = voltage_reading(320);
        next_check(&mut torch);
        assert_eq!(torch.aux(), Aux::Off);
        torch.adc.voltage = voltage_reading(380);
        let ticks = next_check(&mut torch);
        assert!(ticks <= u32::from(CHECK_TICKS + CHECK_AWAKE) + 1, "{ticks}");
        assert_eq!(torch.aux(), Aux::Blue);
    }

    #[test]
    fn a_press_reads_the_cell_for_the_aux_without_waiting_for_a_check() {
        let mut torch = Rig::default();
        run(&mut torch, false, TICKS_PER_SECOND);
        assert_eq!(torch.aux(), Aux::Blue);
        torch.adc.voltage = voltage_reading(320);
        run(&mut torch, true, 4);
        assert_eq!(torch.aux(), Aux::Off, "pressed");
        run(&mut torch, false, 4);
        tap(&mut torch, 1, Event::Click(2));
        assert_eq!(torch.level(), 0);
        torch.adc.voltage = voltage_reading(380);
        tap(&mut torch, 2, Event::Click(2));
        assert_eq!(torch.aux(), Aux::Blue, "a press that does nothing");
    }

    #[test]
    fn readings_with_the_light_on_never_move_the_aux() {
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = voltage_reading(310);
        run(&mut torch, false, 60 * TICKS_PER_SECOND);
        assert_eq!(torch.level(), ON_LEVEL, "over 3.00 V, so no step down");
        tap(&mut torch, 1, Event::Click(1));
        assert_eq!(torch.aux(), Aux::Blue, "as it goes out");
        rest(&mut torch, CHECK_TICKS - 1, "after the light");
        assert_eq!(torch.aux(), Aux::Blue, "till the next check");
        assert_a_check(&mut torch, "five minutes on");
        assert_eq!(torch.aux(), Aux::Off, "which reads it");
        click(&mut torch);
        torch.adc.voltage = voltage_reading(400);
        run(&mut torch, false, 60 * TICKS_PER_SECOND);
        tap(&mut torch, 1, Event::Click(1));
        assert_eq!(torch.aux(), Aux::Off, "as it goes out");
        next_check(&mut torch);
        assert_eq!(torch.aux(), Aux::Blue, "at the next check");
    }

    #[test]
    fn a_check_never_lights_it_or_changes_the_mode() {
        let mut torch = Rig::default();
        for _ in 0..3 * u32::from(CHECK_TICKS) {
            torch.tick(false);
            assert_eq!(torch.level(), 0);
            assert_eq!(torch.torch.ui.mode(), Mode::Off);
            assert_eq!(torch.torch.ui.output_level(), 0);
        }
        tap(&mut torch, 3, Event::Click(3));
        run(&mut torch, false, LOCKED_TICKS);
        assert_eq!(torch.aux(), Aux::Off, "red for five seconds only");
        torch.adc.voltage = voltage_reading(320);
        for _ in 0..3 {
            next_check(&mut torch);
            assert_eq!(torch.torch.ui.mode(), Mode::Lockout);
            assert_eq!(torch.level(), 0);
            assert_eq!(torch.aux(), Aux::Off);
        }
        tap(&mut torch, 1, Event::Click(1));
        for tick in 1..LOCKED_TICKS {
            torch.tick(false);
            assert_eq!(torch.aux(), Aux::Red, "tick {tick}");
        }
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off);
        tap(&mut torch, 3, Event::Click(3));
        run(&mut torch, false, RELEASE_TICKS + 1);
        assert_eq!(torch.torch.ui.mode(), Mode::Off);
        assert_eq!(torch.aux(), Aux::Off);
    }

    #[test]
    fn garbage_at_rest_leaves_the_aux_alone_and_is_no_fault() {
        for garbage in [0, voltage_reading(60), 4 * 4095] {
            let mut torch = Rig::default();
            torch.adc.voltage = voltage_reading(320);
            next_check(&mut torch);
            assert_eq!(torch.aux(), Aux::Off, "{garbage}");
            torch.adc.voltage = garbage;
            for _ in 0..12 {
                next_check(&mut torch);
                assert_eq!(torch.aux(), Aux::Off, "{garbage}");
                assert_eq!(torch.torch.limits.health(), Health::Good, "{garbage}");
            }
            torch.adc.voltage = voltage_reading(380);
            next_check(&mut torch);
            assert_eq!(torch.aux(), Aux::Blue, "{garbage}");
            torch.adc.voltage = garbage;
            for _ in 0..12 {
                next_check(&mut torch);
                assert_eq!(torch.aux(), Aux::Blue, "{garbage}");
                assert_eq!(torch.torch.limits.health(), Health::Good, "{garbage}");
            }
            torch.adc.voltage = voltage_reading(380);
            click(&mut torch);
            assert_eq!(torch.level(), ON_LEVEL, "{garbage}");
            click(&mut torch);
            torch.adc.voltage = garbage;
            run(&mut torch, true, 2 * TICKS_PER_SECOND);
            assert_eq!(torch.torch.limits.health(), Health::Fault, "{garbage}");
        }
    }

    #[test]
    fn the_rest_rows_in_docs_ui_md_end_on_the_aux_they_describe() {
        let rows = [
            ("Flat cell, charged, left alone", Aux::Blue),
            ("Low cell, at rest", Aux::Off),
            ("Low cell, read by a press", Aux::Off),
            ("Low cell, charged at rest", Aux::Blue),
            ("A cell it can't read, at rest", Aux::Off),
        ];
        for (row, aux) in rows {
            let example = example(row);
            let rig = Rig::play(example.actions());
            assert_eq!(rig.aux(), aux, "{row}");
            assert_eq!(rig.torch.limits.health(), Health::Good, "{row}");
        }
        let mut low = Rig::play(example("Low cell, at rest").actions().take(2));
        assert_eq!(low.aux(), Aux::Off, "dark before the 1C");
        low.adc.voltage = voltage_reading(320);
        click(&mut low);
        assert_eq!(low.level(), ON_LEVEL);
        click(&mut low);
        assert_eq!(low.aux(), Aux::Off, "and once it's off again");
        let garbage = Rig::play(example("A cell it can't read, at rest").actions().take(2));
        assert_eq!(garbage.aux(), Aux::Blue, "blue through the garbage");
        assert_eq!(garbage.torch.limits.health(), Health::Good, "no fault yet");
    }

    #[test]
    fn a_check_whose_result_never_comes_still_ends() {
        let mut torch = Rig::default();
        run(&mut torch, false, CHECK_AWAKE + 1);
        torch.adc.voltage = voltage_reading(320);
        next_check(&mut torch);
        assert_eq!(torch.aux(), Aux::Off);
        rest(&mut torch, CHECK_TICKS - 1, "to the next check");
        for _ in 0..CHECK_AWAKE {
            torch.torch.tick(false, None, true, true);
            assert!(!torch.resting());
        }
        torch.torch.tick(false, None, true, true);
        assert!(torch.resting());
        assert_eq!(torch.convert(), None);
        assert_eq!(torch.aux(), Aux::Off);
        assert_eq!(torch.torch.limits.health(), Health::Good);
    }

    #[test]
    fn a_flat_cell_charged_at_rest_is_good_again_at_the_next_check() {
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = voltage_reading(280);
        run(&mut torch, false, 10 * TICKS_PER_SECOND);
        assert_eq!(torch.torch.limits.health(), Health::Flat);
        torch.adc.voltage = voltage_reading(350);
        for _ in 0..3 {
            next_check(&mut torch);
            assert_eq!(torch.torch.limits.health(), Health::Flat);
            assert_eq!(torch.aux(), Aux::Off, "yellow only after a press");
        }
        torch.adc.voltage = voltage_reading(380);
        let ticks = next_check(&mut torch);
        assert!(ticks <= u32::from(CHECK_TICKS + CHECK_AWAKE) + 1, "{ticks}");
        assert_eq!(torch.torch.limits.health(), Health::Good, "with no press");
        assert_eq!(torch.aux(), Aux::Blue);
        assert_eq!(torch.level(), 0, "and still off");
        click(&mut torch);
        assert_eq!(torch.level(), ON_LEVEL);
    }

    #[test]
    fn a_fault_skips_the_checks() {
        let mut torch = Rig::new(Faults {
            hang: true,
            ..Faults::NONE
        });
        for tick in 0..2 * u32::from(CHECK_TICKS) {
            torch.tick(false);
            assert!(torch.resting(), "tick {tick}");
            assert_eq!(torch.convert(), None, "tick {tick}");
        }
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = 0;
        run(&mut torch, false, 5 * TICKS_PER_SECOND);
        assert_eq!(torch.torch.limits.health(), Health::Fault);
        assert!(torch.resting());
        for tick in 0..2 * u32::from(CHECK_TICKS) {
            torch.tick(false);
            assert!(torch.resting(), "tick {tick}");
        }
    }

    /// 16 ticks is one ADC round, so each tick of it ends one of these holds.
    #[test]
    fn a_conversion_running_as_a_hold_ends_is_read_before_it_rests() {
        let (mut read, mut not) = (0, 0);
        for down in HOLD_TICKS + 1..HOLD_TICKS + 17 {
            let mut torch = Rig::default();
            run(&mut torch, false, CHECK_AWAKE + 1);
            torch.adc.voltage = voltage_reading(320);
            run(&mut torch, true, down - 1);
            assert_eq!(torch.aux(), Aux::Off, "{down}");
            torch.adc.voltage = voltage_reading(380);
            torch.tick(true);
            let running = torch
                .convert()
                .is_some_and(|c| c.channel == Channel::Voltage);
            assert_eq!(torch.tick(false), Some(Event::Release { clicks: 0 }));
            for _ in 0..2 {
                if !torch.resting() {
                    torch.tick(false);
                }
            }
            assert!(torch.resting(), "{down}");
            let aux = if running { Aux::Blue } else { Aux::Off };
            assert_eq!(torch.aux(), aux, "{down}");
            read += u32::from(running);
            not += u32::from(!running);
        }
        assert!(read > 0 && not > 0, "{read} read, {not} not");
    }

    #[test]
    fn the_ramp_never_asks_for_more_than_the_ceiling() {
        let mut torch = Rig::default();
        click(&mut torch);
        run(&mut torch, true, 10 * TICKS_PER_SECOND);
        assert_eq!(torch.level(), CEILING);
        assert_eq!(torch.output(), ramp::output(ramp::CEILING));
    }

    #[test]
    fn rests_only_with_the_light_off_and_no_click_counting() {
        let mut torch = Rig::default();
        assert!(torch.resting());
        run(&mut torch, true, 4);
        assert!(!torch.resting(), "pressed");
        run(&mut torch, false, RELEASE_TICKS);
        assert!(!torch.resting(), "a click waiting to count");
        run(&mut torch, false, 1);
        assert!(!torch.resting(), "on");
        click(&mut torch);
        assert!(torch.resting());
    }

    #[test]
    fn a_locked_torch_rests() {
        let mut torch = Rig::default();
        clicks(&mut torch, 3);
        run(&mut torch, false, RELEASE_TICKS + 1);
        assert!(torch.resting());
    }

    #[test]
    fn the_aux_leds_follow_the_ui() {
        let mut torch = Rig::default();
        assert_eq!(torch.aux(), Aux::Blue);
        click(&mut torch);
        assert_eq!(torch.aux(), Aux::Off);
    }

    #[test]
    fn a_flat_cell_turns_off_shows_yellow_and_ignores_clicks_until_charged() {
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = voltage_reading(280);
        run(&mut torch, false, 10 * TICKS_PER_SECOND);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.ui.output_level(), 0, "the UI's off too");
        assert!(torch.resting());
        run(&mut torch, false, SHOW_TICKS);
        assert_eq!(torch.aux(), Aux::Off);
        click(&mut torch);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.aux(), Aux::Yellow);
        run(&mut torch, false, SHOW_TICKS);
        assert_eq!(torch.aux(), Aux::Off);
        run(&mut torch, true, TICKS_PER_SECOND);
        run(&mut torch, false, RELEASE_TICKS + 1);
        clicks(&mut torch, 3);
        run(&mut torch, false, RELEASE_TICKS + 1);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.aux(), Aux::Yellow, "not locked out");
        torch.adc.voltage = voltage_reading(380);
        click(&mut torch);
        assert_eq!(torch.level(), ON_LEVEL);
        click(&mut torch);
        assert_eq!(torch.aux(), Aux::Blue);
    }

    #[test]
    fn going_flat_shows_yellow_for_the_three_seconds_after() {
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = voltage_reading(280);
        while torch.level() != 0 {
            torch.tick(false);
        }
        for _ in 0..SHOW_TICKS {
            assert_eq!(torch.aux(), Aux::Yellow);
            torch.tick(false);
        }
        assert_eq!(torch.aux(), Aux::Off);
    }

    #[test]
    fn a_cell_that_collapses_goes_out_at_once_and_is_flat_until_it_reads_charged() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.voltage = voltage_reading(battery::COLLAPSE_BELOW - 1);
        let mut ticks = 0;
        while torch.level() == CEILING {
            torch.tick(false);
            ticks += 1;
        }
        assert!(ticks <= 12, "{ticks} ticks");
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.output(), Output::OFF);
        assert_eq!(torch.ui.output_level(), 0, "the UI's off too");
        assert_eq!(torch.torch.limits.health(), Health::Flat);
        assert!(torch.resting());
        assert_eq!(torch.convert(), None);
        for _ in 0..SHOW_TICKS {
            assert_eq!(torch.aux(), Aux::Yellow);
            torch.tick(false);
        }
        assert_eq!(torch.aux(), Aux::Off);
        click(&mut torch);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.aux(), Aux::Yellow);
        torch.adc.voltage = voltage_reading(battery::CHARGED_AT - 1);
        click(&mut torch);
        assert_eq!(torch.level(), 0);
        torch.adc.voltage = voltage_reading(battery::CHARGED_AT);
        click(&mut torch);
        assert_eq!(torch.level(), ON_LEVEL);
        assert_eq!(torch.torch.limits.ceilings().0, CEILING);
    }

    #[test]
    fn a_dip_thats_gone_by_the_next_reading_changes_nothing() {
        // 16 ticks is one ADC round.
        for at in 0..16 {
            let mut torch = Rig::default();
            at_the_top(&mut torch);
            run(&mut torch, false, at);
            torch.adc.voltage = voltage_reading(250);
            torch.tick(false);
            torch.adc.voltage = voltage_reading(380);
            run(&mut torch, false, 10 * TICKS_PER_SECOND);
            assert_eq!(torch.level(), CEILING, "at {at}");
            assert_eq!(torch.torch.limits.health(), Health::Good, "at {at}");
        }
    }

    #[test]
    fn the_stuck_adc_rows_in_docs_ui_md_end_as_they_describe() {
        for row in [
            "An ADC stuck on one reading",
            "An ADC stuck on one reading, at rest",
        ] {
            let example = example(row);
            let rig = Rig::play(example.actions());
            assert_eq!(rig.torch.limits.health(), Health::Fault, "{row}");
            let mut levels = Vec::new();
            Rig::play_with(example.actions(), |rig, _| levels.push(rig.level()));
            let lit: Vec<u8> = levels.iter().copied().filter(|&l| l > 0).collect();
            assert_eq!(lit.last(), Some(&1), "{row}");
        }
        let rest = example("An ADC stuck on one reading, at rest");
        let rig = Rig::play(rest.actions().take(2));
        assert_eq!(rig.torch.limits.health(), Health::Good);
        assert_eq!(rig.aux(), Aux::Blue);
    }

    #[test]
    fn a_stuck_adc_at_rest_is_never_a_fault() {
        let mut torch = Rig::default();
        run(&mut torch, false, CHECK_AWAKE + 1);
        torch.adc.stuck(voltage_reading(380));
        for _ in 0..12 {
            next_check(&mut torch);
            assert_eq!(torch.torch.limits.health(), Health::Good);
            assert_eq!(torch.aux(), Aux::Blue);
        }
        run(&mut torch, true, TICKS_PER_SECOND);
        assert_eq!(torch.torch.limits.health(), Health::Fault);
    }

    #[test]
    fn the_collapse_rows_in_docs_ui_md_end_as_they_describe() {
        let rows = [
            ("A cell that collapses", Health::Flat, Aux::Yellow),
            ("A collapsed cell, 1C again", Health::Flat, Aux::Yellow),
            (
                "A collapsed cell that rests at 3.60 V or more, 1C",
                Health::Good,
                Aux::Off,
            ),
            (
                "A dip under 2.80 V that's gone by the next reading",
                Health::Good,
                Aux::Off,
            ),
        ];
        for (row, health, aux) in rows {
            let example = example(row);
            let rig = Rig::play(example.actions());
            assert_eq!(rig.torch.limits.health(), health, "{row}");
            assert_eq!(rig.aux(), aux, "{row}");
        }
        // The example's wait is 16 ticks, and a collapse takes up to 12.
        let collapses = example("A cell that collapses");
        let mut levels = Vec::new();
        Rig::play_with(collapses.actions(), |rig, _| levels.push(rig.level()));
        let lit: Vec<u8> = levels.iter().copied().filter(|&l| l > 0).collect();
        assert_eq!(lit.last(), Some(&CEILING), "no fade");
        let dark = levels.iter().rev().take_while(|&&l| l == 0).count();
        assert!(dark > 16 - 12, "{dark} ticks dark at the end");
    }

    #[test]
    fn the_pass_that_turns_the_light_off_has_already_rested() {
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = voltage_reading(280);
        while torch.level() != 0 {
            torch.tick(false);
        }
        assert!(torch.resting());
        assert_eq!(
            torch.convert(),
            None,
            "nothing's left to read in power-down"
        );
    }

    #[test]
    fn a_cell_it_cant_read_fades_it_out_and_shows_white() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.voltage = 0;
        run(&mut torch, false, TICKS_PER_SECOND);
        assert!(torch.level() > 0 && torch.level() < CEILING);
        assert_eq!(torch.aux(), Aux::Off, "out while the main LED's lit");
        run(&mut torch, false, 3 * TICKS_PER_SECOND);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.aux(), Aux::White);
        run(&mut torch, false, SHOW_TICKS);
        assert_eq!(torch.aux(), Aux::Off);
        torch.adc.voltage = voltage_reading(380);
        click(&mut torch);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.aux(), Aux::White);
    }

    #[test]
    fn a_click_still_turns_off_a_fault_thats_fading() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.voltage = 0;
        while torch.torch.limits.health() == Health::Good {
            torch.tick(false);
        }
        run(&mut torch, true, 4);
        assert!(torch.level() > 0, "still fading when the click counts");
        assert_eq!(torch.tick(false), Some(Event::Click(1)), "as it's let go");
        assert_eq!(torch.level(), 0);
    }

    #[test]
    fn the_aux_says_why_for_three_seconds_to_the_tick() {
        const THREE_SECONDS: u16 = 3 * 64;
        assert_eq!(TICKS_PER_SECOND, 64);
        let mut torch = Rig::new(Faults {
            hang: true,
            ..Faults::NONE
        });
        for tick in 1..THREE_SECONDS {
            torch.tick(false);
            assert_eq!(torch.aux(), Aux::White, "tick {tick}");
        }
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off);
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = voltage_reading(280);
        run(&mut torch, false, 20 * TICKS_PER_SECOND);
        assert_eq!(torch.aux(), Aux::Off);
        run(&mut torch, true, 4);
        run(&mut torch, false, RELEASE_TICKS);
        assert_eq!(torch.aux(), Aux::Off, "not before the click counts");
        assert_eq!(torch.tick(false), Some(Event::Click(1)));
        assert_eq!(torch.aux(), Aux::Yellow);
        for tick in 1..THREE_SECONDS {
            torch.tick(false);
            assert_eq!(torch.aux(), Aux::Yellow, "tick {tick}");
        }
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off);
    }

    #[test]
    fn a_fault_that_fades_out_shows_white_for_the_three_seconds_after() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.voltage = 0;
        while torch.torch.limits.health() == Health::Good {
            torch.tick(false);
        }
        let mut fade = 0;
        while torch.level() != 0 {
            assert_eq!(torch.aux(), Aux::Off, "out while the main LED's lit");
            torch.tick(false);
            fade += 1;
        }
        assert!(fade > TICKS_PER_SECOND, "a fade of {fade} ticks");
        for tick in 1..SHOW_TICKS {
            assert_eq!(torch.aux(), Aux::White, "tick {tick}");
            torch.tick(false);
        }
        assert_eq!(torch.aux(), Aux::White);
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off);
    }

    #[test]
    fn a_fault_clicked_off_mid_fade_shows_white_for_the_three_seconds_after() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.voltage = 0;
        while torch.torch.limits.health() == Health::Good {
            torch.tick(false);
        }
        run(&mut torch, false, TICKS_PER_SECOND);
        run(&mut torch, true, 4);
        assert!(torch.level() > 0, "still fading when the click counts");
        assert_eq!(torch.tick(false), Some(Event::Click(1)));
        assert_eq!(torch.level(), 0);
        for tick in 1..SHOW_TICKS {
            assert_eq!(torch.aux(), Aux::White, "tick {tick}");
            torch.tick(false);
        }
        assert_eq!(torch.aux(), Aux::White);
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off);
    }

    #[test]
    fn heat_lowers_the_light_and_leaves_the_ui_where_it_was() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.temperature = temperature_reading(thermal::celsius(80));
        run(&mut torch, false, 120 * TICKS_PER_SECOND);
        assert_eq!(torch.level(), thermal::FLOOR);
        assert_eq!(torch.output(), ramp::output(torch.level()));
        assert_eq!(torch.ui.output_level(), CEILING);
    }

    #[test]
    fn heat_moves_the_light_a_code_or_two_at_a_time() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.temperature = temperature_reading(thermal::LIMIT + 1);
        let (mut last, mut between) = (torch.output(), 0_u32);
        for _ in 0..30 * TICKS_PER_SECOND {
            torch.tick(false);
            let now = torch.output();
            if (now.paths, now.vref) == (last.paths, last.vref) {
                assert!(now.dac.abs_diff(last.dac) <= 2, "{last:?} → {now:?}");
            }
            between += u32::from(now != ramp::output(torch.level()));
            last = now;
        }
        assert!(torch.level() < CEILING - 10, "level {}", torch.level());
        assert!(between > 10 * u32::from(TICKS_PER_SECOND), "{between}");
    }

    #[test]
    fn a_ramp_by_hand_while_hot_still_moves_a_level_a_tick() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        torch.adc.temperature = temperature_reading(thermal::LIMIT + 1);
        run(&mut torch, false, 10 * TICKS_PER_SECOND);
        let held = torch.level();
        assert!(held < CEILING);
        let mut moved = 0_u32;
        for _ in 0..2 * TICKS_PER_SECOND {
            let before = torch.level();
            torch.tick(true);
            if torch.ui.output_level() < CEILING {
                assert_eq!(torch.output(), ramp::output(torch.level()));
                moved += u32::from(torch.level() != before);
            }
        }
        assert!(moved > u32::from(TICKS_PER_SECOND), "{moved}");
    }

    #[test]
    fn a_low_cell_still_moves_the_light_a_level_at_a_time() {
        let mut torch = Rig::default();
        click(&mut torch);
        torch.adc.voltage = voltage_reading(295);
        let mut moved = 0_u32;
        for _ in 0..20 * TICKS_PER_SECOND {
            let before = torch.level();
            torch.tick(false);
            assert_eq!(torch.output(), ramp::output(torch.level()));
            moved += u32::from(torch.level() != before);
        }
        assert!(moved > 10, "{moved}");
    }

    #[test]
    fn a_switch_held_from_power_on_stays_off() {
        let mut torch = Rig::default();
        for _ in 0..10 * 60 * u32::from(TICKS_PER_SECOND) {
            torch.tick(true);
            assert_eq!(torch.level(), 0);
        }
    }

    #[test]
    fn a_switch_held_while_on_still_answers_to_heat() {
        let mut torch = Rig::default();
        click(&mut torch);
        run(&mut torch, true, 10 * TICKS_PER_SECOND);
        assert_eq!(torch.level(), CEILING);
        torch.adc.temperature = temperature_reading(thermal::celsius(80));
        assert!(thermal::celsius(80) > thermal::LIMIT);
        for _ in 0..120 * TICKS_PER_SECOND {
            torch.tick(true);
        }
        assert_eq!(torch.level(), thermal::FLOOR);
        assert_eq!(torch.ui.output_level(), CEILING, "still held at the top");
    }

    #[test]
    fn a_fault_never_lights_and_shows_white_after_power_on_and_a_press() {
        for faults in [
            Faults {
                flash: true,
                ..Faults::NONE
            },
            Faults {
                fuses: true,
                ..Faults::NONE
            },
            Faults {
                hang: true,
                ..Faults::NONE
            },
            Faults {
                supply: true,
                ..Faults::NONE
            },
        ] {
            let mut torch = Rig::new(faults);
            assert_eq!(torch.aux(), Aux::White, "{faults:?}");
            run(&mut torch, false, SHOW_TICKS);
            assert_eq!(torch.aux(), Aux::Off, "{faults:?}");
            click(&mut torch);
            assert_eq!(torch.level(), 0, "{faults:?}");
            assert_eq!(torch.aux(), Aux::White, "{faults:?}");
            run(&mut torch, false, SHOW_TICKS);
            run(&mut torch, true, 30 * TICKS_PER_SECOND);
            assert_eq!(torch.level(), 0, "{faults:?}");
            assert_eq!(torch.aux(), Aux::Off, "{faults:?}");
        }
    }

    #[test]
    fn vddio2_lost_with_the_light_on_puts_it_out_on_that_tick_and_shows_white() {
        let mut torch = Rig::default();
        at_the_top(&mut torch);
        assert_eq!(torch.output(), ramp::output(CEILING));
        torch.vddio2 = false;
        torch.tick(false);
        assert_eq!(torch.level(), 0, "no fade");
        assert_eq!(torch.output(), Output::OFF);
        assert_eq!(torch.torch.limits.health(), Health::Fault);
        assert_eq!(torch.ui.mode(), Mode::Off, "the UI's off too");
        assert!(torch.resting(), "and it rests from that pass");
        for tick in 1..SHOW_TICKS {
            assert_eq!(torch.aux(), Aux::White, "tick {tick}");
            torch.tick(false);
        }
        assert_eq!(torch.aux(), Aux::White);
        torch.tick(false);
        assert_eq!(torch.aux(), Aux::Off);
        for back in [false, true] {
            torch.vddio2 = back;
            click(&mut torch);
            assert_eq!(torch.level(), 0, "VDDIO2 back: {back}");
            assert_eq!(torch.output(), Output::OFF);
            assert_eq!(torch.aux(), Aux::White, "VDDIO2 back: {back}");
            run(&mut torch, true, 2 * TICKS_PER_SECOND);
            run(&mut torch, false, SHOW_TICKS);
            assert_eq!(torch.level(), 0, "VDDIO2 back: {back}");
        }
        assert_eq!(torch.torch.limits.health(), Health::Fault);
    }

    #[test]
    fn vddio2_lost_puts_it_out_from_every_level_and_from_starting() {
        for held in 0..3 * TICKS_PER_SECOND {
            let mut torch = Rig::default();
            click(&mut torch);
            run(&mut torch, true, held);
            let level = torch.level();
            assert!(level >= ON_LEVEL, "{level}");
            torch.vddio2 = false;
            torch.tick(held > 0);
            assert_eq!(torch.level(), 0, "from {level}");
            assert_eq!(torch.output(), Output::OFF, "from {level}");
        }
        let mut torch = Rig::default();
        run(&mut torch, true, 4);
        run(&mut torch, false, RELEASE_TICKS);
        torch.tick(false);
        assert_eq!(torch.output(), Output::STARTING);
        torch.vddio2 = false;
        torch.tick(false);
        assert_eq!(torch.output(), Output::OFF, "never got its level");
        assert_eq!(torch.torch.limits.health(), Health::Fault);
    }

    #[test]
    fn a_click_on_the_tick_vddio2_reads_missing_lights_nothing() {
        let mut torch = Rig::default();
        run(&mut torch, true, 4);
        run(&mut torch, false, RELEASE_TICKS);
        torch.vddio2 = false;
        assert_eq!(torch.tick(false), Some(Event::Click(1)));
        assert_eq!(torch.output(), Output::OFF);
        assert_eq!(torch.ui.mode(), Mode::Off);
        assert_eq!(torch.torch.limits.health(), Health::Fault);
        assert_eq!(torch.aux(), Aux::White);
        torch.vddio2 = true;
        for _ in 0..TICKS_PER_SECOND {
            torch.tick(false);
            assert_eq!(torch.output(), Output::OFF);
        }
    }

    #[test]
    fn vddio2_lost_and_back_at_rest_is_nothing() {
        let mut torch = Rig::default();
        run(&mut torch, false, TICKS_PER_SECOND);
        torch.vddio2 = false;
        rest(&mut torch, 10 * TICKS_PER_SECOND, "with VDDIO2 gone");
        assert_eq!(torch.torch.limits.health(), Health::Good);
        assert_eq!(torch.aux(), Aux::Blue);
        torch.vddio2 = true;
        click(&mut torch);
        assert_eq!(torch.level(), ON_LEVEL);
    }

    #[test]
    fn vddio2_lost_at_rest_is_a_fault_at_the_next_press_before_its_click_counts() {
        let mut torch = Rig::default();
        run(&mut torch, false, TICKS_PER_SECOND);
        torch.vddio2 = false;
        rest(&mut torch, TICKS_PER_SECOND, "with VDDIO2 gone");
        assert_eq!(torch.tick(true), None);
        assert_eq!(torch.torch.limits.health(), Health::Fault, "at the press");
        assert_eq!(torch.aux(), Aux::White);
        run(&mut torch, true, 3);
        for _ in 0..RELEASE_TICKS + 1 {
            torch.tick(false);
            assert_eq!(torch.output(), Output::OFF);
        }
        assert_eq!(torch.ui.mode(), Mode::Off, "the click never reached the UI");
    }

    #[test]
    fn vddio2_lost_at_rest_is_a_fault_at_the_next_check_of_the_cell() {
        let mut torch = Rig::default();
        run(&mut torch, false, CHECK_AWAKE + 1);
        torch.vddio2 = false;
        rest(&mut torch, CHECK_TICKS - 1, "to the next check");
        assert_eq!(torch.torch.limits.health(), Health::Good);
        torch.tick(false);
        assert_eq!(torch.torch.limits.health(), Health::Fault);
        assert_eq!(torch.aux(), Aux::White, "from the check");
        run(&mut torch, false, CHECK_AWAKE);
        assert!(torch.resting());
        run(&mut torch, false, SHOW_TICKS);
        assert_eq!(torch.aux(), Aux::Off);
        for tick in 0..2 * u32::from(CHECK_TICKS) {
            torch.tick(false);
            assert!(torch.resting(), "tick {tick}");
        }
    }

    #[test]
    fn vddio2_lost_in_lockout_is_a_fault_at_the_next_press() {
        let mut torch = Rig::default();
        tap(&mut torch, 3, Event::Click(3));
        run(&mut torch, false, LOCKED_TICKS);
        torch.vddio2 = false;
        rest(&mut torch, TICKS_PER_SECOND, "locked, with VDDIO2 gone");
        torch.tick(true);
        assert_eq!(torch.torch.limits.health(), Health::Fault);
        assert_eq!(torch.aux(), Aux::White);
        assert_eq!(torch.level(), 0);
    }

    #[test]
    fn the_vddio2_rows_in_docs_ui_md_end_as_they_describe() {
        let rows = [
            ("VDDIO2 missing at power-on", Health::Fault, Aux::White),
            ("VDDIO2 lost with the light on", Health::Fault, Aux::White),
            ("VDDIO2 lost, then back", Health::Fault, Aux::White),
            ("VDDIO2 lost with the light off", Health::Fault, Aux::White),
            (
                "VDDIO2 lost and back while it rests",
                Health::Good,
                Aux::Off,
            ),
        ];
        for (row, health, aux) in rows {
            let example = example(row);
            let rig = Rig::play(example.actions());
            assert_eq!(rig.torch.limits.health(), health, "{row}");
            assert_eq!(rig.aux(), aux, "{row}");
        }
        let lost = example("VDDIO2 lost with the light on");
        let (mut last_lit, mut dark_after) = (0, 0);
        Rig::play_with(lost.actions(), |rig, _| {
            if rig.level() > 0 {
                (last_lit, dark_after) = (rig.level(), 0);
            } else {
                dark_after += 1;
            }
        });
        assert_eq!(last_lit, CEILING, "out from the top in one tick");
        assert_eq!(dark_after, 16, "for all of the 250 ms");
    }

    #[test]
    fn a_resting_torch_reads_nothing_and_a_press_starts_the_adc() {
        let mut torch = Rig::default();
        run(&mut torch, false, 64);
        assert!(torch.resting());
        assert_eq!(torch.convert(), None);
        torch.tick(true);
        assert!(!torch.resting());
        assert!(torch.convert().is_some_and(|convert| convert.setup));
    }

    #[test]
    fn still_turns_off_after_hours_on() {
        let mut torch = Rig::default();
        click(&mut torch);
        // Past where button::Clicks' tick count saturates.
        for _ in 0..4 {
            run(&mut torch, false, u16::MAX);
        }
        assert_eq!(torch.output(), dim());
        click(&mut torch);
        assert_eq!(torch.output(), Output::OFF);
    }

    fn between(rng: &mut Rng, low: u16, high: u16) -> u16 {
        low + rng.next_u16() % (high - low + 1)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Pace {
        /// Rests and changes of cell of half a minute at most.
        Quick,
        /// Some rests and changes of cell last minutes, for checks at rest.
        Slow,
    }

    fn span(rng: &mut Rng, pressed: bool, pace: Pace) -> u16 {
        let roll = rng.next_u16() % 16;
        if pressed {
            match roll {
                0..=8 => between(rng, 1, 8),
                9..=10 => between(rng, HOLD_TICKS - 3, HOLD_TICKS + 3),
                _ => between(rng, HOLD_TICKS, 4 * TICKS_PER_SECOND),
            }
        } else {
            match roll {
                12..=15 if pace == Pace::Slow => between(rng, CHECK_TICKS / 4, 3 * CHECK_TICKS),
                0..=6 => between(rng, 1, 8),
                7..=8 => between(rng, RELEASE_TICKS - 3, RELEASE_TICKS + 3),
                9..=13 => between(rng, RELEASE_TICKS + 1, TICKS_PER_SECOND),
                _ => between(rng, TICKS_PER_SECOND, 30 * TICKS_PER_SECOND),
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Source {
        Steady(u16),
        /// One in four anything at all.
        Glitchy(u16),
        /// Anything at all, half of it more than the ADC can read.
        Garbage,
    }

    impl Source {
        fn read(self, rng: &mut Rng) -> u16 {
            match self {
                Self::Steady(raw) => raw,
                Self::Glitchy(raw) if rng.next_u16() % 4 != 0 => raw,
                Self::Glitchy(_) | Self::Garbage => match rng.next_u16() % 2 {
                    0 => rng.next_u16(),
                    _ => rng.next_u16() % (4 * 4095 + 1),
                },
            }
        }

        fn cell(rng: &mut Rng) -> Self {
            match rng.next_u16() % 16 {
                0..=5 => Self::Steady(voltage_reading(between(rng, 340, 420))),
                6..=7 => Self::Steady(voltage_reading(between(rng, 300, 339))),
                8..=9 => Self::Steady(voltage_reading(between(rng, 291, 299))),
                10 => Self::Steady(voltage_reading(between(rng, 280, 289))),
                11 => Self::Steady(voltage_reading(between(rng, 150, 279))),
                12 => Self::Steady(match rng.next_u16() % 3 {
                    0 => 0,
                    1 => voltage_reading(60),
                    _ => 4 * 4095,
                }),
                13..=14 => Self::Glitchy(voltage_reading(between(rng, 300, 420))),
                _ => Self::Garbage,
            }
        }

        fn head(rng: &mut Rng) -> Self {
            let celsius = |degrees: u16| temperature_reading(thermal::celsius(degrees as u8));
            match rng.next_u16() % 8 {
                0..=3 => Self::Steady(celsius(between(rng, 0, 45))),
                4..=5 => Self::Steady(celsius(between(rng, 55, 95))),
                6 => Self::Glitchy(celsius(between(rng, 20, 95))),
                _ => Self::Garbage,
            }
        }
    }

    #[derive(Debug)]
    struct Model {
        mode: Mode,
        asked: u8,
        /// Whether the last 1H since the click on ramped up.
        up: bool,
        /// The tick the aux stops showing yellow or white.
        dark_at: u32,
        red_until: u32,
        cell_low: bool,
        /// `0` meaning due.
        check_in: u16,
        /// `0` for none yet.
        checked_at: u32,
        lit_for: u32,
        /// Plausible readings in a row under 2.80 V with the light on.
        sagging: u8,
    }

    impl Model {
        /// Returns whether it's a collapse, which puts the light out.
        fn loaded(&mut self, raw: u16) -> bool {
            let volts = battery::centivolts(raw);
            if self.lit_for <= 2 || !battery::PLAUSIBLE.contains(&volts) {
                return false;
            }
            self.sagging = if volts < 280 { self.sagging + 1 } else { 0 };
            if self.sagging < 2 {
                return false;
            }
            self.sagging = 0;
            true
        }
    }

    impl Model {
        fn unloaded(&mut self, raw: u16) {
            let volts = battery::centivolts(raw);
            if !battery::PLAUSIBLE.contains(&volts) {
                return;
            }
            if volts < 330 {
                self.cell_low = true;
            } else if volts >= 340 {
                self.cell_low = false;
            }
        }
    }

    impl Model {
        fn handle(&mut self, event: Event) {
            match (self.mode, event) {
                (Mode::Off, Event::Click(1)) => {
                    self.mode = Mode::On;
                    self.asked = ON_LEVEL;
                    self.up = false;
                }
                (Mode::Off, Event::Click(3)) => self.mode = Mode::Lockout,
                (Mode::Lockout, Event::Click(3)) | (Mode::On, Event::Click(1)) => {
                    self.mode = Mode::Off;
                }
                (Mode::On, Event::Hold { clicks: 0, ticks }) => {
                    self.up ^= ticks == 0;
                    self.asked = match self.up {
                        true => (self.asked + 1).min(CEILING),
                        false => (self.asked - 1).max(ui::FLOOR),
                    };
                }
                _ => {}
            }
        }
    }

    #[derive(Debug, Default)]
    struct Seen {
        lit: u32,
        at_the_top: u32,
        at_the_bottom: u32,
        held_down_by_the_cell: u32,
        held_down_by_heat: u32,
        locked: u32,
        resting: u32,
        flat: u32,
        faulted: u32,
        fading_out: u32,
        between_levels: u32,
        failed_at_power_on: u32,
        blue: u32,
        red: u32,
        yellow: u32,
        white: u32,
        results_lost: u32,
        turned_on: u32,
        went_flat: u32,
        collapsed: u32,
        collapsed_mid_fade: u32,
        charged: u32,
        went_blind: u32,
        clicked_off_mid_fade: u32,
        ignored_presses: u32,
        blue_dark_for_the_cell: u32,
        darkened_by_a_press: u32,
        checks: u32,
        darkened_at_a_check: u32,
        relit_at_a_check: u32,
        garbage_at_a_check: u32,
        charged_at_a_check: u32,
        vddio2_missing_unseen: u32,
        cut_while_lit: u32,
        cut_by_a_press: u32,
        cut_at_a_check: u32,
    }

    /// `supply` is its own generator so VDDIO2 doesn't reshuffle the rest.
    fn live(
        rng: &mut Rng,
        supply: &mut Rng,
        pace: Pace,
        faults: Faults,
        ticks: u32,
        seen: &mut Seen,
        life: u32,
    ) -> u32 {
        macro_rules! check {
            ($t:expr, $cond:expr, $($why:tt)+) => {
                assert!($cond, "life {life}, tick {}: {}", $t, format_args!($($why)+))
            };
        }
        let failed = faults.any();
        let mut rig = Rig::new(faults);
        let mut model = Model {
            mode: Mode::Off,
            asked: 0,
            up: false,
            dark_at: if failed { SHOW_TICKS.into() } else { 0 },
            red_until: 0,
            cell_low: false,
            check_in: 0,
            checked_at: 0,
            lit_for: 0,
            sagging: 0,
        };
        let start_up = pace == Pace::Slow && rng.next_u16().is_multiple_of(2);
        let (mut pressed, mut switch_left) = (start_up, 0);
        let (mut cell, mut head) = (Source::Steady(6140), Source::Steady(8603));
        // One result in `lose` never arrives, and `0` is none of them.
        let (mut lose, mut world_left) = (0, 0);
        let (mut up, mut faulted_for) = (0_u32, 0_u32);
        // Channels started one, two and three ticks ago. A voltage conversion
        // straight after another is a reading.
        let (mut converted, mut converted_before, mut converted_first) = (None, None, None);
        let mut lit_before_last = false;
        let mut gone_left = 0_u16;
        let (mut asked_before, mut shown_before) = (0, Output::OFF);
        for t in 1..=ticks {
            if switch_left == 0 {
                pressed = !pressed;
                switch_left = span(rng, pressed, pace);
            }
            switch_left -= 1;
            if world_left == 0 {
                cell = Source::cell(rng);
                head = Source::head(rng);
                lose = match rng.next_u16() % 16 {
                    0..=11 => 0,
                    12..=14 => 4,
                    _ => 1,
                };
                world_left = match pace {
                    Pace::Slow if rng.next_u16().is_multiple_of(2) => {
                        between(rng, 60 * TICKS_PER_SECOND, 3 * CHECK_TICKS)
                    }
                    _ => between(rng, 1, 30 * TICKS_PER_SECOND),
                };
                if supply.next_u16().is_multiple_of(32) {
                    gone_left = match pace {
                        Pace::Slow if supply.next_u16().is_multiple_of(2) => {
                            between(supply, CHECK_TICKS / 2, 2 * CHECK_TICKS)
                        }
                        _ => between(supply, 1, 3 * TICKS_PER_SECOND),
                    };
                }
            }
            world_left -= 1;
            let vddio2 = gone_left == 0;
            gone_left = gone_left.saturating_sub(1);
            up = if pressed { 0 } else { up + 1 };

            let health_before = rig.torch.limits.health();
            let level_before = rig.level();
            let resting_before = rig.resting();
            let mut result = rig.adc.result();
            if lose != 0 && rng.next_u16().is_multiple_of(lose) {
                seen.results_lost += u32::from(result.take().is_some());
            }
            let event = rig.torch.tick(pressed, result, vddio2, true);
            rig.adc
                .start_with(rig.torch.convert(), |channel| match channel {
                    Channel::Voltage => cell.read(rng),
                    Channel::Temperature => head.read(rng),
                    Channel::Ground => match cell {
                        Source::Garbage => cell.read(rng),
                        _ => GROUND_READING,
                    },
                });
            let health = rig.torch.limits.health();
            let level = rig.level();
            let convert = rig.convert();

            if !vddio2 && (pressed || level_before > 0) {
                check!(t, health == Health::Fault, "{health:?}, VDDIO2 missing");
                check!(t, level == 0, "level {level} with VDDIO2 missing");
                check!(t, rig.output() == Output::OFF, "{:?}", rig.output());
                let good = health_before == Health::Good;
                seen.cut_while_lit += u32::from(level_before > 0);
                seen.cut_by_a_press += u32::from(level_before == 0 && good);
            }

            let mut ignored = false;
            if let Some(event) = event {
                let press = matches!(event, Event::Click(_) | Event::Hold { ticks: 0, .. });
                if health_before == Health::Good || level_before > 0 {
                    model.handle(event);
                    if press && model.mode == Mode::Lockout {
                        model.red_until = t + u32::from(LOCKED_TICKS);
                    }
                } else if press {
                    model.dark_at = t + u32::from(SHOW_TICKS);
                    ignored = true;
                }
            }
            let voltage = Some(Channel::Voltage);
            let read = converted == voltage && converted_before == voltage;
            let mut collapsed = false;
            if model.mode != Mode::On {
                model.sagging = 0;
            } else if read {
                collapsed = model.loaded(result.unwrap_or(0));
            }
            // The first reading of a run is the four-a-second one.
            let at_a_check = model.checked_at != 0 && t == model.checked_at + 2 && up >= 3;
            if read && converted_first != voltage {
                let unloaded = model.mode != Mode::On && level_before == 0 && !lit_before_last;
                if unloaded {
                    let low = model.cell_low;
                    model.unloaded(result.unwrap_or(0));
                    let plausible = battery::plausible(battery::centivolts(result.unwrap_or(0)));
                    if at_a_check {
                        seen.darkened_at_a_check += u32::from(!low && model.cell_low);
                        seen.relit_at_a_check += u32::from(low && !model.cell_low);
                        seen.garbage_at_a_check += u32::from(!plausible);
                    } else {
                        seen.darkened_by_a_press += u32::from(!low && model.cell_low);
                    }
                }
            } else {
                check!(t, !at_a_check, "a check with no reading");
            }
            converted_first = converted_before;
            converted_before = converted;
            converted = convert.map(|c| c.channel);
            lit_before_last = level_before > 0;
            model.lit_for = if level > 0 { model.lit_for + 1 } else { 0 };
            if collapsed {
                check!(t, level == 0, "lit at {level} after a collapse");
                check!(t, health != Health::Good, "{health:?} after a collapse");
                check!(t, level_before > 0, "a collapse with the light out");
                seen.collapsed += u32::from(health == Health::Flat);
                seen.collapsed_mid_fade += u32::from(health == Health::Fault);
            }
            if model.mode == Mode::On && level == 0 {
                check!(t, health != Health::Good, "off with nothing wrong");
                model.mode = Mode::Off;
            }
            if health != Health::Good && (health_before == Health::Good || level_before > 0) {
                model.dark_at = t + u32::from(SHOW_TICKS);
            }
            check!(t, rig.torch.ui.mode() == model.mode, "{model:?}");
            let asked = match model.mode {
                Mode::On => model.asked,
                Mode::Off | Mode::Lockout => 0,
            };
            check!(t, rig.torch.ui.output_level() == asked, "{model:?}");

            check!(t, level <= CEILING, "level {level}");
            let expected = match (level_before, level) {
                (_, 0) => Output::OFF,
                (0, _) => Output::STARTING,
                _ => ramp::output(level),
            };
            let shown = rig.output();
            let (battery, thermal) = rig.torch.limits.ceilings();
            let lit = level > 0 && level_before > 0;
            if lit && asked == asked_before && thermal <= battery {
                let (from, to) = (shown_before.modelled(), expected.modelled());
                let between = from.min(to)..=from.max(to);
                check!(t, shown.enable, "{shown:?}");
                check!(t, between.contains(&shown.modelled()), "level {level}");
                seen.between_levels += u32::from(shown != expected);
            } else {
                check!(t, shown == expected, "level {level}");
            }
            (asked_before, shown_before) = (asked, shown);
            check!(t, level <= asked, "level {level} of {asked}");
            check!(t, (level > 0) == (model.mode == Mode::On), "level {level}");
            check!(t, !failed || level == 0, "lit after a failed check");
            let moved = level.abs_diff(level_before);
            let click = event == Some(Event::Click(1));
            if level_before == 0 && level > 0 {
                check!(t, click, "on without a click: {event:?}");
                check!(t, health_before == Health::Good, "{health_before:?}");
                check!(t, health == Health::Good, "{health:?}");
                check!(t, vddio2, "lit with VDDIO2 missing");
                check!(t, level <= ON_LEVEL, "on at {level}");
                seen.turned_on += 1;
            } else if level_before > 0 && level == 0 {
                check!(
                    t,
                    click || health != Health::Good,
                    "went out with nothing wrong and no click"
                );
            } else {
                check!(t, moved <= 1, "{level_before} to {level}");
            }

            match (health_before, health) {
                (Health::Good, Health::Good)
                | (Health::Flat, Health::Flat)
                | (Health::Fault, Health::Fault) => {}
                (Health::Good, Health::Flat) => {
                    check!(
                        t,
                        level_before == 1 || collapsed,
                        "flat from level {level_before}"
                    );
                    seen.went_flat += u32::from(!collapsed);
                }
                (Health::Flat, Health::Good) => {
                    let volts = result.map(battery::centivolts);
                    check!(
                        t,
                        volts.is_some_and(|v| v >= battery::CHARGED_AT && battery::plausible(v)),
                        "charged by {volts:?}"
                    );
                    seen.charged += 1;
                }
                (Health::Good | Health::Flat, Health::Fault) => seen.went_blind += 1,
                (Health::Fault, _) => check!(t, false, "a fault cleared to {health:?}"),
            }
            check!(t, !failed || health == Health::Fault, "{health:?}");
            if health == Health::Flat {
                check!(t, level == 0, "flat at level {level}");
            }
            if health_before == Health::Fault {
                check!(t, level <= level_before, "{level_before} to {level}");
                if level > 0 {
                    check!(t, moved == 1, "{level_before} to {level}");
                } else if level_before > 0 {
                    let out = click || level_before == 1 || !vddio2 || collapsed;
                    check!(t, out, "out from {level_before}");
                    seen.clicked_off_mid_fade += u32::from(click);
                }
            }

            let aux = rig.aux();
            check!(t, level == 0 || aux == Aux::Off, "{aux:?} at level {level}");
            let showing = t < model.dark_at;
            let expected = match (health, model.mode) {
                _ if level > 0 => Aux::Off,
                (Health::Good, Mode::Off) if !model.cell_low => Aux::Blue,
                (Health::Good, Mode::Lockout) if t < model.red_until => Aux::Red,
                (Health::Flat, _) if showing => Aux::Yellow,
                (Health::Fault, _) if showing => Aux::White,
                _ => Aux::Off,
            };
            check!(t, aux == expected, "{aux:?}, {health:?}, {model:?}");

            let resting = rig.resting();
            if resting {
                check!(t, level == 0, "resting at level {level}");
                check!(t, rig.convert().is_none(), "resting with the ADC busy");
            }
            if resting_before && !pressed {
                check!(t, event.is_none(), "an event with the switch up");
                if model.check_in == 0 && health_before != Health::Fault {
                    let started = Some(Convert {
                        channel: Channel::Voltage,
                        setup: true,
                    });
                    check!(t, !resting && convert == started, "no check, {convert:?}");
                    model.checked_at = t;
                    seen.checks += 1;
                    if !vddio2 {
                        check!(t, health == Health::Fault, "{health:?} at a check");
                        seen.cut_at_a_check += 1;
                    }
                } else {
                    check!(t, resting, "woke with the switch up");
                    check!(t, health == health_before, "{health:?} while resting");
                    seen.vddio2_missing_unseen += u32::from(!vddio2 && health == Health::Good);
                }
            }
            let awake = u32::from(CHECK_AWAKE);
            let checking =
                model.checked_at != 0 && t - model.checked_at <= awake && up > t - model.checked_at;
            if checking {
                check!(t, level == 0, "lit by a check");
                check!(
                    t,
                    health == health_before
                        || (health_before == Health::Flat && health == Health::Good)
                        || (!vddio2 && health == Health::Fault),
                    "a check took it from {health_before:?} to {health:?}"
                );
                let rests = t == model.checked_at + awake;
                check!(t, resting == rests, "four ticks awake, then rest");
                if at_a_check && (health_before, health) == (Health::Flat, Health::Good) {
                    seen.charged_at_a_check += 1;
                }
            }
            model.check_in = match resting {
                true => model.check_in.saturating_sub(1),
                false => CHECK_TICKS,
            };
            if resting_before && pressed {
                check!(t, rig.convert().is_some(), "a press that reads nothing");
            }
            // A running conversion is seen through.
            if up > u32::from(RELEASE_TICKS + CHECK_AWAKE) && level == 0 && !checking {
                check!(t, resting, "nothing to do and not resting");
            }
            seen.blue_dark_for_the_cell +=
                u32::from(health == Health::Good && model.mode == Mode::Off && model.cell_low);

            seen.lit += u32::from(level > 0);
            seen.at_the_top += u32::from(level == CEILING);
            seen.at_the_bottom += u32::from(level == ui::FLOOR && asked == ui::FLOOR);
            if health == Health::Good && level > 0 && level < asked {
                let (cell, heat) = rig.torch.limits.ceilings();
                seen.held_down_by_the_cell += u32::from(level == cell);
                seen.held_down_by_heat += u32::from(level == heat);
            }
            seen.locked += u32::from(model.mode == Mode::Lockout);
            seen.resting += u32::from(resting);
            seen.flat += u32::from(health == Health::Flat);
            seen.faulted += u32::from(health == Health::Fault && !failed);
            seen.fading_out += u32::from(health == Health::Fault && level > 0);
            seen.failed_at_power_on += u32::from(failed);
            seen.blue += u32::from(aux == Aux::Blue);
            seen.red += u32::from(aux == Aux::Red);
            seen.yellow += u32::from(aux == Aux::Yellow);
            seen.white += u32::from(aux == Aux::White);
            seen.ignored_presses += u32::from(ignored);

            // Nothing changes a fault, so half a minute of one is enough.
            faulted_for = if health == Health::Fault {
                faulted_for + 1
            } else {
                0
            };
            if faulted_for > 30 * u32::from(TICKS_PER_SECOND) {
                return t;
            }
        }
        ticks
    }

    fn throw_noise_at_torches(seed: u32, pace: Pace, ticks: u32, seen: &mut Seen) {
        let mut rng = Rng::new(seed);
        let mut supply = Rng::new(seed ^ 0x00AA_0000);
        let (mut life, mut run) = (0, 0);
        while run < ticks {
            let faults = match rng.next_u16() % 8 {
                0 => Faults {
                    hang: true,
                    ..Faults::NONE
                },
                _ => Faults::NONE,
            };
            let mut length = u32::from(between(&mut rng, 1000, 20_000));
            if pace == Pace::Slow {
                length *= 10;
            }
            run += live(&mut rng, &mut supply, pace, faults, length, seen, life);
            life += 1;
        }
    }

    #[test]
    fn random_switching_with_long_rests_never_breaks_a_promise() {
        let mut seen = Seen::default();
        for seed in [0x5EED_0005, 0x5EED_0006] {
            throw_noise_at_torches(seed, Pace::Slow, 4_000_000, &mut seen);
        }
        for (what, times) in [
            ("resting", seen.resting),
            ("blue", seen.blue),
            ("blue dark for the cell", seen.blue_dark_for_the_cell),
            ("checking the cell", seen.checks),
            ("darkened at a check", seen.darkened_at_a_check),
            ("relit at a check", seen.relit_at_a_check),
            ("reading garbage at a check", seen.garbage_at_a_check),
            ("charged at a check", seen.charged_at_a_check),
            ("darkened by a press", seen.darkened_by_a_press),
            ("locked out", seen.locked),
            ("flat", seen.flat),
            ("failed at power-on", seen.failed_at_power_on),
            ("resting through VDDIO2 missing", seen.vddio2_missing_unseen),
            ("cut at a check of the cell", seen.cut_at_a_check),
        ] {
            assert!(times > 0, "never {what}: {seen:?}");
        }
    }

    #[test]
    fn random_switching_and_readings_never_break_a_promise() {
        let mut seen = Seen::default();
        for seed in [0x5EED_0003, 0x5EED_0004] {
            throw_noise_at_torches(seed, Pace::Quick, 1_000_000, &mut seen);
        }
        for (what, times) in [
            ("blue dark for the cell", seen.blue_dark_for_the_cell),
            ("darkened by a press", seen.darkened_by_a_press),
            ("lit", seen.lit),
            ("at the top of the ramp", seen.at_the_top),
            ("at the bottom of the ramp", seen.at_the_bottom),
            ("held down by the cell", seen.held_down_by_the_cell),
            ("held down by heat", seen.held_down_by_heat),
            ("locked out", seen.locked),
            ("resting", seen.resting),
            ("flat", seen.flat),
            ("faulted", seen.faulted),
            ("fading out for a fault", seen.fading_out),
            ("between two levels", seen.between_levels),
            ("failed at power-on", seen.failed_at_power_on),
            ("blue", seen.blue),
            ("red", seen.red),
            ("yellow", seen.yellow),
            ("white", seen.white),
            ("missing a result", seen.results_lost),
            ("turned on", seen.turned_on),
            ("gone flat", seen.went_flat),
            ("collapsed", seen.collapsed),
            ("collapsed in a fault's fade", seen.collapsed_mid_fade),
            ("charged", seen.charged),
            ("unable to read the cell", seen.went_blind),
            ("clicked off mid-fade", seen.clicked_off_mid_fade),
            ("pressed with the light kept out", seen.ignored_presses),
            ("resting through VDDIO2 missing", seen.vddio2_missing_unseen),
            ("cut with the light on", seen.cut_while_lit),
            ("cut by a press", seen.cut_by_a_press),
        ] {
            assert!(times > 0, "never {what}: {seen:?}");
        }
    }

    #[test]
    fn the_first_output_always_counts_as_a_change() {
        assert_eq!(Changes::new().update(Output::OFF), Some(Output::OFF));
        assert_eq!(Changes::new().update(dim()), Some(dim()));
    }

    #[test]
    fn turning_on_and_off_are_changes_and_staying_put_is_not() {
        let mut changes = Changes::new();
        assert_eq!(changes.update(Output::OFF), Some(Output::OFF));
        assert_eq!(changes.update(Output::OFF), None);
        assert_eq!(changes.update(dim()), Some(dim()));
        assert_eq!(changes.update(dim()), None);
        assert_eq!(changes.update(Output::OFF), Some(Output::OFF));
    }
}
