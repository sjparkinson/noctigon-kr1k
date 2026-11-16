//! What the checks at power-on found.
//!
//! The firmware reads the fuses back before it turns anything on (AN2632
//! §11), and checks the fields it relies on, not whole bytes. The datasheet,
//! the errata and the ATDF disagree about the factory bytes, so a whole-byte
//! compare would fail on a chip that's fine. It never writes them.
//!
//! It also has the chip's CRCSCAN check all of flash against the CRC-16 that
//! `cargo xtask hex` stores in its last two bytes (AN2632 §9.1), which catches
//! a flash that a `cargo xtask flash` left half-written. The hardware does the
//! sums, so all that's here is what a failure means.
//!
//! A watchdog reset means something hung ([`hung`]). And MVIO saying VDDIO2
//! is missing means PC1, the driver's enable, can't be driven at all
//! ([`Faults::supply`]).
//!
//! A failure keeps the light off until the cell comes out
//! (`limits::Health::Fault`). The aux would show white for a few seconds after
//! power-on and after each press, so a torch that won't come on still says
//! why, but on the KR1K the aux stay dark until something drives the AW2016.
//!
//! That's the whole of the safety net. The RAM and register tests, the clock
//! check and a proof that the watchdog bites are left out on purpose (see
//! `watchdog`).

use crate::regs;

/// The AVR32DD20's fuse bytes, FUSE.WDTCFG to FUSE.BOOTSIZE without the two
/// reserved ones, as read from data space.
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
    /// Whether every field the firmware relies on is something it can cope
    /// with. The intent is that UPDI stays usable and nothing starts that
    /// the firmware doesn't set up itself. The backup of this torch
    /// (2026-11-16, `backup/stock/fuses.hex`) read the factory defaults, and
    /// they pass.
    ///
    /// Section numbers are DS40002413B, the AVR32/16DD14/20 datasheet.
    ///
    /// - `WDTCFG` is 0. A non-zero PERIOD starts the watchdog at boot with
    ///   LOCK set (§22.3.1), so the firmware can't set it up. Nothing says
    ///   what a WINDOW alone does, so the whole byte has to be 0.
    /// - `BODCFG` isn't checked. Whether the brown-out detector runs, and at
    ///   what level, only decides when a sagging cell holds the chip in reset,
    ///   which the firmware already has to survive as a power cut. It's off on
    ///   this torch, so a sagging cell gets no reset at all (docs/safety.md),
    ///   and the firmware never writes fuses to change that.
    /// - `OSCCFG.CLKSEL` is the internal high-frequency oscillator. The 32 kHz
    ///   one as the main clock would leave every timing hundreds of times slow.
    /// - `SYSCFG0.UPDIPINCFG` is UPDI, so the light can still be reflashed.
    ///   On this chip UPDI has its own pin (PF7), and `RSTPINCFG` only says
    ///   whether PF6 is a reset or an input (§8.8.2.4), which the firmware
    ///   never uses.
    /// - `SYSCFG0.CRCSEL` is CRC-16. CRCSCAN has no CRC type of its own, so
    ///   this fuse picks it for the scan the firmware starts too (§8.8.2.4),
    ///   and `cargo xtask hex` only stores a CRC-16. `CRCSRC` and `EESAVE`
    ///   are left alone.
    /// - `SYSCFG1.MVSYSCFG` is dual or single supply, not one of the two
    ///   reserved values (§8.8.2.5). `SUT`, the start-up time, can be
    ///   anything.
    /// - `CODESIZE` and `BOOTSIZE` are 0, so all of flash is one boot section
    ///   (§11.3.1.1). The vector table is linked at 0, and only a whole-flash
    ///   CRC scan expects the checksum where `xtask` puts it (table 30-1).
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

/// Whether `RSTCTRL.RSTFR`, as this boot found it, says the watchdog reset
/// the chip. The firmware's only ever reset by the watchdog if the loop
/// stopped feeding it, so that's [`Faults::hang`].
pub const fn hung(flags: u8) -> bool {
    flags & regs::RSTCTRL_WDRF != 0
}

/// Which checks failed at power-on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Faults {
    /// Flash doesn't match the CRC `cargo xtask hex` put at its end, by
    /// CRCSCAN's sums.
    pub flash: bool,
    /// The fuses aren't what the firmware expects.
    pub fuses: bool,
    /// The watchdog reset the chip, so something hung.
    pub hang: bool,
    /// MVIO says VDDIO2 isn't there (`MVIO.STATUS.VDDIO2S`, DS40002413B
    /// section 19.5.3). PORTC is on VDDIO2 and the fuses have it as a
    /// separate supply, so without it PC1, the driver's enable, is
    /// tri-stated and nothing the firmware writes reaches it (section
    /// 19.3.2.1).
    pub supply: bool,
}

impl Faults {
    /// Every check passed.
    pub const NONE: Self = Self {
        flash: false,
        fuses: false,
        hang: false,
        supply: false,
    };

    /// Whether anything failed, which keeps the light off.
    pub const fn any(self) -> bool {
        self.flash || self.fuses || self.hang || self.supply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This torch's fuses, from the backup of it read with the stock firmware
    /// on it (2026-11-16, `backup/stock/fuses.hex`): `WDTCFG 00, BODCFG 00,
    /// OSCCFG 00, FF, FF, SYSCFG0 D0, SYSCFG1 08, CODESIZE 00, BOOTSIZE 00`.
    /// Those are the factory defaults (DS40002413B section 8.8.2). The
    /// brown-out detector's off, the start-up delay is 0 ms, MVIO is dual
    /// supply, UPDI is on PF7 and PF6 is an input.
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
        // (what, which byte, its value), the bytes in `Fuses`' order. Each
        // changes only fields the firmware doesn't depend on.
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

    /// [`STOCK`] with byte `byte`, in `Fuses`' order, set to `value`.
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
        let flash = Faults {
            flash: true,
            ..Faults::NONE
        };
        let fuses = Faults {
            fuses: true,
            ..Faults::NONE
        };
        let hang = Faults {
            hang: true,
            ..Faults::NONE
        };
        let supply = Faults {
            supply: true,
            ..Faults::NONE
        };
        assert!(flash.any() && fuses.any() && hang.any() && supply.any());
    }
}
