//! The 150 levels, and what each one asks of the Lume X1-C driver. The current
//! is modelled as DAC code × reference × path gain, and nobody has measured it.

use crate::board::{PATH1, PATH2, PATH3};

pub const LEVELS: u8 = 150;

pub const CEILING: u8 = LEVELS;

/// DAC0 is 10-bit (DS40002413B §34.3.2.3).
pub const DAC_MAX: u16 = 1023;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vref {
    /// Specified for VDD from 2.5 V (DS40002413B table 38-18).
    V1024,
    /// Specified for VDD from 2.7 V.
    V2500,
}

impl Vref {
    pub const fn millivolts(self) -> u16 {
        match self {
            Self::V1024 => 1024,
            Self::V2500 => 2500,
        }
    }
}

/// Each includes the ones before it, as the stock firmware has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Paths {
    None,
    Moon,
    Low,
    High,
}

impl Paths {
    pub const fn mask(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Moon => 1 << PATH1,
            Self::Low => 1 << PATH1 | 1 << PATH2,
            Self::High => 1 << PATH1 | 1 << PATH2 | 1 << PATH3,
        }
    }

    const fn gain(self) -> u64 {
        match self {
            Self::None => 0,
            Self::Moon => 1,
            Self::Low => LOW_GAIN,
            Self::High => LOW_GAIN * HIGH_GAIN,
        }
    }
}

/// The low path's current over the moon path's, for the same DAC voltage.
/// Likely about this, from where the stock firmware changes path, and unmeasured.
pub const LOW_GAIN: u64 = 200;

/// The high path's over the low path's. Likely within 15 %, and unmeasured.
pub const HIGH_GAIN: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    /// The boost converter and amplifier's enable, PC1.
    pub enable: bool,
    pub dac: u16,
    pub vref: Vref,
    pub paths: Paths,
}

impl Output {
    pub const OFF: Self = Self {
        enable: false,
        dac: 0,
        vref: Vref::V1024,
        paths: Paths::None,
    };

    /// Sent for one tick before any level, the way the stock firmware waits
    /// 8 ms after raising PC1 before it sets the DAC.
    pub const STARTING: Self = Self {
        enable: true,
        ..Self::OFF
    };

    /// In the model's units, DAC code × reference millivolts × path gain. A
    /// 64-bit multiply, so it must never run on the chip.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "1023 × 2500 × 20,000 is about 5e10, far inside a u64"
    )]
    pub const fn modelled(self) -> u64 {
        if !self.enable {
            return 0;
        }
        widen(self.dac) * widen(self.vref.millivolts()) * self.paths.gain()
    }
}

#[expect(
    clippy::as_conversions,
    reason = "widening in a const fn, where From can't be called"
)]
const fn widen(value: u16) -> u64 {
    value as u64
}

/// Level 1, about the lowest setting the stock firmware uses. Whether lower
/// lights steadily is unmeasured.
pub const FLOOR: Output = Output {
    enable: true,
    dac: 100,
    vref: Vref::V1024,
    paths: Paths::Moon,
};

/// Level 150, the top of the stock firmware's 32 W build, which is the one
/// read off this torch (model 0284).
pub const TOP: Output = Output {
    enable: true,
    dac: 716,
    vref: Vref::V2500,
    paths: Paths::High,
};

/// In order of the current a code asks for, least first.
const GEARS: [(Paths, Vref); 6] = [
    (Paths::Moon, Vref::V1024),
    (Paths::Moon, Vref::V2500),
    (Paths::Low, Vref::V1024),
    (Paths::Low, Vref::V2500),
    (Paths::High, Vref::V1024),
    (Paths::High, Vref::V2500),
];

#[expect(
    clippy::as_conversions,
    reason = "widening u8 to usize for an array length, where From can't be called"
)]
static TABLE: [u16; LEVELS as usize] = table();

const VREF_BIT: u16 = 1 << 10;
const PATHS_SHIFT: u8 = 11;

const _: () = assert!(DAC_MAX < VREF_BIT);
const _: () = assert!(CEILING > 0 && CEILING <= LEVELS);

pub fn output(level: u8) -> Output {
    let Some(index) = level.min(CEILING).checked_sub(1) else {
        return Output::OFF;
    };
    match TABLE.get(usize::from(index)) {
        Some(&packed) => unpack(packed),
        None => Output::OFF,
    }
}

/// Steps between one level and the next.
pub const FRACTION: u16 = 16;

const _: () = assert!(FRACTION.is_power_of_two());

/// `sixteenths` is a level in [`FRACTION`]ths.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a gap of DAC_MAX at most times a part under FRACTION is 15,345 at most"
)]
pub fn output_between(sixteenths: u16) -> Output {
    let level = u8::try_from(sixteenths / FRACTION).unwrap_or(CEILING);
    let low = output(level);
    let part = sixteenths % FRACTION;
    if part == 0 || level == 0 || level >= CEILING {
        return low;
    }
    let high = output(level.saturating_add(1));
    let from = if (high.vref, high.paths) == (low.vref, low.paths) {
        low.dac
    } else if let Some(code) = bridge(level) {
        code
    } else {
        return low;
    };
    let rise = high.dac.saturating_sub(from).min(DAC_MAX) * part / FRACTION;
    Output {
        dac: from.saturating_add(rise),
        ..high
    }
}

static BRIDGES: [Option<(u8, u16)>; GEARS.len() - 1] = bridges();

fn bridge(level: u8) -> Option<u16> {
    BRIDGES
        .iter()
        .flatten()
        .find(|&&(from, _)| from == level)
        .map(|&(_, code)| code)
}

#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "only ever run to build BRIDGES, where an overflow or the assert fails the build, \
              and the code is checked against the next level's before it's narrowed"
)]
const fn bridge_between(low: Output, high: Output) -> Option<u16> {
    if low.paths.mask() != high.paths.mask() || low.vref.millivolts() == high.vref.millivolts() {
        return None;
    }
    let scale = widen(high.vref.millivolts()) * high.paths.gain();
    let code = low.modelled().div_ceil(scale);
    assert!(code >= 1 && code <= widen(high.dac));
    Some(code as u16)
}

#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "only ever run to build BRIDGES, where an overflow or a bad index fails the build, \
              and a level is LEVELS at most, which is a u8"
)]
const fn bridges() -> [Option<(u8, u16)>; GEARS.len() - 1] {
    let table = table();
    let mut bridges = [None; GEARS.len() - 1];
    let (mut found, mut level) = (0, 1);
    while level < LEVELS as usize {
        if let Some(code) = bridge_between(unpack(table[level - 1]), unpack(table[level])) {
            bridges[found] = Some((level as u8, code));
            found += 1;
        }
        level += 1;
    }
    bridges
}

const fn unpack(packed: u16) -> Output {
    Output {
        enable: true,
        dac: packed & DAC_MAX,
        vref: if packed & VREF_BIT != 0 {
            Vref::V2500
        } else {
            Vref::V1024
        },
        paths: match packed >> PATHS_SHIFT {
            1 => Paths::Moon,
            2 => Paths::Low,
            _ => Paths::High,
        },
    }
}

#[expect(
    clippy::as_conversions,
    reason = "only ever run to build TABLE, and From can't be called in a const fn"
)]
const fn pack(output: Output) -> u16 {
    let vref = match output.vref {
        Vref::V1024 => 0,
        Vref::V2500 => VREF_BIT,
    };
    output.dac | vref | (output.paths as u16) << PATHS_SHIFT
}

#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    clippy::panic,
    reason = "only ever run to build TABLE, where an overflow, a bad index or the panic fails \
              the build, and the code is checked against DAC_MAX before it's narrowed"
)]
const fn choose(target: u64, up: bool) -> Output {
    let mut gear = 0;
    while gear < GEARS.len() {
        let (paths, vref) = GEARS[gear];
        let scale = widen(vref.millivolts()) * paths.gain();
        let code = if up {
            target.div_ceil(scale)
        } else {
            (target + scale / 2) / scale
        };
        if code <= DAC_MAX as u64 {
            let dac = if code == 0 { 1 } else { code as u16 };
            return Output {
                enable: true,
                dac,
                vref,
                paths,
            };
        }
        gear += 1;
    }
    panic!("over the driver's full scale")
}

#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::as_conversions,
    reason = "only ever run to build TABLE, where an overflow or a bad index fails the build, \
              and From can't be called in a const fn"
)]
const fn table() -> [u16; LEVELS as usize] {
    let mut table = [0; LEVELS as usize];
    let floor = FLOOR.modelled();
    let span = TOP.modelled() - floor;
    let last = LEVELS as u64 - 1;
    let mut previous = 0;
    let mut i = 0;
    while i < LEVELS as usize {
        let step = i as u64;
        let target = floor + span * step * step * step / (last * last * last);
        let mut level = choose(target, false);
        if level.modelled() <= previous {
            level = choose(previous + 1, true);
        }
        previous = level.modelled();
        table[i] = pack(level);
        i += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> impl Iterator<Item = Output> {
        TABLE.iter().map(|&packed| unpack(packed))
    }

    #[test]
    fn level_zero_is_off() {
        assert_eq!(output(0), Output::OFF);
    }

    #[test]
    fn the_table_runs_from_the_floor_to_the_top() {
        assert_eq!(unpack(TABLE[0]), FLOOR);
        assert_eq!(unpack(TABLE[usize::from(LEVELS) - 1]), TOP);
    }

    #[test]
    fn packing_round_trips() {
        for level in all() {
            assert_eq!(unpack(pack(level)), level);
        }
    }

    #[test]
    fn every_level_asks_for_more_current_than_the_last() {
        for pair in TABLE.windows(2) {
            let (low, high) = (unpack(pair[0]), unpack(pair[1]));
            assert!(low.modelled() < high.modelled(), "{low:?} {high:?}");
        }
    }

    #[test]
    fn every_level_turns_the_driver_on_with_a_code_and_a_path() {
        for level in 1..=LEVELS {
            let out = output(level);
            assert!(out.enable && out.dac > 0 && out.dac <= DAC_MAX);
            assert_ne!(out.paths, Paths::None);
        }
    }

    #[test]
    fn each_level_uses_the_lowest_gear_that_reaches_it() {
        for level in all() {
            let at = GEARS
                .iter()
                .position(|&gear| gear == (level.paths, level.vref))
                .unwrap();
            for &(paths, vref) in &GEARS[..at] {
                let lower = Output {
                    dac: DAC_MAX,
                    paths,
                    vref,
                    ..level
                };
                assert!(lower.modelled() < level.modelled(), "{level:?}");
            }
        }
    }

    #[test]
    fn the_ramp_climbs_through_every_gear() {
        for gear in GEARS {
            assert!(
                all().any(|level| (level.paths, level.vref) == gear),
                "{gear:?}"
            );
        }
    }

    #[test]
    fn nothing_runs_above_the_ceiling() {
        assert_eq!(output(LEVELS), output(CEILING));
        assert_eq!(output(u8::MAX), output(CEILING));
    }

    fn gear(output: Output) -> (Paths, Vref) {
        (output.paths, output.vref)
    }

    #[test]
    fn a_whole_level_between_is_the_level() {
        for level in 0..=LEVELS {
            assert_eq!(output_between(u16::from(level) * FRACTION), output(level));
        }
    }

    #[test]
    fn under_level_one_is_off_and_over_the_ceiling_is_the_ceiling() {
        for sixteenths in 0..FRACTION {
            assert_eq!(output_between(sixteenths), Output::OFF);
        }
        let top = u16::from(CEILING) * FRACTION;
        for sixteenths in [top + 1, top + FRACTION, u16::MAX] {
            assert_eq!(output_between(sixteenths), output(CEILING));
        }
    }

    #[test]
    fn between_two_levels_is_between_them_unless_the_paths_change() {
        let (mut references, mut paths) = (0, 0);
        for level in 1..CEILING {
            let (low, high) = (output(level), output(level + 1));
            references += usize::from(low.paths == high.paths && low.vref != high.vref);
            paths += usize::from(low.paths != high.paths);
            for part in 1..FRACTION {
                let out = output_between(u16::from(level) * FRACTION + part);
                if gear(low) == gear(high) {
                    assert_eq!((out.enable, gear(out)), (true, gear(low)));
                    assert!(low.dac <= out.dac && out.dac < high.dac, "{level} {part}");
                } else if low.paths == high.paths {
                    assert_eq!((out.enable, gear(out)), (true, gear(high)));
                    let between = low.modelled()..=high.modelled();
                    assert!(between.contains(&out.modelled()), "{level} {part}");
                } else {
                    assert_eq!(out, low, "{level} {part}");
                }
            }
        }
        assert_eq!((references, paths), (3, 2));
        assert_eq!(references + paths, GEARS.len() - 1);
    }

    #[test]
    fn just_over_level_125_is_its_current_on_the_next_levels_reference() {
        let (low, high) = (output(125), output(126));
        assert_eq!((low.vref, low.dac), (Vref::V1024, 1008));
        assert_eq!((high.vref, high.dac), (Vref::V2500, 423));
        let out = output_between(125 * FRACTION + 1);
        assert_eq!(out, Output { dac: 413, ..high });
        assert!(out.modelled() - low.modelled() < low.modelled() / 1000);
    }

    #[test]
    fn every_bridge_is_the_lowest_code_that_asks_for_no_less() {
        assert_eq!(
            BRIDGES.map(|bridge| bridge.map(|(level, _)| level)),
            [Some(5), Some(27), Some(125), None, None]
        );
        for (level, code) in BRIDGES.into_iter().flatten() {
            let (low, high) = (output(level), output(level + 1));
            let on_the_next = |dac| Output { dac, ..high }.modelled();
            assert!(on_the_next(code) >= low.modelled(), "{level}");
            assert!(on_the_next(code - 1) < low.modelled(), "{level}");
            assert!(code <= high.dac, "{level}");
        }
        assert_eq!(bridge(0), None);
    }

    #[test]
    fn from_level_50_up_no_sixteenth_is_a_step() {
        let mut last = output_between(50 * FRACTION);
        for sixteenths in 50 * FRACTION + 1..=u16::from(CEILING) * FRACTION {
            let now = output_between(sixteenths);
            if gear(now) == gear(last) {
                assert!(now.dac - last.dac <= 2, "{sixteenths}");
            } else {
                let rise = now.modelled() - last.modelled();
                assert!(rise < last.modelled() / 1000, "{sixteenths}");
            }
            last = now;
        }
    }

    #[test]
    fn between_never_asks_for_less_as_it_climbs() {
        let mut last = 0;
        for sixteenths in FRACTION..=u16::from(CEILING) * FRACTION {
            let now = output_between(sixteenths).modelled();
            assert!(now >= last, "{sixteenths}");
            last = now;
        }
    }

    #[test]
    fn halfway_between_the_top_two_levels_is_halfway_between_their_codes() {
        assert_eq!((output(149).dac, output(150).dac), (702, 716));
        let halfway = 149 * FRACTION + FRACTION / 2;
        assert_eq!(output_between(halfway).dac, 709);
    }

    #[test]
    fn the_ceiling_is_the_stock_firmwares_top() {
        assert_eq!(output(CEILING), TOP);
    }

    #[test]
    fn the_top_is_the_stock_firmwares_and_under_full_scale() {
        let full = Output {
            dac: DAC_MAX,
            ..TOP
        };
        assert_eq!(TOP.modelled() * 1023 / full.modelled(), 716);
    }

    #[test]
    fn off_and_starting_ask_for_nothing() {
        assert_eq!(Output::OFF.modelled(), 0);
        assert_eq!(Output::STARTING.modelled(), 0);
        const { assert!(Output::STARTING.enable) };
        assert_eq!(Output::STARTING.paths, Paths::None);
    }

    #[test]
    fn the_paths_add_up() {
        assert_eq!(Paths::None.mask(), 0);
        assert_eq!(Paths::Moon.mask(), 0x80);
        assert_eq!(Paths::Low.mask(), 0xC0);
        assert_eq!(Paths::High.mask(), 0xE0);
    }
}
