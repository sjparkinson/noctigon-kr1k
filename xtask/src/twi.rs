//! TWI0's host and the AW2016 on its bus, for sim.rs's fake chip. Sections
//! are DS40002413B's and quoted headings the AW2016 datasheet's (Awinic V1.3).

use kr1k::aw2016::{ADDRESS, CHGDIS, CHIPEN, GCR1, GCR2, IMAX_5MA, LCFG1, LCTR, PWM1, RESET, RSTR};
use kr1k::board::{AUX_BLUE, AUX_GREEN, AUX_RED};
use kr1k::regs::{
    TWI_ARBLOST, TWI_BUSERR, TWI_BUSSTATE, TWI_BUSSTATE_BUSY, TWI_BUSSTATE_IDLE,
    TWI_BUSSTATE_OWNER, TWI_BUSSTATE_UNKNOWN, TWI_CLKHOLD, TWI_ENABLE, TWI_MCMD_STOP, TWI_RIF,
    TWI_RXACK, TWI_WIF, addr as reg,
};
use kr1k::ui::Aux;

use crate::int;

/// Main clocks a second.
const CLK_PER: u64 = int::u64_from_u32(kr1k::watchdog::CLOCK_HZ);
/// The AW2016's fastest SCL and shortest SCL low ("I2C Interface Timing").
const SCL_MAX_HZ: u64 = 400_000;
const T_LOW_MIN_NS: u64 = 1300;
/// The longest fall time table 38-22 allows at 400 kHz.
const T_OF_MAX_NS: u64 = 250;
/// Main clocks after CHIPEN rises before registers past GCR1 take a write.
/// Likely, from `PebbleOS`'s driver. The datasheet gives no wait.
pub const WAKE_CLOCKS: u64 = CLK_PER / 1000;

/// Besides RSTR and GCR1.
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
// The bits modelled in LCFG1 to LCFG3, LCTR and GCR2.
const CUR: u8 = 0x0F;
const ENABLES: u8 = 0x07;
const IMAX: u8 = 0x03;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    Absent,
    SdaLow,
    SclLow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Byte {
    Address(u8),
    Data(u8),
}

pub struct Aw2016 {
    regs: [u8; 0x40],
    woke_at: u64,
}

impl Aw2016 {
    const fn new() -> Self {
        Self {
            regs: [0; 0x40],
            woke_at: 0,
        }
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "at is GCR1, GCR2, LCTR, or a channel's LCFG or PWM, all under 0x40"
    )]
    fn reg(&self, at: u8) -> u8 {
        self.regs[usize::from(at)]
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "at is GCR1 or one of MODELLED, all under 0x40"
    )]
    fn reg_mut(&mut self, at: u8) -> &mut u8 {
        &mut self.regs[usize::from(at)]
    }

    pub fn awake(&self) -> bool {
        self.reg(GCR1) & CHIPEN != 0
    }

    fn output(&self, channel: u8) -> Option<(u8, u8)> {
        let cur = self.reg(LCFG1.checked_add(channel)?) & CUR;
        let pwm = self.reg(PWM1.checked_add(channel)?);
        let on = self.awake() && self.reg(LCTR) & 1 << channel != 0;
        (on && cur > 0 && pwm > 0).then_some((cur, pwm))
    }

    pub fn lit(&self) -> bool {
        (0..3).any(|channel| self.output(channel).is_some())
    }

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
                     the UI has"
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

    /// Standby and reset are as "Operating Mode" and "Software Reset" have them.
    pub fn write(&mut self, now: u64, at: u8, value: u8) -> Result<(), String> {
        let since = now.saturating_sub(self.woke_at);
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
                *self.reg_mut(GCR1) = value;
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
            _ if since < WAKE_CLOCKS => {
                return Err(format!(
                    "writes {value:#04x} to the AW2016's register {at:#04x} {since} clocks after \
                     CHIPEN rose, inside the {WAKE_CLOCKS} its oscillator likely needs, so it's \
                     likely dropped"
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
                    u16::from(at.abs_diff(LCFG1)) + 1
                ));
            }
            _ => *self.reg_mut(at) = value,
        }
        Ok(())
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "TWI0's status bits, which the datasheet keeps separate"
)]
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
    /// When the byte on the bus finishes, in main clocks, and the byte.
    sending: Option<(u64, Byte)>,
    stopping: Option<u64>,
    /// `Some` once the address is acknowledged, and `Some(Some(_))` once the
    /// first byte has set the register pointer ("Write Cycle").
    #[expect(clippy::option_option, reason = "the three states the doc above names")]
    pointer: Option<Option<u8>>,
    gave_up: bool,
    /// Times the host's been turned on.
    pub sessions: u32,
    /// Transfers refused by a NACK or a bus error, or given up on mid-byte.
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

    pub const fn on(&self) -> bool {
        self.on
    }

    /// Main clocks in an SCL period, by equation 1 of §29.3.2.2.1 with no rise time.
    fn period(&self) -> u32 {
        10 + u32::from(2 * u16::from(self.baud))
    }

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
            // §29.3.2.2.3, case M4.
            (Byte::Address(_), Fault::SdaLow) => {
                self.arblost = true;
                self.buserr = true;
                self.state = TWI_BUSSTATE_BUSY;
                self.failures = self.failures.saturating_add(1);
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
                    self.failures = self.failures.saturating_add(1);
                    self.gave_up = true;
                }
            }
            (Byte::Data(value), _) => {
                self.clkhold = true;
                self.rxack = false;
                match self.pointer {
                    Some(None) => self.pointer = Some(Some(value)),
                    Some(Some(at)) => {
                        let past = || format!("writes past the AW2016's registers, at {at:#04x}");
                        if usize::from(at) >= 0x40 {
                            return Err(past());
                        }
                        let was_lit = self.aw2016.lit();
                        self.aw2016.write(now, at, value)?;
                        if pc1_high && self.aw2016.lit() && !was_lit {
                            return Err(
                                "lights the aux with PC1 high: they're out while the main \
                                 LED's lit"
                                    .into(),
                            );
                        }
                        self.pointer = Some(Some(at.checked_add(1).ok_or_else(past)?));
                    }
                    None => return Err("sends data with no address acknowledged".into()),
                }
            }
        }
        Ok(())
    }

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

    fn write_mctrla(&mut self, value: u8) -> Result<(), String> {
        match value {
            0 => {
                if self.stopping.is_some() {
                    return Err("turns the TWI host off while its STOP is still going out".into());
                }
                // Off leaves the bus state unknown again (§29.3.2.2.2).
                let stranded = u32::from(self.sending.is_some());
                *self = Self {
                    fault: self.fault,
                    aw2016: std::mem::replace(&mut self.aw2016, Aw2016::new()),
                    baud: self.baud,
                    sessions: self.sessions,
                    failures: self.failures.saturating_add(stranded),
                    ..Self::new()
                };
            }
            TWI_ENABLE => {
                let ns = |clocks: u32| u64::from(clocks) * 1_000_000_000 / CLK_PER;
                let scl_hz = CLK_PER
                    .checked_div(u64::from(self.period()))
                    .unwrap_or(u64::MAX);
                // SCL low is by equation 3.2, less the fall time.
                if scl_hz > SCL_MAX_HZ
                    || ns(u32::from(self.baud) + 6).saturating_sub(T_OF_MAX_NS) < T_LOW_MIN_NS
                {
                    return Err(format!(
                        "turns the TWI host on with MBAUD {}, an SCL of {} kHz, too quick for \
                         the AW2016's 400 kHz and 1.3 µs low",
                        self.baud,
                        scl_hz / 1000
                    ));
                }
                if !self.on {
                    self.sessions = self.sessions.saturating_add(1);
                }
                self.on = true;
            }
            _ => {
                return Err(format!(
                    "writes {value:#04x} to TWI0.MCTRLA; the simulator only models the host off, \
                     or on with no interrupt, smart mode, quick command or time-out"
                ));
            }
        }
        Ok(())
    }

    fn write_maddr(&mut self, now: u64, value: u8) -> Result<(), String> {
        if !self.on {
            return Err("writes TWI0.MADDR with the host off".into());
        }
        if self.sending.is_some() || self.stopping.is_some() {
            return Err("writes TWI0.MADDR with the bus busy".into());
        }
        if self.gave_up {
            return Err(
                "addresses the bus again after a NACK or a bus error in the same update, rather \
                 than giving up"
                    .into(),
            );
        }
        match self.state {
            TWI_BUSSTATE_IDLE => {}
            TWI_BUSSTATE_UNKNOWN => {
                return Err(
                    "writes TWI0.MADDR with the bus state unknown: it's forced idle after the \
                     host's turned on (section 29.3.2.1.1)"
                        .into(),
                );
            }
            _ => {
                return Err(
                    "writes TWI0.MADDR with the bus still owned or busy, a repeated start the \
                     simulator doesn't model"
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
        // A start and nine SCL periods, and nine for a data byte
        // (§29.3.2.2.3, §29.3.2.2.4).
        let done = match self.fault {
            Fault::SclLow => u64::MAX,
            _ => now.saturating_add(10 * u64::from(self.period())),
        };
        self.sending = Some((done, Byte::Address(value)));
        Ok(())
    }

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
            reg::TWI0_MCTRLA => self.write_mctrla(value)?,
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
            reg::TWI0_MADDR => self.write_maddr(now, value)?,
            reg::TWI0_MDATA => {
                self.ready("TWI0.MDATA")?;
                if self.rxack {
                    return Err("writes TWI0.MDATA after a NACK".into());
                }
                self.clear_flags();
                let done = now.saturating_add(9 * u64::from(self.period()));
                self.sending = Some((done, Byte::Data(value)));
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
                self.stopping = Some(now.saturating_add(u64::from(self.period())));
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

    /// MDATA and MCTRLB only take a write with the bus owned and SCL held (§29.5.9).
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
