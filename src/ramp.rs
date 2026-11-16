//! The 150 levels, and what each one asks of the Lume X1-C driver.
//!
//! The driver regulates the LED's current to a setpoint, so there's no PWM.
//! Three things set the current (docs/hardware.md has the board).
//!
//! - DAC0's 10-bit code, 0 to 1023, out on PD6 as `code / 1024` of its
//!   reference (DS40002413B section 34.3.2.3).
//! - That reference, `VREF.DAC0REF`, which this firmware runs at 1.024 V or
//!   2.5 V (section 21.5.2).
//! - The power paths, PA7 alone (moon), PA7 and PA6 (low), or all three
//!   (high). Each adds a sense resistor in parallel, so the same DAC voltage
//!   asks for more current.
//!
//! So the current goes as `code × reference × gain`, where the gain is the
//! path's. Nobody has measured the gains. [`LOW_GAIN`] and [`HIGH_GAIN`] are
//! estimates from where the stock firmware changes path, read for their range
//! only, and if they're off the ramp has a step or a dip where it changes
//! path, which is a matter of looks, not of safety. Nothing above the table's
//! top depends on them, since the top is all on the high path.
//!
//! The curve is this firmware's own, a cubic in that modelled current from
//! [`FLOOR`] to [`TOP`], like the TS26 firmware's, worked out at compile time.
//! Each level uses the lowest path and reference that can reach it, so the
//! DAC code sits as high in its range as it can. Where a path change leaves a
//! code under 0x030, the DAC's linearity isn't specified (table 38-23), and
//! its offset error of up to 5 codes is a large share of a code that small,
//! so those few levels could come out a little off the curve. Nobody's
//! measured what any level comes out as in lumens.

use crate::board::{PATH1, PATH2, PATH3};

/// How many levels the ramp has. Level 0 is off.
pub const LEVELS: u8 = 150;

/// The highest level the firmware will run at, until thermal regulation has
/// been checked on a real light.
///
/// Level 120 on this curve is about half of [`TOP`], so about half the power
/// the stock firmware runs this torch at. The TS26 firmware stopped at about
/// half its stock power too. Raise it to [`LEVELS`] once a real head has
/// been measured, and nothing goes past the stock firmware's top even then.
pub const CEILING: u8 = 120;

/// The DAC's biggest code.
pub const DAC_MAX: u16 = 1023;

/// The DAC reference, `VREF.DAC0REF`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vref {
    /// 1.024 V, specified for VDD from 2.5 V (table 38-18).
    V1024,
    /// 2.5 V, specified for VDD from 2.7 V.
    V2500,
}

impl Vref {
    /// The reference in millivolts.
    pub const fn millivolts(self) -> u16 {
        match self {
            Self::V1024 => 1024,
            Self::V2500 => 2500,
        }
    }
}

/// Which power paths are on. Each includes the ones before it, as the stock
/// firmware has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Paths {
    None,
    /// PA7.
    Moon,
    /// PA7 and PA6.
    Low,
    /// PA7, PA6 and PA5.
    High,
}

impl Paths {
    /// The PORTA pins that are high, as a mask.
    pub const fn mask(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Moon => 1 << PATH1,
            Self::Low => 1 << PATH1 | 1 << PATH2,
            Self::High => 1 << PATH1 | 1 << PATH2 | 1 << PATH3,
        }
    }

    /// The modelled gain, relative to the moon path alone.
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
/// Likely about this, from where the stock firmware moves between them, and
/// unmeasured.
pub const LOW_GAIN: u64 = 200;

/// The high path's current over the low path's. Likely about this, from where
/// the stock firmware moves between them, give or take 15 %, and unmeasured.
pub const HIGH_GAIN: u64 = 100;

/// What the main LED's driver should be doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    /// The boost converter and amplifier's enable, PC1.
    pub enable: bool,
    /// DAC0's code, 0 to [`DAC_MAX`].
    pub dac: u16,
    pub vref: Vref,
    pub paths: Paths,
}

impl Output {
    /// Everything off and at its lowest, as the stock firmware turns off.
    pub const OFF: Self = Self {
        enable: false,
        dac: 0,
        vref: Vref::V1024,
        paths: Paths::None,
    };

    /// The boost converter and amplifier on, asked for no current. `Torch`
    /// sends this for one tick before any level, the way the stock firmware
    /// waits 8 ms after raising PC1 before it sets the DAC.
    pub const STARTING: Self = Self {
        enable: true,
        ..Self::OFF
    };

    /// The current this asks for, in the model's units: DAC code times
    /// reference millivolts times path gain. For the table and tests. It's a
    /// 64-bit multiply, so it never runs on the chip.
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

/// Level 1, the moon path at code 100 against 1.024 V. That's about the
/// lowest setting the stock firmware uses. Whether lower lights steadily is
/// unmeasured.
pub const FLOOR: Output = Output {
    enable: true,
    dac: 100,
    vref: Vref::V1024,
    paths: Paths::Moon,
};

/// Level 150, the high path at code 716 against 2.5 V, which is the top of
/// the stock firmware's 32 W build, the one the backup of this torch found
/// (0284). It's 70 % of the driver's full scale.
pub const TOP: Output = Output {
    enable: true,
    dac: 716,
    vref: Vref::V2500,
    paths: Paths::High,
};

/// Every path and reference, from the least current a code asks for to the
/// most.
const GEARS: [(Paths, Vref); 6] = [
    (Paths::Moon, Vref::V1024),
    (Paths::Moon, Vref::V2500),
    (Paths::Low, Vref::V1024),
    (Paths::Low, Vref::V2500),
    (Paths::High, Vref::V1024),
    (Paths::High, Vref::V2500),
];

/// Each level packed in a `u16`, the code in bits 0 to 9, the reference in
/// bit 10 and the paths in 11 and 12.
#[expect(
    clippy::as_conversions,
    reason = "widening u8 to usize for an array length, where From can't be called"
)]
static TABLE: [u16; LEVELS as usize] = table();

const VREF_BIT: u16 = 1 << 10;
const PATHS_SHIFT: u8 = 11;

const _: () = assert!(DAC_MAX < VREF_BIT);
const _: () = assert!(CEILING > 0 && CEILING <= LEVELS);

/// The output for `level`. Level 0 is off, and anything over [`CEILING`] runs
/// at the ceiling.
pub fn output(level: u8) -> Output {
    let Some(index) = level.min(CEILING).checked_sub(1) else {
        return Output::OFF;
    };
    match TABLE.get(usize::from(index)) {
        Some(&packed) => unpack(packed),
        None => Output::OFF,
    }
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

/// The lowest gear whose codes reach `target`, and the code there, rounded
/// to the nearest or up.
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

/// Worked out at compile time, so the wide maths never reaches the chip. Each
/// level is held to more current than the last, by the model.
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
        // A lower gear would need a code over the top of the DAC.
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

    #[test]
    fn the_ceiling_is_about_half_the_stock_firmwares_top() {
        let ceiling = output(CEILING);
        assert_eq!(ceiling.paths, Paths::High);
        let share = ceiling.modelled() * 100 / TOP.modelled();
        assert!((45..=55).contains(&share), "{share} %");
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
