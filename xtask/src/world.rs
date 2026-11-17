//! What the simulated chip sees. That's a script's scene, which
//! `kr1k::testing`'s `Rig` sees too, plus the fuse bytes, which only the
//! firmware reads.

use kr1k::regs::{FUSE_MVSYSCFG, FUSE_MVSYSCFG_SINGLE, addr as reg};
use kr1k::selftest::{Faults, Fuses};
use kr1k::sensors::Channel;
use kr1k::testing::Scene;

use crate::updi::map;

/// The temperature sensor calibration the simulated chip has in its signature
/// row, the same as the library tests'.
pub use kr1k::testing::CALIBRATION;

/// The fuse bytes the simulated chip has, FUSE.WDTCFG to FUSE.BOOTSIZE.
pub const FUSE_BYTES: usize = 9;

/// Each fuse byte's name, by its offset from FUSE.WDTCFG. Offsets 3 and 4 are
/// reserved.
pub const FUSE_NAMES: [&str; FUSE_BYTES] = [
    "WDTCFG", "BODCFG", "OSCCFG", "", "", "SYSCFG0", "SYSCFG1", "CODESIZE", "BOOTSIZE",
];

/// This torch's fuses, from the backup read with the stock firmware on it
/// (2026-11-16, `backup/stock/fuses.hex`), which are the factory defaults
/// (DS40002413B section 8.8.2). That's no watchdog from the fuse, the
/// brown-out detector off, the high-frequency oscillator, UPDI on PF7, no CRC
/// at start-up, dual supply with no start-up delay, and no boot or
/// application section. The reserved bytes read 0xFF in that backup, and
/// `World::fuse` never reads them anyway.
pub const FACTORY_FUSES: [u8; FUSE_BYTES] = [0x00, 0x00, 0x00, 0xFF, 0xFF, 0xD0, 0x08, 0x00, 0x00];

/// FUSE.OSCCFG's offset in [`FACTORY_FUSES`].
const OSCCFG: usize = reg::FUSE_OSCCFG - reg::FUSE_WDTCFG;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct World {
    /// The cell, the temperature, and whether flash matches its CRC, which
    /// the firmware checks once, so only the first tick's counts.
    pub scene: Scene,
    /// The fuse bytes.
    pub fuses: [u8; FUSE_BYTES],
}

impl World {
    /// A cell at 3.8 V, a room at 25 °C and factory fuses.
    pub const NORMAL: Self = Self {
        scene: Scene::NORMAL,
        fuses: FACTORY_FUSES,
    };

    /// The raw reading a conversion on `channel` gives.
    pub fn reading(&self, channel: Channel) -> u16 {
        self.scene.reading(channel)
    }

    /// The fuse at a data-space address, if it's one of `fuses` and not a
    /// reserved byte, which the datasheet doesn't say how reads.
    pub fn fuse(&self, addr: usize) -> Option<u8> {
        let offset = addr.checked_sub(usize::from(map::FUSES))?;
        if FUSE_NAMES.get(offset)?.is_empty() {
            return None;
        }
        self.fuses.get(offset).copied()
    }

    /// A world whose fuses fail the firmware's read-back, which is what a
    /// script's `fault` means. OSCCFG asks for the 32 kHz oscillator.
    pub const fn with_bad_fuse(mut self) -> Self {
        self.fuses[OSCCFG] = 0x01;
        self.scene.fault = true;
        self
    }

    /// What the firmware's power-on checks find in this world.
    pub fn faults(&self) -> Faults {
        let at = |addr| self.fuse(addr).unwrap_or(0xFF);
        let fuses = Fuses {
            wdtcfg: at(reg::FUSE_WDTCFG),
            bodcfg: at(reg::FUSE_BODCFG),
            osccfg: at(reg::FUSE_OSCCFG),
            syscfg0: at(reg::FUSE_SYSCFG0),
            syscfg1: at(reg::FUSE_SYSCFG1),
            codesize: at(reg::FUSE_CODESIZE),
            bootsize: at(reg::FUSE_BOOTSIZE),
        };
        Faults {
            fuses: !fuses.ok(),
            flash: self.scene.corrupt_flash,
            supply: !self.vddio2s(),
            ..Faults::NONE
        }
    }

    /// What MVIO.STATUS.VDDIO2S reads. With the dual-supply fuse it's whether
    /// VDDIO2 is there. With the single-supply one it always reads 1
    /// (DS40002413B section 19.3.1), whatever VDDIO2 is doing, so the
    /// firmware can't see it go.
    pub fn vddio2s(&self) -> bool {
        let supply = self.fuse(reg::FUSE_SYSCFG1).unwrap_or(0xFF) & FUSE_MVSYSCFG;
        supply == FUSE_MVSYSCFG_SINGLE || self.scene.vddio2
    }
}

impl From<Scene> for World {
    /// The world a script's scene describes, `fault` being a bad fuse.
    fn from(scene: Scene) -> Self {
        let world = Self {
            scene,
            ..Self::NORMAL
        };
        if scene.fault {
            world.with_bad_fuse()
        } else {
            world
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kr1k::testing::temperature_reading;

    #[test]
    fn factory_fuses_pass_and_a_bad_one_is_a_fault() {
        assert_eq!(World::NORMAL.faults(), Faults::NONE);
        assert!(World::NORMAL.with_bad_fuse().faults().fuses);
    }

    #[test]
    fn a_scripts_scene_is_the_same_world_and_finds_the_same_faults() {
        assert_eq!(World::from(Scene::NORMAL), World::NORMAL);
        let scene = Scene {
            cell: 295,
            kelvin: 353,
            fault: true,
            corrupt_flash: true,
            adc_dead: true,
            vddio2: false,
            adc_stuck: Some(6140),
        };
        let world = World::from(scene);
        assert_eq!(world.scene, scene);
        assert_eq!(world.faults(), scene.faults());
        assert!(world.faults().supply);
    }

    #[test]
    fn vddio2s_follows_vddio2_with_dual_supply_and_reads_1_with_single() {
        const SYSCFG1: usize = reg::FUSE_SYSCFG1 - reg::FUSE_WDTCFG;
        let mut world = World::NORMAL;
        assert!(world.vddio2s());
        assert!(!world.faults().supply);
        world.scene.vddio2 = false;
        assert!(!world.vddio2s());
        assert!(world.faults().supply);
        world.fuses[SYSCFG1] = FUSE_MVSYSCFG_SINGLE;
        assert!(world.vddio2s(), "single supply never says it's gone");
        assert!(!world.faults().supply);
    }

    #[test]
    fn fuses_read_by_address_and_the_reserved_byte_doesnt() {
        assert_eq!(World::NORMAL.fuse(reg::FUSE_BODCFG), Some(0x00), "BODCFG");
        assert_eq!(World::NORMAL.fuse(reg::FUSE_OSCCFG + 1), None, "reserved");
        assert_eq!(World::NORMAL.fuse(reg::FUSE_OSCCFG + 2), None, "reserved");
        let end = usize::from(map::FUSES) + FUSE_BYTES;
        assert_eq!(World::NORMAL.fuse(end), None, "past the end");
    }

    #[test]
    fn a_cell_and_a_temperature_read_as_the_library_expects() {
        let voltage = World::NORMAL.reading(Channel::Voltage);
        assert!(kr1k::battery::centivolts(voltage).abs_diff(380) <= 1);
        let temperature = World::NORMAL.reading(Channel::Temperature);
        assert_eq!(temperature, temperature_reading(Scene::NORMAL.kelvin));
    }
}
