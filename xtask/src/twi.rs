//! TWI0's host and the AW2016 on its bus, for sim.rs's fake chip.
//!
//! Like the rest of the simulator, this is one reading of two datasheets, run
//! against code written from the same reading, and it proves nothing. Section
//! numbers are DS40002413B's. The AW2016's facts are its datasheet's (Awinic,
//! V1.3), which has no section numbers, so they're named by heading.
//!
//! # The host
//!
//! - MBAUD is only written with the host off (§29.5.7). An SCL period is
//!   `10 + 2 × BAUD` main clocks, equation 1 of §29.3.2.2.1 with no rise time,
//!   which is the quickest the bus can go, and it has to be within the
//!   AW2016's 400 kHz with an SCL low of at least its 1.3 µs, by equation 3.2
//!   with the 250 ns fall time table 38-22 allows.
//! - MCTRLA is off or on, and nothing else: no interrupts, no smart mode, no
//!   quick command and no time-out. Turning the host on leaves the bus state
//!   unknown, and MSTATUS.BUSSTATE has to be forced idle before an address
//!   goes out (§29.3.2.1.1). Turning it off leaves it unknown again (§29.3.2.2.2).
//! - An address in MADDR takes a start and nine SCL periods, a byte in MDATA
//!   nine, and each ends in WIF and CLKHOLD with RXACK saying whether the
//!   AW2016 acknowledged (§29.3.2.2.3, §29.3.2.2.4). Nothing in MADDR, MDATA
//!   or MCTRLB is written while a byte's on the bus (§29.5.9), no data goes
//!   after a NACK, and no address goes out again in the same update after a
//!   NACK or a bus error, since the firmware gives up rather than retrying.
//!   Only writes are modelled, since the firmware never reads.
//! - MCTRLB only takes STOP, with the host owning the bus and holding SCL.
//!   The bus is idle a period later. The host stops in every sleep mode
//!   (§29.3.5), so `sleep` with it on is refused by sim.rs.
//! - A completed byte lands when MSTATUS is next read, or anything else of
//!   the host's is touched, like the ADC's result, so it's timed by the
//!   simulated clock and nothing else.
//!
//! # The AW2016
//!
//! - Its address is 0x64. Anything else goes unacknowledged.
//! - A write's first byte is the register, and each byte after it goes in
//!   the next one up ("Write Cycle").
//! - It comes up in standby with every register at 0. In standby only RSTR
//!   and GCR1 take a write, and writing anything else is an error here, since
//!   the real chip drops it ("Operating Mode"). RSTR takes 0x55, which resets
//!   every register ("Software Reset").
//! - For a millisecond after CHIPEN rises, writes to anything but RSTR and
//!   GCR1 are refused too. That's likely, not the datasheet's, which gives no
//!   wait. A driver for it in PebbleOS found such writes acknowledged and
//!   dropped, and the Linux driver waits 2 ms.
//! - Only GCR1's CHIPEN and CHGDIS, GCR2's IMAX, LCTR's enables, LCFG1 to
//!   LCFG3's current settings and PWM1 to PWM3 are modelled. Anything else
//!   written is an error, so a pattern mode, a protection turned off or an
//!   interrupt enable fails here until it's taught.
//!
//! # Faults on the bus
//!
//! A run can take the AW2016 off the bus, so every address goes
//! unacknowledged, or hold SDA low, so every address loses arbitration with a
//! bus error and leaves the bus busy (§29.3.2.2.3, case M4), or hold SCL low,
//! so a byte never finishes and no flag ever comes up. The firmware has to
//! give up on each without hanging and carry on.

use kr1k::aw2016::{ADDRESS, CHGDIS, CHIPEN, GCR1, GCR2, IMAX_5MA, LCFG1, LCTR, PWM1, RESET, RSTR};
use kr1k::board::{AUX_BLUE, AUX_GREEN, AUX_RED};
use kr1k::regs::{
    TWI_ARBLOST, TWI_BUSERR, TWI_BUSSTATE, TWI_BUSSTATE_BUSY, TWI_BUSSTATE_IDLE,
    TWI_BUSSTATE_OWNER, TWI_BUSSTATE_UNKNOWN, TWI_CLKHOLD, TWI_ENABLE, TWI_MCMD_STOP, TWI_RIF,
    TWI_RXACK, TWI_WIF, addr as reg,
};
use kr1k::ui::Aux;

/// Main clocks a second, as sim.rs runs the chip.
const CLK_PER: u64 = 4_000_000;
/// The AW2016's fastest SCL ("I2C Interface Timing").
const SCL_MAX_HZ: u64 = 400_000;
/// Its shortest SCL low, 1.3 µs, in nanoseconds.
const T_LOW_MIN_NS: u64 = 1300;
/// The longest fall time table 38-22 allows at 400 kHz, in nanoseconds.
const T_OF_MAX_NS: u64 = 250;
/// Main clocks after CHIPEN rises before the AW2016 takes any register but
/// RSTR and GCR1. A millisecond, likely, from the PebbleOS driver.
pub const WAKE_CLOCKS: u64 = CLK_PER / 1000;

/// The registers this models, past RSTR and GCR1.
const MODELLED: [u8; 8] = [
    GCR2,
    LCTR,
    LCFG1,
    LCFG1 + 1,
    LCFG1 + 2,
    PWM1,
    PWM1 + 1,
    PWM1 + 2,
];
/// The bits of each LCFG register that are CUR.
const CUR: u8 = 0x0F;
/// LCTR's three enables.
const ENABLES: u8 = 0x07;
/// GCR2's IMAX.
const IMAX: u8 = 0x03;

/// What's wrong with the bus in a run, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// The AW2016 never acknowledges its address.
    Absent,
    /// SDA is held low, so every address loses arbitration.
    SdaLow,
    /// SCL is held low, so no byte ever finishes.
    SclLow,
}

/// What's on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Byte {
    Address(u8),
    Data(u8),
}

/// The AW2016's registers, 0x00 to 0x3F, as far as this models them.
pub struct Aw2016 {
    regs: [u8; 0x40],
    /// When CHIPEN last rose.
    woke_at: u64,
}

impl Aw2016 {
    const fn new() -> Self {
        Self {
            regs: [0; 0x40],
            woke_at: 0,
        }
    }

    fn reg(&self, at: u8) -> u8 {
        self.regs[usize::from(at)]
    }

    /// Whether GCR1.CHIPEN is set, so it's awake rather than in standby.
    pub fn awake(&self) -> bool {
        self.reg(GCR1) & CHIPEN != 0
    }

    /// An output's current setting and PWM code, if it's lit.
    fn output(&self, channel: u8) -> Option<(u8, u8)> {
        let cur = self.reg(LCFG1 + channel) & CUR;
        let pwm = self.reg(PWM1 + channel);
        let on = self.awake() && self.reg(LCTR) & 1 << channel != 0;
        (on && cur > 0 && pwm > 0).then_some((cur, pwm))
    }

    /// Whether any aux LED is lit.
    pub fn lit(&self) -> bool {
        (0..3).any(|channel| self.output(channel).is_some())
    }

    /// The colour the aux show, through the board's channel map, and whether
    /// it's bright, or `None` dark. A mix of outputs no docs/ui.md colour has,
    /// a colour that's partly dim and partly bright, or dim at a global
    /// current above the lowest, is an error.
    pub fn colour(&self) -> Result<Option<(Aux, bool)>, String> {
        let [red, green, blue] = [AUX_RED, AUX_GREEN, AUX_BLUE].map(|ch| self.output(ch));
        let colour = match (red.is_some(), green.is_some(), blue.is_some()) {
            (false, false, false) => return Ok(None),
            (false, false, true) => Aux::Blue,
            (true, false, false) => Aux::Red,
            (true, true, false) => Aux::Yellow,
            (true, true, true) => Aux::White,
            (r, g, b) => {
                return Err(format!(
                    "the AW2016 lights red {r}, green {g} and blue {b}, which is no colour \
                     docs/ui.md has"
                ));
            }
        };
        let pwms: Vec<u8> = [red, green, blue]
            .iter()
            .flatten()
            .map(|&(_, pwm)| pwm)
            .collect();
        let bright = pwms.iter().all(|&pwm| pwm == u8::MAX);
        if !bright && pwms.contains(&u8::MAX) {
            return Err(format!(
                "the AW2016 shows {colour:?} partly dim and partly bright, PWM {pwms:?}"
            ));
        }
        if !bright && self.reg(GCR2) & IMAX != IMAX_5MA {
            return Err(format!(
                "the AW2016 shows {colour:?} dim with GCR2.IMAX {:#04b}, not the lowest \
                 global current, 5 mA",
                self.reg(GCR2) & IMAX
            ));
        }
        Ok(Some((colour, bright)))
    }

    /// Takes a byte into register `at`, at `now`.
    fn write(&mut self, now: u64, at: u8, value: u8) -> Result<(), String> {
        let late = now.saturating_sub(self.woke_at) < WAKE_CLOCKS;
        match at {
            RSTR if value == RESET => self.regs = [0; 0x40],
            RSTR => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's RSTR, which only takes 0x55, the reset"
                ));
            }
            GCR1 if value & !(CHIPEN | CHGDIS) != 0 => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's GCR1; only CHIPEN and CHGDIS are \
                     modelled"
                ));
            }
            GCR1 => {
                if value & CHIPEN != 0 && !self.awake() {
                    self.woke_at = now;
                }
                self.regs[usize::from(GCR1)] = value;
            }
            _ if !MODELLED.contains(&at) => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's register {at:#04x}, which the \
                     simulator doesn't model"
                ));
            }
            _ if !self.awake() => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's register {at:#04x} in standby, where \
                     only RSTR and GCR1 take a write, so it's dropped"
                ));
            }
            _ if late => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's register {at:#04x} {} clocks after \
                     CHIPEN rose, inside the {WAKE_CLOCKS} its oscillator likely needs, so it's \
                     likely dropped",
                    now - self.woke_at
                ));
            }
            GCR2 if value & !IMAX != 0 => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's GCR2, turning its protection off, \
                     which the simulator doesn't model"
                ));
            }
            LCTR if value & !ENABLES != 0 => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's LCTR; only the enables are modelled"
                ));
            }
            _ if (LCFG1..LCFG1 + 3).contains(&at) && value & !CUR != 0 => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's LCFG{}, a pattern, fade or sync the \
                     simulator doesn't model",
                    at - LCFG1 + 1
                ));
            }
            _ => self.regs[usize::from(at)] = value,
        }
        Ok(())
    }
}

/// TWI0 in host mode, and the bus it drives.
pub struct Twi {
    pub fault: Fault,
    pub aw2016: Aw2016,
    baud: u8,
    on: bool,
    state: u8,
    wif: bool,
    rxack: bool,
    arblost: bool,
    buserr: bool,
    clkhold: bool,
    /// The byte on the bus and when it finishes.
    sending: Option<(u64, Byte)>,
    /// When the STOP that's going out leaves the bus idle.
    stopping: Option<u64>,
    /// Whether this transfer's address was acknowledged, and the register
    /// it's pointing at once its first byte's in.
    pointer: Option<Option<u8>>,
    /// A NACK or a bus error has happened since the host was turned on.
    gave_up: bool,
    /// Times the host's been turned on.
    pub sessions: u32,
    /// Times a transfer was refused, by a NACK or a bus error, or a byte
    /// never finished.
    pub failures: u32,
}

impl Twi {
    pub const fn new() -> Self {
        Self {
            fault: Fault::None,
            aw2016: Aw2016::new(),
            baud: 0,
            on: false,
            state: TWI_BUSSTATE_UNKNOWN,
            wif: false,
            rxack: false,
            arblost: false,
            buserr: false,
            clkhold: false,
            sending: None,
            stopping: None,
            pointer: None,
            gave_up: false,
            sessions: 0,
            failures: 0,
        }
    }

    /// Whether the host's on.
    pub const fn on(&self) -> bool {
        self.on
    }

    /// Main clocks in an SCL period.
    fn period(&self) -> u64 {
        10 + 2 * u64::from(self.baud)
    }

    /// Lands whatever's finished by `now`. `pc1_high` says the light's on, and
    /// lighting an aux LED then is an error.
    pub fn catch_up(&mut self, now: u64, pc1_high: bool) -> Result<(), String> {
        if let Some(at) = self.stopping
            && now >= at
        {
            self.stopping = None;
            self.state = TWI_BUSSTATE_IDLE;
        }
        let Some((at, byte)) = self.sending else {
            return Ok(());
        };
        if now < at {
            return Ok(());
        }
        self.sending = None;
        self.wif = true;
        match (byte, self.fault) {
            (Byte::Address(_), Fault::SdaLow) => {
                self.arblost = true;
                self.buserr = true;
                self.state = TWI_BUSSTATE_BUSY;
                self.failures += 1;
                self.gave_up = true;
                return Ok(());
            }
            (Byte::Address(address), fault) => {
                self.clkhold = true;
                let acked = fault == Fault::None && address == ADDRESS << 1;
                self.rxack = !acked;
                if acked {
                    self.pointer = Some(None);
                } else {
                    self.failures += 1;
                    self.gave_up = true;
                }
            }
            (Byte::Data(value), _) => {
                self.clkhold = true;
                self.rxack = false;
                match self.pointer {
                    Some(None) => self.pointer = Some(Some(value)),
                    Some(Some(at)) => {
                        if usize::from(at) >= 0x40 {
                            return Err(format!(
                                "writes past the AW2016's registers, at {at:#04x}"
                            ));
                        }
                        let was_lit = self.aw2016.lit();
                        self.aw2016.write(now, at, value)?;
                        if pc1_high && self.aw2016.lit() && !was_lit {
                            return Err(
                                "lights the aux with PC1 high: they're out while the main \
                                 LED's lit (docs/ui.md)"
                                    .into(),
                            );
                        }
                        self.pointer = Some(Some(at + 1));
                    }
                    None => return Err("sends data with no address acknowledged".into()),
                }
            }
        }
        Ok(())
    }

    /// MSTATUS as it reads at `now`.
    pub fn status(&mut self, now: u64, pc1_high: bool) -> Result<u8, String> {
        self.catch_up(now, pc1_high)?;
        let flag = |on: bool, bit: u8| if on { bit } else { 0 };
        Ok(flag(self.wif, TWI_WIF)
            | flag(self.clkhold, TWI_CLKHOLD)
            | flag(self.rxack, TWI_RXACK)
            | flag(self.arblost, TWI_ARBLOST)
            | flag(self.buserr, TWI_BUSERR)
            | self.state)
    }

    /// A write to one of TWI0's registers at `now`.
    pub fn write(&mut self, now: u64, a: usize, value: u8, pc1_high: bool) -> Result<(), String> {
        self.catch_up(now, pc1_high)?;
        match a {
            reg::TWI0_MBAUD => {
                if self.on {
                    return Err(
                        "writes TWI0.MBAUD with the host on, which section 29.5.7 doesn't allow"
                            .into(),
                    );
                }
                self.baud = value;
            }
            reg::TWI0_MCTRLA => match value {
                0 => {
                    if self.stopping.is_some() {
                        return Err(
                            "turns the TWI host off while its STOP is still going out".into()
                        );
                    }
                    // A byte still on the bus is one the firmware gave up
                    // waiting for.
                    let stranded = u32::from(self.sending.is_some());
                    *self = Self {
                        fault: self.fault,
                        aw2016: std::mem::replace(&mut self.aw2016, Aw2016::new()),
                        baud: self.baud,
                        sessions: self.sessions,
                        failures: self.failures + stranded,
                        ..Self::new()
                    };
                }
                TWI_ENABLE => {
                    let ns = |clocks: u64| clocks * 1_000_000_000 / CLK_PER;
                    let period = self.period();
                    if CLK_PER / period > SCL_MAX_HZ
                        || ns(u64::from(self.baud) + 6).saturating_sub(T_OF_MAX_NS) < T_LOW_MIN_NS
                    {
                        return Err(format!(
                            "turns the TWI host on with MBAUD {}, an SCL of {} kHz, too quick \
                             for the AW2016's 400 kHz and 1.3 µs low",
                            self.baud,
                            CLK_PER / period / 1000
                        ));
                    }
                    if !self.on {
                        self.sessions += 1;
                    }
                    self.on = true;
                }
                _ => {
                    return Err(format!(
                        "writes {value:#04x} to TWI0.MCTRLA; the simulator only models the host \
                         off, or on with no interrupt, smart mode, quick command or time-out"
                    ));
                }
            },
            reg::TWI0_MSTATUS => {
                if !self.on {
                    return Err("writes TWI0.MSTATUS with the host off".into());
                }
                if value & TWI_BUSSTATE == TWI_BUSSTATE_IDLE {
                    self.state = TWI_BUSSTATE_IDLE;
                }
                self.wif &= value & TWI_WIF == 0;
                self.clkhold &= value & (TWI_WIF | TWI_RIF) == 0;
                self.arblost &= value & TWI_ARBLOST == 0;
                self.buserr &= value & TWI_BUSERR == 0;
            }
            reg::TWI0_MADDR => {
                if !self.on {
                    return Err("writes TWI0.MADDR with the host off".into());
                }
                if self.sending.is_some() || self.stopping.is_some() {
                    return Err("writes TWI0.MADDR with the bus busy".into());
                }
                if self.gave_up {
                    return Err(
                        "addresses the bus again after a NACK or a bus error in the same \
                         update, rather than giving up"
                            .into(),
                    );
                }
                match self.state {
                    TWI_BUSSTATE_IDLE => {}
                    TWI_BUSSTATE_UNKNOWN => {
                        return Err(
                            "writes TWI0.MADDR with the bus state unknown: it's forced idle \
                             after the host's turned on (section 29.3.2.1.1)"
                                .into(),
                        );
                    }
                    _ => {
                        return Err(
                            "writes TWI0.MADDR with the bus still owned or busy, a repeated \
                             start the simulator doesn't model"
                                .into(),
                        );
                    }
                }
                if value & 1 != 0 {
                    return Err(format!(
                        "addresses {:#04x} for a read, which the simulator doesn't model",
                        value >> 1
                    ));
                }
                self.clear_flags();
                self.state = TWI_BUSSTATE_OWNER;
                self.pointer = None;
                let done = match self.fault {
                    Fault::SclLow => u64::MAX,
                    _ => now + 10 * self.period(),
                };
                self.sending = Some((done, Byte::Address(value)));
            }
            reg::TWI0_MDATA => {
                self.ready("TWI0.MDATA")?;
                if self.rxack {
                    return Err("writes TWI0.MDATA after a NACK".into());
                }
                self.clear_flags();
                self.sending = Some((now + 9 * self.period(), Byte::Data(value)));
            }
            reg::TWI0_MCTRLB => {
                if value != TWI_MCMD_STOP {
                    return Err(format!(
                        "writes {value:#04x} to TWI0.MCTRLB; the simulator only models STOP"
                    ));
                }
                self.ready("TWI0.MCTRLB")?;
                self.clear_flags();
                self.pointer = None;
                self.stopping = Some(now + self.period());
            }
            _ => {
                return Err(format!(
                    "writes {value:#04x} to TWI0 register {a:#06x}, which the simulator \
                     doesn't model"
                ));
            }
        }
        Ok(())
    }

    /// Refuses `what` unless the host owns the bus and holds SCL between
    /// bytes, which is when MDATA and MCTRLB take a write (§29.5.9).
    fn ready(&self, what: &str) -> Result<(), String> {
        if !self.on || self.state != TWI_BUSSTATE_OWNER {
            return Err(format!("writes {what} without owning the bus"));
        }
        if self.sending.is_some() || !self.clkhold {
            return Err(format!("writes {what} with a byte still on the bus"));
        }
        Ok(())
    }

    const fn clear_flags(&mut self) {
        self.wif = false;
        self.clkhold = false;
        self.arblost = false;
        self.buserr = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAUD: u8 = 15;
    const PERIOD: u64 = 10 + 2 * BAUD as u64;

    fn on(fault: Fault) -> Twi {
        let mut twi = Twi::new();
        twi.fault = fault;
        twi.write(0, reg::TWI0_MBAUD, BAUD, false).unwrap();
        twi.write(0, reg::TWI0_MCTRLA, TWI_ENABLE, false).unwrap();
        twi.write(0, reg::TWI0_MSTATUS, TWI_BUSSTATE_IDLE, false)
            .unwrap();
        twi
    }

    /// Sends `bytes` to the AW2016 from `now` as one transfer, waiting out
    /// each byte, then a STOP, and returns the time it ends.
    fn transfer(twi: &mut Twi, mut now: u64, bytes: &[u8]) -> u64 {
        twi.write(now, reg::TWI0_MADDR, ADDRESS << 1, false)
            .unwrap();
        now += 10 * PERIOD;
        for &byte in bytes {
            let status = twi.status(now, false).unwrap();
            assert_eq!(status & (TWI_WIF | TWI_RXACK), TWI_WIF, "{status:#04x}");
            twi.write(now, reg::TWI0_MDATA, byte, false).unwrap();
            now += 9 * PERIOD;
        }
        twi.status(now, false).unwrap();
        twi.write(now, reg::TWI0_MCTRLB, TWI_MCMD_STOP, false)
            .unwrap();
        now += PERIOD;
        assert_eq!(
            twi.status(now, false).unwrap() & TWI_BUSSTATE,
            TWI_BUSSTATE_IDLE
        );
        now
    }

    #[test]
    fn an_address_takes_ten_scl_periods_and_a_byte_nine_from_mbaud() {
        let mut twi = on(Fault::None);
        twi.write(0, reg::TWI0_MADDR, ADDRESS << 1, false).unwrap();
        assert_eq!(
            twi.status(10 * PERIOD - 1, false).unwrap(),
            TWI_BUSSTATE_OWNER
        );
        assert_eq!(
            twi.status(10 * PERIOD, false).unwrap(),
            TWI_WIF | TWI_CLKHOLD | TWI_BUSSTATE_OWNER
        );
        twi.write(10 * PERIOD, reg::TWI0_MDATA, GCR1, false)
            .unwrap();
        assert_eq!(twi.status(19 * PERIOD - 1, false).unwrap() & TWI_WIF, 0);
        assert_ne!(twi.status(19 * PERIOD, false).unwrap() & TWI_WIF, 0);
    }

    #[test]
    fn a_write_lands_in_the_aw2016_auto_incrementing() {
        let mut twi = on(Fault::None);
        let now = transfer(&mut twi, 0, &[GCR1, CHIPEN]);
        assert!(twi.aw2016.awake());
        let now = transfer(&mut twi, now + WAKE_CLOCKS, &[LCFG1, 0, 0, 1, 0, 0, 255]);
        transfer(&mut twi, now, &[LCTR, 1 << AUX_BLUE]);
        assert_eq!(twi.aw2016.colour(), Ok(Some((Aux::Blue, true))));
    }

    #[test]
    fn standby_takes_only_rstr_and_gcr1_and_a_reset_clears_everything() {
        let mut twi = on(Fault::None);
        twi.write(0, reg::TWI0_MADDR, ADDRESS << 1, false).unwrap();
        twi.status(10 * PERIOD, false).unwrap();
        twi.write(10 * PERIOD, reg::TWI0_MDATA, LCTR, false)
            .unwrap();
        twi.status(19 * PERIOD, false).unwrap();
        twi.write(19 * PERIOD, reg::TWI0_MDATA, 1, false).unwrap();
        let err = twi.status(28 * PERIOD, false).unwrap_err();
        assert!(err.contains("in standby"), "{err}");

        let mut aw = Aw2016::new();
        aw.write(0, GCR1, CHIPEN).unwrap();
        let err = aw.write(WAKE_CLOCKS - 1, LCTR, 1).unwrap_err();
        assert!(err.contains("oscillator"), "{err}");
        aw.write(WAKE_CLOCKS, LCTR, 1).unwrap();
        aw.write(WAKE_CLOCKS, RSTR, RESET).unwrap();
        assert!(!aw.awake() && aw.reg(LCTR) == 0);
        assert!(aw.write(0, RSTR, 1).is_err());
    }

    #[test]
    fn what_the_simulator_doesnt_model_is_an_error() {
        let mut aw = Aw2016::new();
        aw.write(0, GCR1, CHIPEN).unwrap();
        for (at, value) in [
            (GCR1, 0x80),
            (GCR2, 0x10),
            (LCTR, 0x08),
            (LCFG1, 0x10),
            (0x37, 0),
            (0x02, 0),
        ] {
            assert!(aw.write(WAKE_CLOCKS, at, value).is_err(), "{at:#04x}");
        }
        let mut twi = on(Fault::None);
        assert!(
            twi.write(0, reg::TWI0_MCTRLA, TWI_ENABLE | 0x02, false)
                .is_err()
        );
        assert!(
            twi.write(0, reg::TWI0_MADDR, ADDRESS << 1 | 1, false)
                .is_err()
        );
        assert!(twi.write(0, reg::TWI0_CTRLA, 0, false).is_err());
    }

    #[test]
    fn the_host_wants_the_bus_forced_idle_and_mbaud_written_while_off() {
        let mut twi = Twi::new();
        twi.write(0, reg::TWI0_MBAUD, BAUD, false).unwrap();
        twi.write(0, reg::TWI0_MCTRLA, TWI_ENABLE, false).unwrap();
        let err = twi
            .write(0, reg::TWI0_MADDR, ADDRESS << 1, false)
            .unwrap_err();
        assert!(err.contains("forced idle"), "{err}");
        assert!(twi.write(0, reg::TWI0_MBAUD, BAUD, false).is_err());
        let mut fast = Twi::new();
        fast.write(0, reg::TWI0_MBAUD, 0, false).unwrap();
        let err = fast
            .write(0, reg::TWI0_MCTRLA, TWI_ENABLE, false)
            .unwrap_err();
        assert!(err.contains("too quick"), "{err}");
        let mut fm = Twi::new();
        fm.write(0, reg::TWI0_MBAUD, 1, false).unwrap();
        fm.write(0, reg::TWI0_MCTRLA, TWI_ENABLE, false).unwrap();
    }

    #[test]
    fn nothing_goes_on_the_bus_mid_byte_and_nothing_after_a_nack() {
        let mut twi = on(Fault::None);
        twi.write(0, reg::TWI0_MADDR, ADDRESS << 1, false).unwrap();
        assert!(twi.write(1, reg::TWI0_MDATA, 0, false).is_err());
        assert!(
            twi.write(1, reg::TWI0_MCTRLB, TWI_MCMD_STOP, false)
                .is_err()
        );
        assert!(twi.write(1, reg::TWI0_MADDR, ADDRESS << 1, false).is_err());

        let mut twi = on(Fault::Absent);
        twi.write(0, reg::TWI0_MADDR, ADDRESS << 1, false).unwrap();
        let status = twi.status(10 * PERIOD, false).unwrap();
        assert_eq!(status & (TWI_WIF | TWI_RXACK), TWI_WIF | TWI_RXACK);
        assert!(twi.write(10 * PERIOD, reg::TWI0_MDATA, 0, false).is_err());
        twi.write(10 * PERIOD, reg::TWI0_MCTRLB, TWI_MCMD_STOP, false)
            .unwrap();
        let err = twi
            .write(11 * PERIOD, reg::TWI0_MADDR, ADDRESS << 1, false)
            .unwrap_err();
        assert!(err.contains("giving up"), "{err}");
        assert_eq!(twi.failures, 1);
    }

    #[test]
    fn sda_held_low_loses_arbitration_and_scl_held_low_never_finishes() {
        let mut twi = on(Fault::SdaLow);
        twi.write(0, reg::TWI0_MADDR, ADDRESS << 1, false).unwrap();
        let status = twi.status(10 * PERIOD, false).unwrap();
        assert_eq!(
            status,
            TWI_WIF | TWI_ARBLOST | TWI_BUSERR | TWI_BUSSTATE_BUSY
        );
        assert!(
            twi.write(10 * PERIOD, reg::TWI0_MCTRLB, TWI_MCMD_STOP, false)
                .is_err()
        );
        twi.write(10 * PERIOD, reg::TWI0_MCTRLA, 0, false).unwrap();

        let mut twi = on(Fault::SclLow);
        twi.write(0, reg::TWI0_MADDR, ADDRESS << 1, false).unwrap();
        assert_eq!(twi.status(1 << 40, false).unwrap() & TWI_WIF, 0);
        assert!(twi.sending.is_some());
        twi.write(1 << 40, reg::TWI0_MCTRLA, 0, false).unwrap();
        assert!(!twi.on() && twi.sending.is_none());
    }

    #[test]
    fn the_aux_never_light_with_pc1_high() {
        let mut twi = on(Fault::None);
        let now = transfer(&mut twi, 0, &[GCR1, CHIPEN]);
        let now = transfer(&mut twi, now + WAKE_CLOCKS, &[LCFG1, 1, 1, 1, 9, 9, 9]);
        twi.write(now, reg::TWI0_MADDR, ADDRESS << 1, true).unwrap();
        let now = now + 10 * PERIOD;
        twi.status(now, true).unwrap();
        twi.write(now, reg::TWI0_MDATA, LCTR, true).unwrap();
        twi.status(now + 9 * PERIOD, true).unwrap();
        twi.write(now + 9 * PERIOD, reg::TWI0_MDATA, 7, true)
            .unwrap();
        let err = twi.status(now + 18 * PERIOD, true).unwrap_err();
        assert!(err.contains("PC1 high"), "{err}");
    }

    #[test]
    fn colours_read_through_the_channel_map_and_dim_wants_the_lowest_current() {
        let mut aw = Aw2016::new();
        aw.write(0, GCR1, CHIPEN).unwrap();
        let w = |aw: &mut Aw2016, at: u8, v: u8| aw.write(WAKE_CLOCKS, at, v).unwrap();
        w(&mut aw, GCR2, IMAX_5MA);
        for ch in 0..3 {
            w(&mut aw, LCFG1 + ch, 1);
            w(&mut aw, PWM1 + ch, 64);
        }
        w(&mut aw, LCTR, 1 << AUX_RED | 1 << AUX_GREEN);
        assert_eq!(aw.colour(), Ok(Some((Aux::Yellow, false))));
        w(&mut aw, LCTR, 1 << AUX_GREEN);
        assert!(aw.colour().is_err(), "green alone is no colour");
        w(&mut aw, LCTR, 1 << AUX_BLUE);
        w(&mut aw, GCR2, 0);
        assert!(aw.colour().unwrap_err().contains("lowest global current"));
        w(&mut aw, PWM1 + AUX_BLUE, 255);
        assert_eq!(aw.colour(), Ok(Some((Aux::Blue, true))));
        w(&mut aw, GCR1, 0);
        assert_eq!(aw.colour(), Ok(None), "standby");
    }
}
