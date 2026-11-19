//! The MCU's temperature sensor in sixteenths of a kelvin, from four accumulated
//! 12-bit conversions against 2.048 V (the formula in DS40002413B §33.3.3.8).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    /// `SIGROW.TEMPSENSE1`, in counts of one conversion.
    pub offset: u16,
    /// `SIGROW.TEMPSENSE0`, 4096ths of a kelvin a count.
    pub slope: u16,
}

impl Calibration {
    pub fn sixteenths(self, sum: u16) -> u16 {
        sixteenths(sum, self.offset, self.slope)
    }
}

/// Built from 8-by-8 products because the chip has no runtime 32-bit multiply.
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
    use crate::testing::{CALIBRATION, ROOM_READING};

    fn reference(sum: u16, offset: u16, slope: u16) -> u16 {
        let d = (4 * i64::from(offset) - i64::from(sum)).clamp(0, i64::from(u16::MAX));
        ((d * i64::from(slope) + 512) >> 10).min(i64::from(u16::MAX)) as u16
    }

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
        for sum in 0..=crate::sensors::FULL_SCALE {
            for offset in (0..=u16::MAX).step_by(4099).chain([CALIBRATION.offset]) {
                for slope in (0..=u16::MAX).step_by(4111).chain([CALIBRATION.slope]) {
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
        for reading in 0..4096_i64 {
            let offset = i64::from(CALIBRATION.offset);
            let slope = i64::from(CALIBRATION.slope);
            let datasheet = (((offset - reading) * slope + 2048) >> 12).max(0);
            let ours = i64::from(CALIBRATION.sixteenths(4 * reading as u16));
            assert!((ours - 16 * datasheet).abs() <= 8, "reading {reading}");
        }
    }

    #[test]
    fn the_backups_calibration_reads_room_temperature_about_where_it_should() {
        // 298 K is 25 °C, where four conversions sum to about 8603.
        assert_eq!(CALIBRATION.sixteenths(ROOM_READING) / 16, 298);
        let step = CALIBRATION.sixteenths(ROOM_READING) - CALIBRATION.sixteenths(ROOM_READING + 1);
        assert!(step <= 1, "{step}");
    }

    #[test]
    fn a_warmer_die_reads_lower() {
        assert!(CALIBRATION.sixteenths(8000) > CALIBRATION.sixteenths(9000));
    }

    #[test]
    fn a_sum_at_or_over_four_times_the_offset_is_zero() {
        assert_eq!(sixteenths(4 * 3364, 3364, 1006), 0);
        assert_eq!(sixteenths(u16::MAX, 3364, 1006), 0);
    }

    #[test]
    fn a_sum_nothing_could_produce_stays_far_from_plausible() {
        assert!(CALIBRATION.sixteenths(0) > 423 * 16);
        assert_eq!(sixteenths(0, u16::MAX, u16::MAX), u16::MAX);
    }
}
