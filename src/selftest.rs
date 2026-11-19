//! What the checks at power-on found. The fuses are read back, never written
//! (AN2632 §11), and CRCSCAN checks flash (AN2632 §9.1).

use crate::regs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuses {
    pub wdtcfg: u8,
    pub bodcfg: u8,
    pub osccfg: u8,
    pub syscfg0: u8,
    pub syscfg1: u8,
    pub codesize: u8,
    pub bootsize: u8,
}

impl Fuses {
    /// Sections are DS40002413B. A non-zero `WDTCFG` PERIOD starts the watchdog
    /// locked (§22.3.1), `CRCSEL` also picks the CRC for a scan the firmware
    /// starts (§8.8.2.4), and a whole-flash scan wants one boot section (§11.3.1.1).
    pub const fn ok(self) -> bool {
        let supply = self.syscfg1 & regs::FUSE_MVSYSCFG;
        self.wdtcfg == 0
            && self.osccfg & regs::FUSE_CLKSEL == regs::FUSE_CLKSEL_OSCHF
            && self.syscfg0 & regs::FUSE_UPDIPINCFG == regs::FUSE_UPDIPINCFG_UPDI
            && self.syscfg0 & regs::FUSE_CRCSEL == regs::FUSE_CRCSEL_CRC16
            && (supply == regs::FUSE_MVSYSCFG_DUAL || supply == regs::FUSE_MVSYSCFG_SINGLE)
            && self.codesize == 0
            && self.bootsize == 0
    }
}

/// `flags` is `RSTCTRL.RSTFR` as this boot found it.
pub const fn hung(flags: u8) -> bool {
    flags & regs::RSTCTRL_WDRF != 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Faults {
    pub flash: bool,
    pub fuses: bool,
    pub hang: bool,
    /// `MVIO.STATUS.VDDIO2S` says VDDIO2 is missing (DS40002413B §19.5.3),
    /// which tri-states PC1, the driver's enable (§19.3.2.1).
    pub supply: bool,
}

impl Faults {
    pub const NONE: Self = Self {
        flash: false,
        fuses: false,
        hang: false,
        supply: false,
    };

    pub const fn any(self) -> bool {
        self.flash || self.fuses || self.hang || self.supply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read off this torch, and the factory defaults (DS40002413B §8.8.2).
    const STOCK: Fuses = Fuses {
        wdtcfg: 0x00,
        bodcfg: 0x00,
        osccfg: 0x00,
        syscfg0: 0xD0,
        syscfg1: 0x08,
        codesize: 0x00,
        bootsize: 0x00,
    };

    #[test]
    fn this_torchs_fuses_pass() {
        assert!(STOCK.ok());
        assert_eq!(STOCK.syscfg1, regs::FUSE_MVSYSCFG_DUAL);
        assert_eq!(STOCK.bodcfg & regs::FUSE_ACTIVE, 0, "BOD off");
    }

    #[test]
    fn the_brown_out_detector_on_passes_too() {
        let bod = Fuses {
            bodcfg: regs::FUSE_ACTIVE_ENABLE,
            syscfg1: regs::FUSE_MVSYSCFG_DUAL | regs::FUSE_SUT_64MS,
            ..STOCK
        };
        assert!(bod.ok());
    }

    #[test]
    fn checks_fields_not_whole_bytes() {
        let fine = [
            ("BOD off", 1, 0x00),
            ("BOD sampled at 2.85 V and on in sleep", 1, 0x79),
            ("PF6 as reset", 3, 0xD8),
            ("CRC of all flash at start-up", 3, 0x10),
            ("CRC of the boot section at start-up", 3, 0x50),
            ("EEPROM kept on chip erase", 3, 0xD1),
            ("single supply", 4, 0x17),
            ("no start-up delay", 4, 0x08),
        ];
        for (what, byte, value) in fine {
            assert!(with(byte, value).ok(), "{what}");
        }
    }

    #[test]
    fn each_wrong_field_fails() {
        let bad = [
            ("watchdog on from the fuse", 0, 0x0B),
            ("watchdog window only", 0, 0x10),
            ("32 kHz main clock", 2, 0x01),
            ("CLKSEL reserved", 2, 0x02),
            ("UPDI pin as GPIO", 3, 0xC0),
            ("CRC-32", 3, 0xF0),
            ("MVSYSCFG reserved 0", 4, 0x07),
            ("MVSYSCFG reserved 3", 4, 0x1F),
            ("application code section", 5, 0x20),
            ("boot section", 6, 0x02),
        ];
        for (what, byte, value) in bad {
            assert!(!with(byte, value).ok(), "{what}");
        }
        let erased = Fuses {
            wdtcfg: 0xFF,
            bodcfg: 0xFF,
            osccfg: 0xFF,
            syscfg0: 0xFF,
            syscfg1: 0xFF,
            codesize: 0xFF,
            bootsize: 0xFF,
        };
        assert!(!erased.ok());
    }

    fn with(byte: usize, value: u8) -> Fuses {
        let mut fuses = STOCK;
        let field = match byte {
            0 => &mut fuses.wdtcfg,
            1 => &mut fuses.bodcfg,
            2 => &mut fuses.osccfg,
            3 => &mut fuses.syscfg0,
            4 => &mut fuses.syscfg1,
            5 => &mut fuses.codesize,
            _ => &mut fuses.bootsize,
        };
        *field = value;
        fuses
    }

    #[test]
    fn a_watchdog_reset_is_a_hang_and_nothing_else_is() {
        assert!(hung(regs::RSTCTRL_WDRF));
        assert!(hung(regs::RSTCTRL_WDRF | 0x01), "whatever else is set");
        assert!(!hung(0));
        assert!(!hung(!regs::RSTCTRL_WDRF));
    }

    #[test]
    fn only_a_fault_counts() {
        assert!(!Faults::NONE.any());
        assert!(crate::testing::EACH_FAULT.into_iter().all(Faults::any));
    }
}
