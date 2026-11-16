//! The MCU's internal temperature sensor, turned into sixteenths of a kelvin.
//!
//! The datasheet (DS40002413B section 33.3.3.8) gives kelvin from one 12-bit
//! conversion against the 2.048 V reference as
//!
//! ```text
//! kelvin  ==  ((offset - reading) * slope + 2048) >> 12
//! ```
//!
//! with the offset from `SIGROW.TEMPSENSE1` and the slope from
//! `SIGROW.TEMPSENSE0`, both 16-bit words. The reading falls as the die warms.
//! That's whole kelvin, and at about a quarter of a kelvin a count, most of
//! what rounds away is real. `thermal` predicts from how the temperature's
//! moving, so a reading that sits still and then jumps a kelvin misleads it.
//!
//! So the ADC accumulates four conversions, and this works in their sum `s`.
//! Four conversions are four times the reading, and sixteenths are sixteen
//! times the kelvin, so the sum works out like this.
//!
//! ```text
//! sixteenths  ==  ((4 * offset - s) * slope + 512) >> 10
//! ```
//!
//! The product runs to 32 bits, and there's no runtime 32-bit multiply on
//! this chip (docs/toolchain.md). [`multiply`] builds it from four 8-by-8
//! products, which the chip's `mul` does, and adds them up with shifts, which
//! need no library call. Every step is exact, so this is the formula above to
//! the bit.

/// The factory calibration for this chip's temperature sensor, from the
/// signature row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    /// `SIGROW.TEMPSENSE1`, the reading at absolute zero, in counts of one
    /// conversion.
    pub offset: u16,
    /// `SIGROW.TEMPSENSE0`, 4096ths of a kelvin a count.
    pub slope: u16,
}

impl Calibration {
    /// Sixteenths of a kelvin from four accumulated conversions, as
    /// [`sixteenths`] works it out.
    pub fn sixteenths(self, sum: u16) -> u16 {
        sixteenths(sum, self.offset, self.slope)
    }
}

/// `a × b`, exactly, from four 8-by-8 products.
///
/// Each partial product is at most 255 × 255, which fits a `u16`, so it's a
/// single `mul` on the chip. The sum is at most 65,535 squared, under
/// `u32::MAX`.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "each byte product is at most 65,025, and the sum of the shifted products is \
              a × b, which is under 2^32"
)]
pub fn multiply(a: u16, b: u16) -> u32 {
    let [al, ah] = a.to_le_bytes();
    let [bl, bh] = b.to_le_bytes();
    let byte = |x: u8, y: u8| u32::from(u16::from(x) * u16::from(y));
    byte(al, bl) + (byte(al, bh) << 8) + (byte(ah, bl) << 8) + (byte(ah, bh) << 16)
}

/// Sixteenths of a kelvin from four accumulated temperature sensor
/// conversions, using the factory calibration from the signature row.
///
/// A sum at or over four times the offset (which would mean a broken sensor,
/// a bad read or a die below absolute zero) comes out as 0 rather than
/// wrapping, and anything over `u16::MAX` sixteenths comes out as that. Neither
/// is plausible to `thermal`.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the product is at most 65,535 squared, so adding 512 stays under 2^32"
)]
pub fn sixteenths(sum: u16, offset: u16, slope: u16) -> u16 {
    let Some(difference) = (u32::from(offset) << 2).checked_sub(u32::from(sum)) else {
        return 0;
    };
    let difference = u16::try_from(difference).unwrap_or(u16::MAX);
    u16::try_from((multiply(difference, slope) + 512) >> 10).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sum above, done the slow way with room to spare.
    fn reference(sum: u16, offset: u16, slope: u16) -> u16 {
        let d = (4 * i64::from(offset) - i64::from(sum)).clamp(0, i64::from(u16::MAX));
        ((d * i64::from(slope) + 512) >> 10).min(i64::from(u16::MAX)) as u16
    }

    /// The backup of this torch's signature row (backup/stock/prodsig.hex).
    const BACKUP: Calibration = Calibration {
        offset: 0x0D24,
        slope: 0x03EE,
    };

    #[test]
    fn multiply_is_exact() {
        let edges = [0, 1, 0xFF, 0x100, 0x1FF, 0x7FFF, 0x8000, 0xFF00, 0xFFFF];
        let grid = (0..=u16::MAX).step_by(251).chain(edges);
        for a in grid.clone() {
            for b in grid.clone() {
                assert_eq!(multiply(a, b), u32::from(a) * u32::from(b), "{a} {b}");
            }
        }
    }

    #[test]
    fn matches_the_sum_for_every_accumulated_reading_and_a_spread_of_calibrations() {
        for sum in 0..=4 * 4095 {
            for offset in (0..=u16::MAX).step_by(4099).chain([BACKUP.offset]) {
                for slope in (0..=u16::MAX).step_by(4111).chain([BACKUP.slope]) {
                    assert_eq!(
                        sixteenths(sum, offset, slope),
                        reference(sum, offset, slope),
                        "sum {sum}, offset {offset}, slope {slope}",
                    );
                }
            }
        }
    }

    #[test]
    fn four_conversions_land_within_half_a_kelvin_of_the_datasheet() {
        // Its formula for one conversion rounds to the nearest kelvin, so
        // four of the same conversion can only differ from it by that.
        for reading in 0..4096_i64 {
            let offset = i64::from(BACKUP.offset);
            let slope = i64::from(BACKUP.slope);
            let datasheet = (((offset - reading) * slope + 2048) >> 12).max(0);
            let ours = i64::from(BACKUP.sixteenths(4 * reading as u16));
            assert!((ours - 16 * datasheet).abs() <= 8, "reading {reading}");
        }
    }

    #[test]
    fn the_backups_calibration_reads_room_temperature_about_where_it_should() {
        // 25 °C is 298 K, and 4 × (3364 - 298 × 4096 / 1006) is about 8603.
        assert_eq!(BACKUP.sixteenths(8603) / 16, 298);
        // A count of the sum is about a sixteenth.
        let step = BACKUP.sixteenths(8603) - BACKUP.sixteenths(8604);
        assert!(step <= 1, "{step}");
    }

    #[test]
    fn a_warmer_die_reads_lower() {
        assert!(BACKUP.sixteenths(8000) > BACKUP.sixteenths(9000));
    }

    #[test]
    fn a_sum_at_or_over_four_times_the_offset_is_zero() {
        assert_eq!(sixteenths(4 * 3364, 3364, 1006), 0);
        assert_eq!(sixteenths(u16::MAX, 3364, 1006), 0);
    }

    #[test]
    fn a_sum_nothing_could_produce_stays_far_from_plausible() {
        // An ADC stuck at zero.
        assert!(BACKUP.sixteenths(0) > 423 * 16);
        assert_eq!(sixteenths(0, u16::MAX, u16::MAX), u16::MAX);
    }
}
