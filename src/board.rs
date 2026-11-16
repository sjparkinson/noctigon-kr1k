//! The KR1K's Lume X1-C driver pin map, as bit numbers within each port.
//! It's what the stock firmware's board definition says, and nobody has
//! traced it on a real board yet.
//!
//! These are board facts, not registers. `src/avr/hw.rs` drives the pins
//! through `src/regs.rs`, and xtask's simulator wires its fake chip up the
//! same way from here.
//!
//! | Pin           | Job                                                    |
//! |---------------|--------------------------------------------------------|
//! | PA0           | FET gate (TCA0 WO0), not fitted on this driver, unused |
//! | PA1           | Unknown, an output held low as the stock build has it  |
//! | PA2           | TWI0 SDA to the AW2016 aux driver                      |
//! | PA3           | TWI0 SCL to the AW2016 aux driver                      |
//! | PA4           | Button LED, plain GPIO                                 |
//! | PA5           | Power path 3, high range                               |
//! | PA6           | Power path 2, low range                                |
//! | PA7           | Power path 1, moon range                               |
//! | PC1           | Boost and amplifier enable, high is on                 |
//! | PC2, PC3      | Unused                                                 |
//! | PD4           | E-switch, active low, external 100k pull-up            |
//! | PD5           | Unused                                                 |
//! | PD6           | DAC0 out, the current setpoint                         |
//! | PD7           | Power-bank enable on other boards, unused              |
//! | PF6           | Reset pin, left alone                                  |
//! | PF7           | UPDI, left alone                                       |
//!
//! The RGB aux LEDs hang off the AW2016, an I2C LED driver, so no MCU pin
//! lights a colour. `ui::Aux` still says which colour the library wants, and
//! turning that into AW2016 channel writes is a job for a TWI driver that
//! doesn't exist yet. Until then the aux stays dark.
//!
//! PA1 is the one pin that isn't what the stock board definition says. That
//! has nothing on it and gives it a pull-up, but the build this torch shipped
//! with (0284, in the gitignored `backup/stock/flash.hex`) writes 0xF2 to
//! `VPORTA.DIR` in its reset path and never sets PA1's `OUT`, so PA1 is an
//! output held low. `cargo xtask peek` on 2026-11-16, with that build asleep,
//! read `VPORTA.DIR` as 0xE2, PA1 still an output. What PA1 connects to on
//! the X1-C is unknown (on the plain Lume X1 it's the aux red LED), so the
//! firmware does what the stock build did and never drives it high.

/// PA0, the FET gate on drivers that have a FET. Unused here.
pub const FET: u8 = 0;
/// PA1, connected to nobody knows what, so an output held low as the stock
/// build on this torch has it (see the module doc). Never high, and no
/// pull-up.
pub const HELD_LOW: u8 = 1;
/// PA2, TWI0 SDA.
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
/// All three power paths, as a PORTA mask.
pub const PATHS: u8 = 1 << PATH1 | 1 << PATH2 | 1 << PATH3;

/// PC1, the dangerous one, which is low whenever the light's off.
pub const ENABLE: u8 = 1;
/// PC2.
pub const UNUSED_PC2: u8 = 2;
/// PC3.
pub const UNUSED_PC3: u8 = 3;

/// PD4, the e-switch.
pub const SWITCH: u8 = 4;
/// PD5.
pub const UNUSED_PD5: u8 = 5;
/// PD6, DAC0's output.
pub const DAC: u8 = 6;
/// PD7, a power-bank enable on other boards.
pub const POWER_BANK: u8 = 7;

/// PF6, the reset pin.
pub const RESET: u8 = 6;
/// PF7, UPDI.
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
