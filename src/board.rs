//! The KR1K's Lume X1-C driver pin map, as bit numbers within each port. It's
//! the stock firmware's, bar PA1, and nobody has traced it on a real board.

/// PA0, the FET gate, not fitted on this driver.
pub const FET: u8 = 0;
/// PA1, connected to something unknown. The stock build on this torch holds
/// it as an output at 0, so never high, and no pull-up.
pub const HELD_LOW: u8 = 1;
/// PA2, TWI0 SDA to the AW2016.
pub const TWI_SDA: u8 = 2;
/// PA3, TWI0 SCL.
pub const TWI_SCL: u8 = 3;
/// PA4, the LED behind the switch.
pub const BUTTON_LED: u8 = 4;
/// PA5, power path 3, the high range.
pub const PATH3: u8 = 5;
/// PA6, power path 2, the low range.
pub const PATH2: u8 = 6;
/// PA7, power path 1, the moon range.
pub const PATH1: u8 = 7;
pub const PATHS: u8 = 1 << PATH1 | 1 << PATH2 | 1 << PATH3;

/// PC1, the boost and amplifier enable. High is on.
pub const ENABLE: u8 = 1;
pub const UNUSED_PC2: u8 = 2;
pub const UNUSED_PC3: u8 = 3;

/// PD4, the e-switch, active low, with an external 100k pull-up.
pub const SWITCH: u8 = 4;
pub const UNUSED_PD5: u8 = 5;
/// PD6, DAC0's output, the current setpoint.
pub const DAC: u8 = 6;
/// PD7, a power-bank enable on other boards, unused here.
pub const POWER_BANK: u8 = 7;

/// The AW2016 output each aux colour is on, counting LED1 as 0. The stock
/// firmware's channel map, which nobody has traced on a real board.
pub const AUX_RED: u8 = 1;
pub const AUX_GREEN: u8 = 0;
pub const AUX_BLUE: u8 = 2;

/// PF6, the reset pin, left alone.
pub const RESET: u8 = 6;
/// PF7, UPDI, left alone.
pub const UPDI: u8 = 7;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_port_uses_each_pin_once() {
        let porta = [
            FET, HELD_LOW, TWI_SDA, TWI_SCL, BUTTON_LED, PATH3, PATH2, PATH1,
        ];
        let portc = [ENABLE, UNUSED_PC2, UNUSED_PC3];
        let portd = [SWITCH, UNUSED_PD5, DAC, POWER_BANK];
        let portf = [RESET, UPDI];
        for port in [&porta[..], &portc, &portd, &portf] {
            let mask = port.iter().fold(0u8, |all, &pin| {
                assert!(pin < 8, "pin {pin}");
                assert_eq!(all & 1 << pin, 0, "pin {pin} twice");
                all | 1 << pin
            });
            assert_eq!(mask.count_ones() as usize, port.len());
        }
    }

    #[test]
    fn the_paths_are_pa5_to_pa7() {
        assert_eq!(PATHS, 0xE0);
        assert_eq!((PATH1, PATH2, PATH3), (7, 6, 5));
    }

    #[test]
    fn the_pins_stock_names_are_where_it_says() {
        assert_eq!(ENABLE, 1, "PC1");
        assert_eq!(DAC, 6, "PD6");
        assert_eq!(SWITCH, 4, "PD4");
        assert_eq!(BUTTON_LED, 4, "PA4");
        assert_eq!((TWI_SDA, TWI_SCL), (2, 3), "PA2, PA3");
        assert_eq!((RESET, UPDI), (6, 7), "PF6, PF7");
    }
}
