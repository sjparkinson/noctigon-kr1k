//! A UPDI programmer for the AVR32DD20 over a serial adapter (DS40002413B
//! chapters 11 and 36), with the sequences of pymcuprog's `nvmp2.py`.
//!
//! It deliberately can't write fuses, the lock bits or the user row, and
//! can't unlock a locked chip.

use std::fmt;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use kr1k::regs::{
    NVMCTRL_CMD_CHER as NVM_CHIP_ERASE, NVMCTRL_CMD_EEERWR as NVM_EEPROM_ERASE_WRITE,
    NVMCTRL_CMD_FLPER as NVM_FLASH_PAGE_ERASE, NVMCTRL_CMD_FLWR as NVM_FLASH_WRITE,
    NVMCTRL_CMD_NONE as NVM_NO_COMMAND, NVMCTRL_EEBUSY as NVM_EEBUSY, NVMCTRL_ERROR as NVM_ERROR,
    NVMCTRL_ERROR_DOUBLESELECT as NVM_ERROR_COLLISION,
    NVMCTRL_ERROR_ILLEGALCMD as NVM_ERROR_INVALID_COMMAND,
    NVMCTRL_ERROR_ILLEGALSADDR as NVM_ERROR_WRITE_PROTECT, NVMCTRL_FBUSY as NVM_FBUSY,
};

/// All but [`UPDI_FLASH`] are data-space addresses (DS40002413B §36.2.2).
pub mod map {
    use kr1k::regs::{self, addr, mem};

    use crate::int;

    /// UPDI's address space (DS §36.2.2, figure 36-2), not in the ATDF.
    pub const UPDI_FLASH: u32 = 0x80_0000;
    pub const FLASH: u16 = int::u16_of(mem::MAPPED_PROGMEM_START);
    pub const FLASH_SIZE: usize = mem::PROGMEM_SIZE;
    pub const FLASH_PAGE: usize = mem::PROGMEM_PAGE;

    pub const EEPROM: u16 = int::u16_of(mem::EEPROM_START);
    pub const EEPROM_SIZE: usize = mem::EEPROM_SIZE;

    pub const SIGNATURE: u16 = int::u16_of(mem::SIGNATURES_START);
    pub const AVR32DD20: [u8; 3] = regs::SIGNATURE;

    /// Read only, since UPDI can't write the signature row (DS table 8-5).
    pub const SERIAL: u16 = int::u16_of(addr::SIGROW_SERNUM0);
    pub const SERIAL_SIZE: usize = addr::SIGROW_SERNUM15 - addr::SIGROW_SERNUM0 + 1;

    /// Read for `backup`. Nothing here writes the fuses.
    pub const FUSES: u16 = int::u16_of(mem::FUSES_START);
    pub const FUSES_SIZE: usize = mem::FUSES_SIZE;

    /// LOCK.KEY, low byte first. Read for `backup`, and nothing here writes it.
    pub const LOCK: u16 = int::u16_of(mem::LOCKBITS_START);
    pub const LOCK_SIZE: usize = mem::LOCKBITS_SIZE;
    /// Any other value locks the chip (DS §8.9.2.1).
    #[cfg(test)]
    pub const UNLOCKED: [u8; LOCK_SIZE] = regs::LOCK_KEY_NOLOCK.to_le_bytes();

    /// Read for `backup`. Nothing here writes the user row.
    pub const USERROW: u16 = int::u16_of(mem::USER_SIGNATURES_START);
    pub const USERROW_SIZE: usize = mem::USER_SIGNATURES_SIZE;

    pub const NVMCTRL_CTRLA: u16 = int::u16_of(addr::NVMCTRL_CTRLA);
    pub const NVMCTRL_STATUS: u16 = int::u16_of(addr::NVMCTRL_STATUS);
}

/// `send` swallows the echo of what it writes.
pub trait Wire {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<()>;
    /// Resets UPDI's own state machine, not the chip.
    fn double_break(&mut self) -> io::Result<()>;
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub type Result<T> = std::result::Result<T, String>;

pub fn fits_flash(data: &[u8]) -> Result<()> {
    if data.len() > map::FLASH_SIZE {
        return Err(format!(
            "the image is {} bytes; the AVR32DD20 has {}",
            data.len(),
            map::FLASH_SIZE
        ));
    }
    Ok(())
}

pub fn fits_eeprom(data: &[u8]) -> Result<()> {
    if data.len() > map::EEPROM_SIZE {
        return Err(format!(
            "the EEPROM image is bigger than {} bytes",
            map::EEPROM_SIZE
        ));
    }
    Ok(())
}

const SYNC: u8 = 0x55;
const ACK: u8 = 0x40;

// Instructions (DS40002413B figure 36-8).
const LDS: u8 = 0x00;
const LD: u8 = 0x20;
const STS: u8 = 0x40;
const ST: u8 = 0x60;
const LDCS: u8 = 0x80;
const REPEAT: u8 = 0xA0;
const STCS: u8 = 0xC0;
const KEY: u8 = 0xE0;

const PTR_INC: u8 = 0x04;
const PTR_ADDRESS: u8 = 0x08;
/// Size A of three bytes, in bits 3:2 of LDS and STS.
const ADDRESS_24: u8 = 0x08;
/// The same size in bits 1:0 of an ST to the pointer (DS §36.3.3.4).
const PTR_SIZE_24: u8 = 0x02;
const DATA_8: u8 = 0x00;
const DATA_16: u8 = 0x01;
const KEY_SIB: u8 = 0x04;
const KEY_64: u8 = 0x00;
const SIB_16: u8 = 0x01;

// Control and status registers (DS §36.4) and their bits (§36.5.3 to §36.5.9).
const CS_STATUSA: u8 = 0x00;
const CS_CTRLA: u8 = 0x02;
const CS_CTRLB: u8 = 0x03;
const ASI_KEY_STATUS: u8 = 0x07;
const ASI_RESET_REQ: u8 = 0x08;
const ASI_SYS_STATUS: u8 = 0x0B;

const CTRLA_IBDLY: u8 = 1 << 7; // inter-byte delay on replies
const CTRLA_RSD: u8 = 1 << 3; // response signature (ACK) disable
const CTRLB_CCDETDIS: u8 = 1 << 3; // collision detection off
const CTRLB_UPDIDIS: u8 = 1 << 2; // turn UPDI off
const KEY_STATUS_NVMPROG: u8 = 1 << 4;
const SYS_STATUS_PROGSTART: u8 = 1 << 3;
const SYS_STATUS_LOCKSTATUS: u8 = 1 << 0;
const RESET_REQ_APPLY: u8 = 0x59;

const KEY_NVMPROG: &[u8; 8] = b"NVMProg ";

/// SIB bytes 8 to 10, `NVM_VERSION` (DS figure 36-20). A tinyAVR reads `P:0`.
const SIB_NVM_P2: &[u8; 3] = b"P:2";

/// The most one REPEAT covers, in bytes on a read and words on a write.
const MAX_BLOCK: usize = 256;

/// As avrdude allows to reach programming mode. SUT is 64 ms at most (ATDF).
const PROGMODE_TIMEOUT: Duration = Duration::from_millis(500);

const ADDRESS_END: u32 = 1 << 24;

/// A unique ID by DS §8.6.2.3, unconfirmed across chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialNumber([u8; map::SERIAL_SIZE]);

impl SerialNumber {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        bytes.try_into().map(Self).map_err(|_| {
            format!(
                "a serial number is {} bytes, not {}",
                map::SERIAL_SIZE,
                bytes.len()
            )
        })
    }

    pub const fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Nothing says a real chip ever reads like this.
    pub fn is_blank(&self) -> bool {
        self.0 == [0xFF; map::SERIAL_SIZE] || self.0 == [0x00; map::SERIAL_SIZE]
    }

    pub fn check_saved(&self, dir: &Path) -> Result<()> {
        if !self.is_blank() {
            return Ok(());
        }
        Err(format!(
            "the backup in {} was saved with a serial number of all {:02X}, which can't tell \
             one chip from another. `restore` only writes a backup onto the chip it was read \
             from, and with that serial number it can't tell, so it won't write this backup",
            dir.display(),
            self.0[0]
        ))
    }

    fn check_chip(&self, chip: &Self, dir: &Path) -> Result<()> {
        self.check_saved(dir)?;
        if chip.is_blank() {
            return Err(format!(
                "this chip's serial number reads all {:02X}, which can't tell one chip from \
                 another, so `restore` can't tell whether the backup in {} (serial number \
                 {self}) was read from it. Nothing was erased or written",
                chip.0[0],
                dir.display()
            ));
        }
        if chip != self {
            return Err(format!(
                "this chip's serial number is {chip}, and the backup in {} was read from a \
                 chip whose serial number is {self}. `restore` only writes a backup onto the \
                 chip it was read from, so nothing was erased or written",
                dir.display()
            ));
        }
        Ok(())
    }
}

impl fmt::Display for SerialNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

pub struct Programmer<W: Wire> {
    wire: W,
}

impl<W: Wire> Programmer<W> {
    pub fn connect(wire: W) -> Result<Self> {
        let mut p = Self { wire };
        if p.init_link().is_err() {
            p.wire.double_break().map_err(io_error)?;
            p.init_link()?;
        }
        Ok(p)
    }

    /// The SIB needs no key (DS §36.3.7), so it's checked before the key goes out.
    pub fn connect_for_programming(wire: W) -> Result<Self> {
        let mut p = Self::connect(wire)?;
        if let Err(error) = p.check_sib() {
            let _ = p.disconnect();
            return Err(error);
        }
        match p.enter_progmode().and_then(|()| p.check_signature()) {
            Ok(()) => Ok(p),
            Err(error) => {
                let _ = p.leave();
                Err(error)
            }
        }
    }

    pub fn connect_for_restore(wire: W, saved: &SerialNumber, dir: &Path) -> Result<Self> {
        saved.check_saved(dir)?;
        let mut p = Self::connect_for_programming(wire)?;
        match p
            .serial_number()
            .and_then(|chip| saved.check_chip(&chip, dir))
        {
            Ok(()) => Ok(p),
            Err(error) => {
                let _ = p.leave();
                Err(error)
            }
        }
    }

    fn init_link(&mut self) -> Result<()> {
        self.stcs(CS_CTRLB, CTRLB_CCDETDIS)?;
        self.stcs(CS_CTRLA, CTRLA_IBDLY)?;
        match self.ldcs(CS_STATUSA)? {
            0 => Err("the UPDI link came up but STATUSA reads 0".into()),
            _ => Ok(()),
        }
    }

    /// Readable without a key, locked or not (DS §36.3.7).
    pub fn sib(&mut self) -> Result<[u8; 16]> {
        self.send(&[SYNC, KEY | KEY_SIB | SIB_16])?;
        let mut sib = [0; 16];
        self.recv(&mut sib)?;
        Ok(sib)
    }

    pub fn check_sib(&mut self) -> Result<()> {
        let sib = self.sib()?;
        if &sib[8..11] == SIB_NVM_P2 {
            return Ok(());
        }
        Err(format!(
            "expected an AVR32DD20, whose System Information Block names NVM version P:2, but \
             this chip's reads `{}`, so it isn't one. Nothing was erased or written, and no \
             programming key was sent",
            String::from_utf8_lossy(&sib).trim_end()
        ))
    }

    /// DS40002413B §36.3.8.2 steps 2 to 7.
    pub fn enter_progmode(&mut self) -> Result<()> {
        let status = self.ldcs(ASI_SYS_STATUS)?;
        if status & SYS_STATUS_LOCKSTATUS == 0 && status & SYS_STATUS_PROGSTART != 0 {
            return Ok(());
        }

        let mut frame = vec![SYNC, KEY | KEY_64];
        frame.extend(KEY_NVMPROG.iter().rev());
        self.send(&frame)?;
        if self.ldcs(ASI_KEY_STATUS)? & KEY_STATUS_NVMPROG == 0 {
            return Err("the chip didn't accept the NVM programming key".into());
        }

        self.reset()?;
        let start = self.wire.now();
        loop {
            let status = self.ldcs(ASI_SYS_STATUS)?;
            if status & SYS_STATUS_LOCKSTATUS == 0 && status & SYS_STATUS_PROGSTART != 0 {
                return Ok(());
            }
            if self.wire.now().duration_since(start) >= PROGMODE_TIMEOUT {
                break;
            }
        }
        Err(
            "the chip didn't enter programming mode. If it's locked, unlocking means \
             erasing it, which this tool won't do"
                .into(),
        )
    }

    pub fn check_signature(&mut self) -> Result<()> {
        let signature = self.read(map::SIGNATURE, 3)?;
        if signature == map::AVR32DD20 {
            Ok(())
        } else {
            Err(format!(
                "expected an AVR32DD20 (signature {}) but found {}",
                hex(&map::AVR32DD20),
                hex(&signature)
            ))
        }
    }

    pub fn serial_number(&mut self) -> Result<SerialNumber> {
        let bytes = self.read(map::SERIAL, map::SERIAL_SIZE)?;
        SerialNumber::from_bytes(&bytes)
    }

    /// `addr` is in UPDI's address space, so flash is from [`map::UPDI_FLASH`].
    pub fn read(&mut self, addr: impl Into<u32>, len: usize) -> Result<Vec<u8>> {
        let addr = addr.into();
        let mut out = vec![0; len];
        for (chunk, at) in out.chunks_mut(MAX_BLOCK).zip((0..).step_by(MAX_BLOCK)) {
            self.st_ptr(offset(addr, at)?)?;
            let mut frame = repeat(chunk.len())?;
            frame.extend([SYNC, LD | PTR_INC | DATA_8]);
            self.send(&frame)?;
            self.recv(chunk)?;
        }
        Ok(out)
    }

    /// CHER (DS §11.3.2.3.8). Only reached unlocked, so it's never an unlock.
    pub fn chip_erase(&mut self) -> Result<()> {
        self.wait_nvm()?;
        self.nvm_command(NVM_CHIP_ERASE)?;
        let erased = self.wait_nvm();
        self.clear_command(erased)
    }

    /// A word only lands on a page this call erased (DS §11.3.2.3.1).
    pub fn erase_and_write_flash(&mut self, data: &[u8]) -> Result<()> {
        fits_flash(data)?;
        self.chip_erase()?;
        let pages = (0..).step_by(map::FLASH_PAGE);
        for (chunk, at) in data.chunks(map::FLASH_PAGE).zip(pages) {
            if chunk.iter().all(|&b| b == 0xFF) {
                continue;
            }
            let page = offset(map::UPDI_FLASH, at)?;
            self.erase_flash_page(page)?;
            self.write_flash_page(page, chunk)?;
        }
        Ok(())
    }

    /// FLPER (DS §11.3.2.3.2), with 0xFF as pymcuprog stores.
    fn erase_flash_page(&mut self, page: u32) -> Result<()> {
        self.wait_nvm()?;
        self.nvm_command(NVM_FLASH_PAGE_ERASE)?;
        let erased = self.sts(page, 0xFF).and_then(|()| self.wait_nvm());
        self.clear_command(erased)
    }

    /// FLWR (DS §11.3.2.3.1) with ACKs off is safe only while a word write
    /// (75 µs max, DS table 38-8) beats a word at 115,200 baud (208 µs).
    fn write_flash_page(&mut self, page: u32, data: &[u8]) -> Result<()> {
        assert!(data.len() <= map::FLASH_PAGE, "a page at most");
        let mut words = data.to_vec();
        if words.len() % 2 == 1 {
            words.push(0xFF);
        }
        let repeat = repeat(words.len() / 2)?;

        self.wait_nvm()?;
        self.nvm_command(NVM_FLASH_WRITE)?;
        let written = self.st_ptr(page).and_then(|()| {
            let mut frame = vec![SYNC, STCS | CS_CTRLA, CTRLA_IBDLY | CTRLA_RSD];
            frame.extend(repeat);
            frame.extend([SYNC, ST | PTR_INC | DATA_16]);
            frame.extend(&words);
            frame.extend([SYNC, STCS | CS_CTRLA, CTRLA_IBDLY]);
            self.send(&frame)?;
            self.wait_nvm()
        });
        self.clear_command(written)
    }

    /// Fuse programming "is identical to the EEPROM programming" (DS
    /// §11.3.2.2), so this must only ever store inside EEPROM.
    pub fn write_eeprom(&mut self, data: &[u8]) -> Result<()> {
        fits_eeprom(data)?;
        self.wait_nvm()?;
        self.nvm_command(NVM_EEPROM_ERASE_WRITE)?;
        let written = data.iter().enumerate().try_for_each(|(i, &byte)| {
            self.sts(offset(map::EEPROM.into(), i)?, byte)?;
            self.wait_nvm()
        });
        self.clear_command(written)
    }

    pub fn verify(&mut self, addr: impl Into<u32>, data: &[u8]) -> Result<()> {
        let addr = addr.into();
        let actual = self.read(addr, data.len())?;
        let mut bytes = actual.iter().zip(data).zip(addr..);
        match bytes.find(|((read, wrote), _)| read != wrote) {
            None => Ok(()),
            Some(((read, wrote), at)) => Err(format!(
                "verify failed at 0x{at:06X}: wrote {wrote:02X}, read {read:02X}"
            )),
        }
    }

    /// DS §36.3.8.2 steps 9 and 10.
    pub fn leave(mut self) -> Result<W> {
        self.reset()?;
        self.disconnect()
    }

    /// UPDIDIS issues a System Reset (DS §36.3.2.2.2), unconfirmed on a chip.
    pub fn disconnect(mut self) -> Result<W> {
        self.stcs(CS_CTRLB, CTRLB_UPDIDIS | CTRLB_CCDETDIS)?;
        Ok(self.wire)
    }

    /// STATUS.ERROR is bits 6:4 (DS §11.5.3), where pymcuprog masks 0x30.
    fn wait_nvm(&mut self) -> Result<()> {
        for _ in 0..10_000 {
            let status = self.lds(map::NVMCTRL_STATUS)?;
            if status & NVM_ERROR != 0 {
                return Err(nvm_error(status));
            }
            if status & (NVM_FBUSY | NVM_EEBUSY) == 0 {
                return Ok(());
            }
        }
        Err("timed out waiting for the NVM controller".into())
    }

    fn nvm_command(&mut self, command: u8) -> Result<()> {
        self.sts(map::NVMCTRL_CTRLA, command)
    }

    /// A change of command must go through NOCMD or NOOP (DS §11.5.1).
    fn clear_command(&mut self, done: Result<()>) -> Result<()> {
        let cleared = self.nvm_command(NVM_NO_COMMAND);
        done.and(cleared)
    }

    fn reset(&mut self) -> Result<()> {
        self.stcs(ASI_RESET_REQ, RESET_REQ_APPLY)?;
        self.stcs(ASI_RESET_REQ, 0)
    }

    fn ldcs(&mut self, reg: u8) -> Result<u8> {
        self.send(&[SYNC, LDCS | reg])?;
        self.recv1()
    }

    fn stcs(&mut self, reg: u8, value: u8) -> Result<()> {
        self.send(&[SYNC, STCS | reg, value])
    }

    fn lds(&mut self, addr: impl Into<u32>) -> Result<u8> {
        let mut frame = vec![SYNC, LDS | ADDRESS_24 | DATA_8];
        frame.extend(address(addr.into())?);
        self.send(&frame)?;
        self.recv1()
    }

    fn sts(&mut self, addr: impl Into<u32>, value: u8) -> Result<()> {
        let mut frame = vec![SYNC, STS | ADDRESS_24 | DATA_8];
        frame.extend(address(addr.into())?);
        self.send(&frame)?;
        self.ack()?;
        self.send(&[value])?;
        self.ack()
    }

    fn st_ptr(&mut self, addr: u32) -> Result<()> {
        let mut frame = vec![SYNC, ST | PTR_ADDRESS | PTR_SIZE_24];
        frame.extend(address(addr)?);
        self.send(&frame)?;
        self.ack()
    }

    fn ack(&mut self) -> Result<()> {
        match self.recv1()? {
            ACK => Ok(()),
            other => Err(format!(
                "expected an ACK (0x40) from the chip, got 0x{other:02X}"
            )),
        }
    }

    fn recv1(&mut self) -> Result<u8> {
        let mut byte = [0];
        self.recv(&mut byte)?;
        Ok(byte[0])
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.wire.send(bytes).map_err(io_error)
    }

    fn recv(&mut self, buf: &mut [u8]) -> Result<()> {
        self.wire.recv(buf).map_err(io_error)
    }
}

fn address(addr: u32) -> Result<[u8; 3]> {
    if addr >= ADDRESS_END {
        return Err("address out of range".into());
    }
    let [lo, mid, hi, _] = addr.to_le_bytes();
    Ok([lo, mid, hi])
}

fn offset(addr: u32, by: usize) -> Result<u32> {
    u32::try_from(by)
        .ok()
        .and_then(|by| addr.checked_add(by))
        .filter(|&at| at < ADDRESS_END)
        .ok_or_else(|| "address out of range".to_string())
}

fn repeat(count: usize) -> Result<Vec<u8>> {
    match count.checked_sub(1).map(u8::try_from) {
        Some(Ok(more)) => Ok(vec![SYNC, REPEAT, more]),
        _ => Err(format!("REPEAT covers 1 to {MAX_BLOCK}, not {count}")),
    }
}

/// The datasheet's names (DS §11.5.3), which differ from the ATDF's.
fn nvm_error(status: u8) -> String {
    let code = status & NVM_ERROR;
    let what = match code {
        NVM_ERROR_INVALID_COMMAND => "INVALIDCMD, the selected command is not supported",
        NVM_ERROR_WRITE_PROTECT => "WRITEPROTECT, a write to a protected section",
        NVM_ERROR_COLLISION => "CMDCOLLISION, a command selected while another was in place",
        _ => "a code the datasheet calls reserved",
    };
    format!(
        "the NVM controller reported error {} ({what})",
        code >> NVM_ERROR.trailing_zeros()
    )
}

fn hex(bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    hex.join(" ")
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err hands the error over by value"
)]
fn io_error(error: io::Error) -> String {
    match error.kind() {
        io::ErrorKind::TimedOut => "no reply from the chip. Check the probe's contact, \
                                    the pad order and that the cell is out"
            .into(),
        _ => format!("serial: {error}"),
    }
}

#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) says the fake is for other xtask modules' tests, which pub inside a private module hides"
)]
pub(crate) mod tests {
    use super::*;
    use fake::Dd20;

    const BACKUP: &str = "backup/breakout";

    #[test]
    fn map_assumes_what_the_atdf_says() {
        use kr1k::regs::{addr, mem};

        let sernum = [
            addr::SIGROW_SERNUM0,
            addr::SIGROW_SERNUM1,
            addr::SIGROW_SERNUM2,
            addr::SIGROW_SERNUM3,
            addr::SIGROW_SERNUM4,
            addr::SIGROW_SERNUM5,
            addr::SIGROW_SERNUM6,
            addr::SIGROW_SERNUM7,
            addr::SIGROW_SERNUM8,
            addr::SIGROW_SERNUM9,
            addr::SIGROW_SERNUM10,
            addr::SIGROW_SERNUM11,
            addr::SIGROW_SERNUM12,
            addr::SIGROW_SERNUM13,
            addr::SIGROW_SERNUM14,
            addr::SIGROW_SERNUM15,
        ];
        assert_eq!(sernum.len(), map::SERIAL_SIZE, "serial number size");
        for (n, at) in sernum.into_iter().enumerate() {
            assert_eq!(at, map::SERIAL as usize + n, "SERNUM{n}");
        }

        assert_eq!(map::FLASH_SIZE, mem::MAPPED_PROGMEM_SIZE, "all of flash");
        assert_eq!(map::FLASH_PAGE, mem::MAPPED_PROGMEM_PAGE, "flash page");
        assert_eq!(map::AVR32DD20.len(), mem::SIGNATURES_SIZE, "signature size");
        assert_eq!(addr::LOCK_KEY, map::LOCK as usize, "LOCK.KEY");
        const { assert!(mem::LOCKBITS_END <= mem::FUSES_START) };
    }

    #[test]
    fn map_is_the_datasheets() {
        // DS40002413B §8 and §36.2.2.
        assert_eq!(map::UPDI_FLASH, 0x80_0000, "flash in UPDI space");
        assert_eq!(map::FLASH_SIZE, 32 * 1024, "flash size");
        assert_eq!(map::FLASH_PAGE, 512, "flash page");
        assert_eq!(map::EEPROM, 0x1400, "EEPROM");
        assert_eq!(map::EEPROM_SIZE, 256, "EEPROM size");
        assert_eq!(map::SIGNATURE, 0x1100, "signature");
        assert_eq!(map::AVR32DD20, [0x1E, 0x95, 0x3A], "AVR32DD20");
        assert_eq!(map::SERIAL, 0x1110, "serial number");
        assert_eq!(map::SERIAL_SIZE, 16, "serial number size");
        assert_eq!(map::LOCK, 0x1040, "lock bits");
        assert_eq!(map::LOCK_SIZE, 4, "LOCK.KEY");
        assert_eq!(map::UNLOCKED, [0x5C, 0xC5, 0xC5, 0x5C], "unlocked");
        assert_eq!(map::FUSES, 0x1050, "fuses");
        assert_eq!(map::FUSES_SIZE, 16, "fuses");
        assert_eq!(map::USERROW, 0x1080, "user row");
        assert_eq!(map::USERROW_SIZE, 32, "user row size");
        assert_eq!(map::NVMCTRL_CTRLA, 0x1000, "NVMCTRL.CTRLA");
        assert_eq!(map::NVMCTRL_STATUS, 0x1002, "NVMCTRL.STATUS");
    }

    #[test]
    fn nvm_commands_are_the_datasheets() {
        // DS40002413B §11.5.1 and §11.5.3, and pymcuprog's `nvmp2.py`.
        assert_eq!(NVM_NO_COMMAND, 0x00, "NOCMD");
        assert_eq!(NVM_FLASH_WRITE, 0x02, "FLWR");
        assert_eq!(NVM_FLASH_PAGE_ERASE, 0x08, "FLPER");
        assert_eq!(NVM_EEPROM_ERASE_WRITE, 0x13, "EEERWR");
        assert_eq!(NVM_CHIP_ERASE, 0x20, "CHER");
        assert_eq!(NVM_FBUSY, 0x01, "FBUSY");
        assert_eq!(NVM_EEBUSY, 0x02, "EEBUSY");
        assert_eq!(NVM_ERROR, 0x70, "ERROR[2:0]");
        assert_eq!(NVM_ERROR_INVALID_COMMAND, 0x10, "INVALIDCMD");
        assert_eq!(NVM_ERROR_WRITE_PROTECT, 0x20, "WRITEPROTECT");
        assert_eq!(NVM_ERROR_COLLISION, 0x30, "CMDCOLLISION");
    }

    #[test]
    fn reads_the_sib() {
        let mut p = Programmer::connect(Dd20::new()).unwrap();
        assert_eq!(&p.sib().unwrap(), b"AVR     P:2D:1-3");
        p.check_sib().unwrap();
    }

    #[test]
    fn reads_data_space_while_the_firmware_runs() {
        let mut p = Programmer::connect(Dd20::new()).unwrap();
        assert_eq!(p.read(map::SIGNATURE, 3).unwrap(), map::AVR32DD20);
        p.disconnect().unwrap();
    }

    #[test]
    fn retries_with_a_double_break() {
        let mut dd = Dd20::new();
        dd.asleep = true;
        let dd = Programmer::connect(dd).unwrap().disconnect().unwrap();
        assert_eq!(dd.breaks, 1);
        assert!(dd.updi_disabled);
    }

    #[test]
    fn waits_for_the_chip_to_start_up_on_a_fast_or_slow_adapter() {
        for round_trip in [Duration::from_micros(100), Duration::from_millis(16)] {
            let mut dd = Dd20::new();
            dd.start_up = Duration::from_millis(400);
            dd.round_trip = round_trip;
            let mut p = Programmer::connect(dd).unwrap();
            p.enter_progmode()
                .unwrap_or_else(|e| panic!("{round_trip:?} a round trip: {e}"));
            p.check_signature().unwrap();
        }
    }

    #[test]
    fn gives_up_on_the_clock_not_a_count_of_polls() {
        for round_trip in [Duration::from_micros(100), Duration::from_millis(16)] {
            let mut dd = Dd20::new();
            dd.start_up = Duration::from_millis(600);
            dd.round_trip = round_trip;
            let poll = round_trip + 2 * dd.byte_time;
            let setup = 20 * dd.byte_time + 2 * round_trip;
            let mut p = Programmer::connect(dd).unwrap();
            let before = p.wire.elapsed;
            assert!(p.enter_progmode().unwrap_err().contains("programming mode"));
            let waited = p.wire.elapsed.checked_sub(before).unwrap();
            assert!(
                (PROGMODE_TIMEOUT..PROGMODE_TIMEOUT + setup + 2 * poll).contains(&waited),
                "{round_trip:?} a round trip: gave up after {waited:?}"
            );
        }
    }

    #[test]
    fn refuses_another_chip_by_its_signature() {
        // An AVR32DD32 (DS table 8-4) and an ATtiny1616.
        for signature in [[0x1E, 0x95, 0x38], [0x1E, 0x94, 0x21]] {
            let mut dd = Dd20::new();
            dd.signature = signature;
            let mut p = Programmer::connect(dd).unwrap();
            p.enter_progmode().unwrap();
            let err = p.check_signature().unwrap_err();
            assert!(err.contains(&hex(&signature)), "{err}");
            assert!(err.contains("1E 95 3A"), "{err}");
        }
    }

    #[test]
    fn connects_for_programming_and_stays_connected() {
        let mut dd = Dd20::new();
        let p = Programmer::connect_for_programming(&mut dd);
        assert!(p.is_ok());
        drop(p);
        assert!(dd.progmode, "not in programming mode");
        assert!(!dd.updi_disabled, "UPDI was turned off");
    }

    fn with_contents(mut dd: Dd20) -> Dd20 {
        dd.flash[..0x3B7E].fill(0x42);
        dd.eeprom[..58].fill(0x24);
        dd
    }

    fn assert_contents_kept(dd: &Dd20) {
        assert!(
            dd.flash[..0x3B7E].iter().all(|&b| b == 0x42),
            "flash changed"
        );
        assert!(
            dd.flash[0x3B7E..].iter().all(|&b| b == 0xFF),
            "flash changed"
        );
        assert!(dd.eeprom[..58].iter().all(|&b| b == 0x24), "EEPROM changed");
        assert!(dd.eeprom[58..].iter().all(|&b| b == 0xFF), "EEPROM changed");
        assert_eq!(dd.nvm_operations(), 0, "an erase or write went in");
    }

    fn assert_refused(dd: &Dd20) {
        assert_eq!(dd.resets, 2, "one reset to enter and one to leave");
        dd.assert_let_go();
        assert_contents_kept(dd);
    }

    fn refused(dd: Dd20) -> String {
        let mut dd = with_contents(dd);
        let Err(error) = Programmer::connect_for_programming(&mut dd) else {
            panic!("connected for programming");
        };
        assert_refused(&dd);
        error
    }

    fn refused_a_restore(dd: Dd20, saved: &SerialNumber) -> String {
        let mut dd = with_contents(dd);
        let Err(error) = Programmer::connect_for_restore(&mut dd, saved, Path::new(BACKUP)) else {
            panic!("connected for a restore");
        };
        assert_refused(&dd);
        error
    }

    #[test]
    fn a_wrong_signature_ends_with_updi_off() {
        let mut dd = Dd20::new();
        dd.signature = [0x1E, 0x95, 0x38];
        let err = refused(dd);
        assert!(err.contains("1E 95 38"), "{err}");
    }

    #[test]
    fn a_tinyavr_is_refused_on_its_sib_before_the_key() {
        let mut dd = with_contents(Dd20::new());
        // What a real ATtiny1616 reported.
        dd.sib = *b"tinyAVR P:0D:0-3";
        dd.signature = [0x1E, 0x94, 0x21];
        let Err(err) = Programmer::connect_for_programming(&mut dd) else {
            panic!("connected for programming");
        };
        assert!(err.contains("tinyAVR P:0D:0-3"), "{err}");
        assert!(err.contains("no programming key was sent"), "{err}");
        assert_eq!(dd.keys, 0, "a key went out");
        assert_eq!(dd.resets, 0, "the chip was reset");
        assert_eq!(dd.data_accesses, 0, "data space was touched");
        assert!(dd.updi_disabled, "UPDI was left on");
        assert_contents_kept(&dd);
    }

    #[test]
    fn a_start_up_that_never_finishes_ends_with_updi_off() {
        let mut dd = Dd20::new();
        dd.start_up = Duration::from_secs(3600);
        let err = refused(dd);
        assert!(err.contains("programming mode"), "{err}");
    }

    #[test]
    fn a_locked_chip_ends_with_updi_off_and_nothing_written() {
        let mut dd = Dd20::new();
        dd.lock = fake::LOCKED;
        let err = refused(dd);
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn reads_the_serial_number_from_sernum0_to_sernum15() {
        let mut dd = Dd20::new();
        dd.serial = std::array::from_fn(|n| 0xA0 + n as u8);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        let serial = p.serial_number().unwrap();
        assert_eq!(serial.bytes(), p.wire.serial);
        assert_eq!(
            serial.to_string(),
            "A0 A1 A2 A3 A4 A5 A6 A7 A8 A9 AA AB AC AD AE AF"
        );
    }

    #[test]
    fn a_serial_number_is_sixteen_bytes() {
        assert!(SerialNumber::from_bytes(&[0x12; 16]).is_ok());
        for wrong in [0, 10, 15, 17] {
            let err = SerialNumber::from_bytes(&vec![0x12; wrong]).unwrap_err();
            assert!(err.contains(&format!("16 bytes, not {wrong}")), "{err}");
        }
    }

    #[test]
    fn a_serial_number_of_all_ff_or_all_00_is_blank() {
        for fill in [0xFF, 0x00] {
            assert!(SerialNumber([fill; map::SERIAL_SIZE]).is_blank());
            for n in 0..map::SERIAL_SIZE {
                let mut bytes = [fill; map::SERIAL_SIZE];
                bytes[n] ^= 0x01;
                assert!(!SerialNumber(bytes).is_blank(), "{fill:02X}, byte {n}");
            }
        }
        assert!(!SerialNumber(Dd20::new().serial).is_blank(), "the fake's");
    }

    #[test]
    fn connects_for_a_restore_onto_the_chip_the_backup_was_read_from() {
        let mut dd = Dd20::new();
        let saved = SerialNumber(dd.serial);
        let p = Programmer::connect_for_restore(&mut dd, &saved, Path::new(BACKUP));
        assert!(p.is_ok());
        drop(p);
        assert!(dd.progmode, "not in programming mode");
        assert!(!dd.updi_disabled, "UPDI was turned off");
    }

    #[test]
    fn a_restore_onto_another_chip_ends_with_updi_off_and_nothing_written() {
        let saved = SerialNumber(Dd20::new().serial);
        for n in 0..map::SERIAL_SIZE {
            let mut dd = Dd20::new();
            dd.serial[n] ^= 0x01;
            let chip = SerialNumber(dd.serial);
            let err = refused_a_restore(dd, &saved);
            assert!(err.contains(&chip.to_string()), "byte {n}: {err}");
            assert!(err.contains(&saved.to_string()), "byte {n}: {err}");
            assert!(err.contains(BACKUP), "byte {n}: {err}");
        }
    }

    #[test]
    fn a_restore_onto_a_chip_with_a_blank_serial_number_ends_with_updi_off() {
        let saved = SerialNumber(Dd20::new().serial);
        for (blank, name) in [(0xFF, "all FF"), (0x00, "all 00")] {
            let mut dd = Dd20::new();
            dd.serial = [blank; map::SERIAL_SIZE];
            let err = refused_a_restore(dd, &saved);
            assert!(err.contains(name), "{err}");
            assert!(err.contains(&saved.to_string()), "{err}");
            assert!(err.contains(BACKUP), "{err}");
        }
    }

    #[test]
    fn a_restore_of_a_backup_with_a_blank_serial_number_sends_nothing() {
        for (blank, name) in [(0xFF, "all FF"), (0x00, "all 00")] {
            let mut dd = Dd20::new();
            dd.serial = [blank; map::SERIAL_SIZE];
            let saved = SerialNumber(dd.serial);
            let Err(err) = Programmer::connect_for_restore(&mut dd, &saved, Path::new(BACKUP))
            else {
                panic!("connected for a restore");
            };
            assert!(err.contains(name), "{err}");
            assert!(err.contains(BACKUP), "{err}");
            assert_eq!(dd.sent, 0, "bytes went out");
        }
    }

    #[test]
    fn a_restore_checks_the_signature_before_the_serial_number() {
        let mut dd = Dd20::new();
        dd.signature = [0x1E, 0x95, 0x38];
        dd.serial[0] ^= 0x01;
        let err = refused_a_restore(dd, &SerialNumber(Dd20::new().serial));
        assert!(err.contains("1E 95 38"), "{err}");
    }

    #[test]
    fn writes_and_verifies_flash_then_starts_the_firmware() {
        let (image, _) = images();
        let mut p = Programmer::connect(with_contents(Dd20::new())).unwrap();
        p.enter_progmode().unwrap();
        p.erase_and_write_flash(&image).unwrap();
        p.verify(map::UPDI_FLASH, &image).unwrap();
        let dd = p.leave().unwrap();

        assert_eq!(&dd.flash[..image.len()], &image[..]);
        assert!(dd.flash[image.len()..].iter().all(|&b| b == 0xFF));
        assert_eq!(dd.chip_erases, 1);
        assert_eq!(dd.page_erases, 2);
        assert_eq!(dd.word_writes, 500);
        assert_eq!(dd.unerased_writes, 0);
        assert_eq!(dd.lost_while_busy, 0);
        assert!(dd.updi_disabled);
        assert!(!dd.in_reset);
    }

    #[test]
    fn an_odd_length_image_ends_on_a_whole_word() {
        let image = [0x12, 0x34, 0x56];
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.erase_and_write_flash(&image).unwrap();
        p.verify(map::UPDI_FLASH, &image).unwrap();
        let dd = p.leave().unwrap();
        assert_eq!(&dd.flash[..4], &[0x12, 0x34, 0x56, 0xFF]);
        assert_eq!(dd.word_writes, 2);
    }

    #[test]
    fn verify_catches_a_chip_erase_that_did_not_take() {
        let mut image = vec![0xFF; 2 * map::FLASH_PAGE];
        image[map::FLASH_PAGE] = 0x12;
        let mut dd = with_contents(Dd20::new());
        dd.ignores_erases = true;

        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.erase_and_write_flash(&image).unwrap();
        let err = p.verify(map::UPDI_FLASH, &image).unwrap_err();
        assert!(err.contains("verify failed at 0x800000"), "{err}");
    }

    #[test]
    fn verify_catches_a_page_erase_that_did_not_take() {
        let (mut image, _) = images();
        image.truncate(200);
        let mut dd = with_contents(Dd20::new());
        dd.ignores_erases = true;

        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.erase_and_write_flash(&image).unwrap();
        let err = p.verify(map::UPDI_FLASH, &image).unwrap_err();
        assert!(err.contains("verify failed"), "{err}");
        assert!(p.wire.unerased_writes > 0);
    }

    #[test]
    fn refuses_an_image_bigger_than_flash_before_erasing() {
        let mut p = Programmer::connect_for_programming(with_contents(Dd20::new())).unwrap();
        let err = p
            .erase_and_write_flash(&vec![0; map::FLASH_SIZE + 1])
            .unwrap_err();
        assert_eq!(err, "the image is 32769 bytes; the AVR32DD20 has 32768");
        let dd = p.leave().unwrap();
        assert_contents_kept(&dd);
    }

    #[test]
    fn skips_blank_flash_pages() {
        let mut image = vec![0xFF; 3 * map::FLASH_PAGE];
        image[2 * map::FLASH_PAGE] = 0x12;
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.erase_and_write_flash(&image).unwrap();
        let dd = p.leave().unwrap();
        assert_eq!(dd.page_erases, 1);
        assert_eq!(dd.word_writes, map::FLASH_PAGE / 2);
        assert_eq!(dd.flash[2 * map::FLASH_PAGE], 0x12);
    }

    #[test]
    fn writes_every_eeprom_byte_over_eesave_contents_blank_ones_too() {
        let (_, mut eeprom) = images();
        eeprom[128..].fill(0xFF);
        let mut dd = Dd20::new();
        dd.eesave = true;
        dd.eeprom.fill(0x42);

        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.erase_and_write_flash(&[0x12]).unwrap();
        assert!(p.wire.eeprom.iter().all(|&b| b == 0x42), "EESAVE didn't");
        p.write_eeprom(&eeprom).unwrap();
        p.verify(map::EEPROM, &eeprom).unwrap();
        let dd = p.leave().unwrap();
        assert_eq!(dd.eeprom_writes, 256);
        assert_eq!(dd.lost_while_busy, 0);
    }

    #[test]
    fn eeprom_erase_write_leaves_bytes_it_was_not_given() {
        let eeprom = vec![0x11; 40];
        let mut dd = Dd20::new();
        dd.eeprom.fill(0x42);

        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.write_eeprom(&eeprom).unwrap();
        let dd = p.leave().unwrap();
        assert_eq!(&dd.eeprom[..40], &eeprom[..]);
        assert!(dd.eeprom[40..].iter().all(|&b| b == 0x42));
        assert_eq!(dd.eeprom_writes, 40);
    }

    #[test]
    fn refuses_an_eeprom_image_bigger_than_eeprom_before_writing() {
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        let err = p.write_eeprom(&[0x12; 257]).unwrap_err();
        assert_eq!(err, "the EEPROM image is bigger than 256 bytes");
        assert_eq!(p.wire.nvm_operations(), 0);
    }

    #[test]
    fn writes_eeprom_and_reads_fuses_lock_bits_and_user_row() {
        let (_, eeprom) = images();
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.erase_and_write_flash(&[0x12]).unwrap();
        p.write_eeprom(&eeprom).unwrap();
        p.verify(map::EEPROM, &eeprom).unwrap();
        let fuses = p.read(map::FUSES, map::FUSES_SIZE).unwrap();
        // FUSE.SYSCFG0.
        assert_eq!(fuses[5], 0xD0);
        assert_eq!(p.read(map::LOCK, map::LOCK_SIZE).unwrap(), map::UNLOCKED);
        assert_eq!(
            p.read(map::USERROW, map::USERROW_SIZE).unwrap(),
            [0xFF; map::USERROW_SIZE]
        );
        let dd = p.leave().unwrap();
        assert_eq!(dd.eeprom_writes, 256);
        assert_eq!(dd.word_writes, 1);
    }

    #[test]
    fn reports_a_verify_mismatch() {
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        let err = p.verify(map::UPDI_FLASH, &[0xFF, 0x00]).unwrap_err();
        assert!(err.contains("0x800001"), "{err}");
    }

    #[test]
    fn updi_registers_and_instructions_are_the_datasheets() {
        // DS40002413B §36.3.1.3, figure 36-4, figure 36-8, §36.3.3, §36.5.6
        // and table 36-5 (the key least significant byte first).
        assert_eq!(SYNC, 0x55, "SYNCH");
        assert_eq!(ACK, 0x40, "ACK");

        assert_eq!(
            [LDS, LD, STS, ST, LDCS, REPEAT, STCS, KEY],
            [0x00, 0x20, 0x40, 0x60, 0x80, 0xA0, 0xC0, 0xE0],
            "opcodes"
        );
        assert_eq!(
            [
                LDS | ADDRESS_24 | DATA_8,
                STS | ADDRESS_24 | DATA_8,
                ST | PTR_ADDRESS | PTR_SIZE_24,
                LD | PTR_INC | DATA_8,
                ST | PTR_INC | DATA_16,
                KEY | KEY_SIB | SIB_16,
                KEY | KEY_64,
            ],
            [0x08, 0x48, 0x6A, 0x24, 0x65, 0xE5, 0xE0],
            "instructions"
        );

        assert_eq!(CS_STATUSA, 0x00, "STATUSA");
        assert_eq!(CS_CTRLA, 0x02, "CTRLA");
        assert_eq!(CS_CTRLB, 0x03, "CTRLB");
        assert_eq!(ASI_KEY_STATUS, 0x07, "ASI_KEY_STATUS");
        assert_eq!(ASI_RESET_REQ, 0x08, "ASI_RESET_REQ");
        assert_eq!(ASI_SYS_STATUS, 0x0B, "ASI_SYS_STATUS");

        assert_eq!(CTRLA_IBDLY, 0x80, "CTRLA.IBDLY");
        assert_eq!(CTRLA_RSD, 0x08, "CTRLA.RSD");
        assert_eq!(CTRLB_CCDETDIS, 0x08, "CTRLB.CCDETDIS");
        assert_eq!(CTRLB_UPDIDIS, 0x04, "CTRLB.UPDIDIS");
        assert_eq!(KEY_STATUS_NVMPROG, 0x10, "ASI_KEY_STATUS.NVMPROG");
        assert_eq!(SYS_STATUS_PROGSTART, 0x08, "ASI_SYS_STATUS.PROGSTART");
        assert_eq!(SYS_STATUS_LOCKSTATUS, 0x01, "ASI_SYS_STATUS.LOCKSTATUS");
        assert_eq!(RESET_REQ_APPLY, 0x59, "the reset signature");
        assert_eq!(
            u64::from_be_bytes(*KEY_NVMPROG),
            0x4E56_4D50_726F_6720,
            "the NVM programming key"
        );
        assert_eq!(SIB_NVM_P2, b"P:2", "NVM_VERSION");
    }

    #[test]
    fn addresses_are_three_bytes_and_stop_at_24_bits() {
        assert_eq!(address(0x80_1234).unwrap(), [0x34, 0x12, 0x80]);
        assert_eq!(address(0x1002).unwrap(), [0x02, 0x10, 0x00]);
        assert!(address(0x100_0000).is_err());
        assert_eq!(offset(map::UPDI_FLASH, 0x7FFF).unwrap(), 0x80_7FFF);
        assert!(offset(0xFF_FFFF, 1).is_err());
    }

    #[test]
    fn nvm_errors_say_which_in_the_datasheets_words() {
        assert_eq!(
            nvm_error(0x30),
            "the NVM controller reported error 3 (CMDCOLLISION, a command selected while \
             another was in place)"
        );
        assert!(nvm_error(0x12).contains("error 1 (INVALIDCMD"));
        assert!(nvm_error(0x21).contains("error 2 (WRITEPROTECT"));
        assert!(nvm_error(0x40).contains("error 4 (a code the datasheet calls reserved)"));
    }

    struct BadLine {
        fails: fn() -> io::Error,
        on_send: bool,
        breaks: usize,
    }

    impl Wire for &mut BadLine {
        fn send(&mut self, _: &[u8]) -> io::Result<()> {
            if self.on_send {
                Err((self.fails)())
            } else {
                Ok(())
            }
        }

        fn recv(&mut self, _: &mut [u8]) -> io::Result<()> {
            Err((self.fails)())
        }

        fn double_break(&mut self) -> io::Result<()> {
            self.breaks += 1;
            Ok(())
        }
    }

    #[test]
    fn a_chip_that_says_nothing_reads_as_no_reply_after_a_double_break() {
        let mut line = BadLine {
            fails: || io::ErrorKind::TimedOut.into(),
            on_send: false,
            breaks: 0,
        };
        let Err(err) = Programmer::connect(&mut line) else {
            panic!("connected to nothing");
        };
        assert_eq!(
            err,
            "no reply from the chip. Check the probe's contact, the pad order and that the \
             cell is out"
        );
        assert_eq!(line.breaks, 1, "one double break and a second try");
    }

    #[test]
    fn a_line_that_does_not_echo_is_not_reported_as_no_reply() {
        let mut line = BadLine {
            fails: || io::Error::other("nothing echoed back. Something's wrong"),
            on_send: true,
            breaks: 0,
        };
        let Err(err) = Programmer::connect(&mut line) else {
            panic!("connected to nothing");
        };
        assert_eq!(err, "serial: nothing echoed back. Something's wrong");
        assert!(!err.contains("no reply"), "{err}");
    }

    #[test]
    fn only_a_timeout_is_no_reply_and_any_other_error_keeps_its_words() {
        let err = io_error(io::Error::new(io::ErrorKind::TimedOut, "took too long"));
        assert!(err.starts_with("no reply from the chip."), "{err}");
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::Other,
        ] {
            let err = io_error(io::Error::new(kind, "the adapter's gone"));
            assert_eq!(err, "serial: the adapter's gone", "{kind:?}");
        }
    }

    #[test]
    fn refuses_a_link_whose_statusa_reads_zero_after_a_double_break() {
        let mut dd = Dd20::new();
        dd.statusa = 0;
        let Err(err) = Programmer::connect(&mut dd) else {
            panic!("connected");
        };
        assert_eq!(err, "the UPDI link came up but STATUSA reads 0");
        assert_eq!(dd.breaks, 1, "one double break and a second try");
        assert!(!dd.progmode, "the key went out");
        assert_eq!(dd.resets, 0, "the chip was reset");
    }

    #[test]
    fn a_chip_that_refuses_the_key_is_not_reset_and_ends_with_updi_off() {
        let mut dd = Dd20::new();
        dd.ignores_key = true;

        let mut p = Programmer::connect(&mut dd).unwrap();
        let err = p.enter_progmode().unwrap_err();
        assert_eq!(err, "the chip didn't accept the NVM programming key");
        assert_eq!(dd.resets, 0, "the chip was reset without the key");

        let mut dd = with_contents(dd);
        let Err(err) = Programmer::connect_for_programming(&mut dd) else {
            panic!("connected for programming");
        };
        assert_eq!(err, "the chip didn't accept the NVM programming key");
        assert_eq!(dd.resets, 1, "only the reset that leaves");
        dd.assert_let_go();
        assert_contents_kept(&dd);
    }

    #[test]
    fn only_a_chip_that_reads_unlocked_and_progstart_is_written_to() {
        for status in [
            0,
            SYS_STATUS_LOCKSTATUS,
            SYS_STATUS_LOCKSTATUS | SYS_STATUS_PROGSTART,
        ] {
            let mut dd = Dd20::new();
            dd.sys_status_after_reset = Some(status);
            let err = refused(dd);
            assert!(
                err.starts_with("the chip didn't enter programming mode."),
                "{status:#04x}: {err}"
            );
            assert!(err.contains("which this tool won't do"), "{err}");
        }

        // 0x02 is BOOTDONE and 0x20 is SYSRST (DS §36.5.9).
        for other in [0x02, 0x20] {
            let mut dd = Dd20::new();
            dd.sys_status_after_reset = Some(other | SYS_STATUS_PROGSTART);
            assert!(
                Programmer::connect_for_programming(dd).is_ok(),
                "{other:#04x}"
            );
        }
    }

    #[test]
    fn a_locked_chip_is_given_the_whole_timeout_and_then_refused() {
        let mut dd = Dd20::new();
        dd.lock = fake::LOCKED;
        let mut p = Programmer::connect(dd).unwrap();
        let before = p.wire.elapsed;
        let err = p.enter_progmode().unwrap_err();
        assert!(err.contains("If it's locked, unlocking means erasing it"));
        let waited = p.wire.elapsed.checked_sub(before).unwrap();
        assert!(waited >= PROGMODE_TIMEOUT, "gave up after {waited:?}");
        assert_eq!(p.wire.nvm_operations(), 0);
    }

    #[test]
    fn already_in_programming_mode_but_locked_is_refused() {
        let mut p = Programmer::connect(Dd20::new()).unwrap();
        p.wire.progmode = true;
        p.wire.lock = fake::LOCKED;
        let err = p.enter_progmode().unwrap_err();
        assert!(err.contains("unlocking means erasing it"), "{err}");
        assert_eq!(p.wire.resets, 1, "never sent the key and reset");
    }

    pub fn images() -> (Vec<u8>, Vec<u8>) {
        let flash = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let eeprom = (0..=255).collect();
        (flash, eeprom)
    }

    fn write_both(dd: Dd20) -> Dd20 {
        let (flash, eeprom) = images();
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.erase_and_write_flash(&flash).unwrap();
        p.verify(map::UPDI_FLASH, &flash).unwrap();
        p.write_eeprom(&eeprom).unwrap();
        p.verify(map::EEPROM, &eeprom).unwrap();
        p.leave().unwrap()
    }

    #[test]
    fn waits_out_every_erase_and_write_before_starting_the_next() {
        // DS table 38-8's typical times, then its maximums.
        for (word, erase, eeprom) in [(70, 10, 10), (75, 45, 45)] {
            for round_trip in [Duration::from_micros(100), Duration::from_millis(16)] {
                let mut dd = with_contents(Dd20::new());
                dd.word_time = Duration::from_micros(word);
                dd.page_erase_time = Duration::from_millis(erase);
                dd.chip_erase_time = Duration::from_millis(erase);
                dd.eeprom_time = Duration::from_millis(eeprom);
                dd.round_trip = round_trip;
                let dd = write_both(dd);
                let case = format!("{word} µs, {erase} ms, {round_trip:?} a round trip");
                assert_eq!(dd.lost_while_busy, 0, "{case}");
                assert_eq!(dd.collisions, 0, "{case}");
                assert_eq!(dd.chip_erases, 1, "{case}");
                assert_eq!(dd.page_erases, 2, "{case}");
                assert_eq!(dd.word_writes, 500, "{case}");
                assert_eq!(dd.eeprom_writes, 256, "{case}");
                if round_trip < dd.page_erase_time {
                    assert!(dd.busy_polls > 0, "{case}: never saw it busy");
                }
            }
        }
    }

    #[test]
    fn words_that_arrive_faster_than_flash_writes_them_are_lost() {
        let image = vec![0x12; map::FLASH_PAGE];
        let mut dd = Dd20::new();
        dd.word_time = Duration::from_micros(75);
        dd.byte_time = Duration::from_micros(10);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.erase_and_write_flash(&image).unwrap();
        assert!(p.verify(map::UPDI_FLASH, &image).is_err());
        assert!(p.wire.lost_while_busy > 0);
    }

    #[test]
    fn waits_for_eeprom_as_well_as_flash() {
        let (_, eeprom) = images();
        let mut dd = with_contents(Dd20::new());
        dd.eeprom_time = Duration::from_millis(30);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.write_eeprom(&eeprom[..8]).unwrap();
        p.verify(map::EEPROM, &eeprom[..8]).unwrap();
        let dd = p.leave().unwrap();
        assert_eq!(dd.lost_while_busy, 0);
        assert!(dd.busy_polls >= 8 * 15, "{} busy polls", dd.busy_polls);
        assert!(
            dd.flash[..0x3B7E].iter().all(|&b| b == 0x42),
            "flash changed"
        );
    }

    #[test]
    fn the_fake_chip_loses_a_store_that_arrives_while_it_is_busy() {
        let mut dd = Dd20::new();
        dd.chip_erase_time = Duration::from_millis(50);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.nvm_command(NVM_CHIP_ERASE).unwrap();
        assert_ne!(p.lds(map::NVMCTRL_STATUS).unwrap() & NVM_FBUSY, 0);
        p.nvm_command(NVM_NO_COMMAND).unwrap();
        p.nvm_command(NVM_FLASH_WRITE).unwrap_or_default();
        assert_eq!(p.wire.collisions, 1, "a command selected while busy");

        p.wire.clear_error();
        p.sts(map::UPDI_FLASH, 0x12).unwrap();
        assert_eq!(p.wire.lost_while_busy, 1);
        assert_eq!(p.wire.word_writes, 0);

        p.wait_nvm().unwrap();
        p.write_flash_page(map::UPDI_FLASH, &[0x12]).unwrap();
        assert_eq!(p.read(map::UPDI_FLASH, 2).unwrap(), [0x12, 0xFF]);
        assert_eq!(p.wire.lost_while_busy, 1);
    }

    #[test]
    fn a_command_sent_while_busy_is_a_collision() {
        let mut dd = Dd20::new();
        dd.chip_erase_time = Duration::from_millis(50);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.nvm_command(NVM_CHIP_ERASE).unwrap();
        p.nvm_command(NVM_NO_COMMAND).unwrap();
        p.nvm_command(NVM_FLASH_PAGE_ERASE).unwrap();
        let err = p.wait_nvm().unwrap_err();
        assert!(err.contains("error 3 (CMDCOLLISION"), "{err}");
    }

    #[test]
    fn a_change_of_command_without_nocmd_between_is_a_collision() {
        // Unconfirmed: the datasheet is silent, so this follows the ATDF's code 3.
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.nvm_command(NVM_FLASH_PAGE_ERASE).unwrap();
        p.nvm_command(NVM_FLASH_WRITE).unwrap();
        let err = p.wait_nvm().unwrap_err();
        assert!(err.contains("CMDCOLLISION"), "{err}");
        p.nvm_command(NVM_NO_COMMAND).unwrap();
        assert!(p.wait_nvm().is_err());
    }

    #[test]
    fn a_store_with_no_command_selected_is_refused() {
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.sts(map::UPDI_FLASH, 0x12).unwrap();
        let err = p.wait_nvm().unwrap_err();
        assert!(err.contains("INVALIDCMD"), "{err}");
        assert_eq!(p.read(map::UPDI_FLASH, 1).unwrap(), [0xFF]);
    }

    #[test]
    fn the_fake_counts_words_written_over_flash_that_was_not_erased() {
        let mut dd = Dd20::new();
        dd.flash[..2].copy_from_slice(&[0xF0, 0x0F]);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.write_flash_page(map::UPDI_FLASH, &[0x3C, 0x3C]).unwrap();
        assert_eq!(p.read(map::UPDI_FLASH, 2).unwrap(), [0x30, 0x0C]);
        assert_eq!(p.wire.unerased_writes, 2);
    }

    #[test]
    fn the_fake_erases_a_page_at_a_time() {
        let mut dd = Dd20::new();
        dd.flash.fill(0x42);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.erase_flash_page(map::UPDI_FLASH + 0x200).unwrap();
        let dd = p.leave().unwrap();
        assert!(dd.flash[..0x200].iter().all(|&b| b == 0x42));
        assert!(dd.flash[0x200..0x400].iter().all(|&b| b == 0xFF));
        assert!(dd.flash[0x400..].iter().all(|&b| b == 0x42));
    }

    #[test]
    fn a_locked_chip_shows_its_signature_and_nothing_else() {
        let mut dd = with_contents(Dd20::new());
        dd.lock = fake::LOCKED;
        let mut p = Programmer::connect(dd).unwrap();
        assert_eq!(p.read(map::SIGNATURE, 3).unwrap(), map::AVR32DD20);
        assert_eq!(p.read(map::UPDI_FLASH, 2).unwrap(), [0, 0]);
        assert_eq!(p.read(map::LOCK, 4).unwrap(), [0; 4]);
    }

    #[test]
    fn a_chip_that_never_stops_being_busy_times_out_with_an_error() {
        let mut dd = with_contents(Dd20::new());
        dd.chip_erase_time = Duration::MAX;
        dd.round_trip = Duration::from_micros(100);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        let before = p.wire.elapsed;
        let err = p.erase_and_write_flash(&[0x12; 200]).unwrap_err();
        assert_eq!(err, "timed out waiting for the NVM controller");
        let waited = p.wire.elapsed.checked_sub(before).unwrap();
        assert!(waited >= Duration::from_secs(1), "gave up after {waited:?}");

        let dd = p.leave().unwrap();
        assert_eq!(dd.page_erases + dd.word_writes, 0);
        assert_eq!(dd.lost_while_busy, 0);
        assert_eq!(dd.collisions, 0);
        assert!(dd.updi_disabled, "UPDI was left on");
    }

    #[test]
    fn a_write_error_is_reported_and_stops_the_flash() {
        let (image, _) = images();
        let mut dd = Dd20::new();
        dd.write_error_at = Some(1 + map::FLASH_PAGE / 2);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        let err = p.erase_and_write_flash(&image).unwrap_err();
        assert!(err.contains("error 2 (WRITEPROTECT"), "{err}");

        let dd = p.leave().unwrap();
        assert_eq!(dd.page_erases, 1);
        assert_eq!(dd.word_writes, map::FLASH_PAGE / 2);
        let written = map::FLASH_PAGE;
        assert_eq!(&dd.flash[..written], &image[..written]);
        assert!(dd.flash[written..].iter().all(|&b| b == 0xFF));
        assert_eq!(dd.lost_while_busy, 0);
    }

    #[test]
    fn a_write_error_in_eeprom_is_reported_too() {
        let (_, eeprom) = images();
        let mut dd = Dd20::new();
        dd.write_error_at = Some(0);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        let err = p.write_eeprom(&eeprom).unwrap_err();
        assert!(err.contains("WRITEPROTECT"), "{err}");
        assert_eq!(p.wire.eeprom_writes, 0);
    }

    /// Written from the same datasheet as the programmer, so it catches slips
    /// and not misreadings.
    pub(crate) mod fake {
        use super::super::*;
        use crate::world::{FACTORY_FUSES, FUSE_BYTES};
        use kr1k::regs::{
            LOCK_KEY_RWLOCK, NVMCTRL_CMD_NOOP as NVM_NOOP,
            NVMCTRL_ERROR_ONGOINGPROG as NVM_ERROR_RESERVED,
        };
        use std::collections::VecDeque;
        use std::ops::Range;

        pub const LOCKED: [u8; map::LOCK_SIZE] = LOCK_KEY_RWLOCK.to_le_bytes();

        /// DS table 36-5.
        const KEY_CHIP_ERASE: &[u8; 8] = b"NVMErase";
        const KEY_USERROW: &[u8; 8] = b"NVMUs&te";

        const fn span(start: u32, size: usize) -> Range<u32> {
            start..start + size as u32
        }
        const fn data(start: u16, size: usize) -> Range<u32> {
            span(start as u32, size)
        }
        const CTRLA: u32 = map::NVMCTRL_CTRLA as u32;
        const STATUS: u32 = map::NVMCTRL_STATUS as u32;
        const FLASH: Range<u32> = span(map::UPDI_FLASH, map::FLASH_SIZE);
        const EEPROM: Range<u32> = data(map::EEPROM, map::EEPROM_SIZE);
        const SIGNATURE: Range<u32> = data(map::SIGNATURE, 3);
        const SERIAL: Range<u32> = data(map::SERIAL, map::SERIAL_SIZE);
        const FUSES: Range<u32> = data(map::FUSES, map::FUSES_SIZE);
        const LOCK: Range<u32> = data(map::LOCK, map::LOCK_SIZE);
        const USERROW: Range<u32> = data(map::USERROW, map::USERROW_SIZE);
        const SRAM_SPACE: Range<u32> = span(
            kr1k::regs::mem::INTERNAL_SRAM_START as u32,
            kr1k::regs::mem::INTERNAL_SRAM_SIZE,
        );
        /// UPDI can't write any of SIGROW (DS table 8-5).
        const SIGROW: Range<u32> = data(map::SIGNATURE, 0x80);

        fn at(range: &Range<u32>, addr: u32) -> usize {
            (addr - range.start) as usize
        }

        enum Expect {
            Sync,
            Opcode,
            CsValue(u8),
            Key(Vec<u8>),
            LdsAddress(Vec<u8>),
            StsAddress(Vec<u8>),
            StsData(u32),
            PtrAddress(Vec<u8>),
            RepeatCount,
            /// Words left, and the low byte of the one arriving.
            StIncWord(usize, Option<u8>),
        }

        #[expect(
            clippy::struct_excessive_bools,
            reason = "the fake chip's state flags, which change independently"
        )]
        pub struct Dd20 {
            pub flash: Vec<u8>,
            pub eeprom: Vec<u8>,
            pub sram: Vec<u8>,
            pub signature: [u8; 3],
            /// The bytes are made up.
            pub serial: [u8; map::SERIAL_SIZE],
            /// As avrdude read them off the torch.
            pub fuses: [u8; map::FUSES_SIZE],
            pub lock: [u8; map::LOCK_SIZE],
            pub userrow: [u8; map::USERROW_SIZE],
            pub sib: [u8; 16],
            pub asleep: bool,
            pub breaks: usize,
            pub sent: usize,
            pub keys: usize,
            pub data_accesses: usize,
            pub stores_in: usize,
            pub updi_disabled: bool,
            pub in_reset: bool,
            pub resets: usize,
            pub progmode: bool,
            pub start_up: Duration,
            /// Per `recv`.
            pub round_trip: Duration,
            pub byte_time: Duration,
            pub elapsed: Duration,
            clock_start: Instant,
            started_at: Duration,
            pub eesave: bool,
            pub ignores_erases: bool,
            pub chip_erases: usize,
            pub page_erases: usize,
            pub word_writes: usize,
            pub eeprom_writes: usize,
            pub unerased_writes: usize,
            pub statusa: u8,
            pub ignores_key: bool,
            /// Nothing says a real chip reads any of these.
            pub sys_status_after_reset: Option<u8>,
            /// Defaults are DS table 38-8's typical times.
            pub word_time: Duration,
            pub page_erase_time: Duration,
            pub chip_erase_time: Duration,
            pub eeprom_time: Duration,
            pub busy_polls: usize,
            /// Dropping these is a strict reading of §11.3.2.1 and §36.5.3.
            pub lost_while_busy: usize,
            pub collisions: usize,
            /// Counting from 0.
            pub write_error_at: Option<usize>,
            stores: usize,
            command: u8,
            /// Stays until written to 0 or a reset (DS §11.5.3).
            error: u8,
            busy: u8,
            busy_until: Duration,
            expect: Expect,
            replies: VecDeque<u8>,
            ctrla: u8,
            key_status: u8,
            ptr: u32,
            repeat: usize,
        }

        impl Dd20 {
            pub fn new() -> Self {
                let mut fuses = [0xFF; map::FUSES_SIZE];
                fuses[..FUSE_BYTES].copy_from_slice(&FACTORY_FUSES);
                Self {
                    flash: vec![0xFF; map::FLASH_SIZE],
                    eeprom: vec![0xFF; map::EEPROM_SIZE],
                    sram: vec![0; kr1k::regs::mem::INTERNAL_SRAM_SIZE],
                    signature: map::AVR32DD20,
                    serial: [
                        0x37, 0x51, 0x3A, 0x07, 0xC4, 0x19, 0xE2, 0x6D, 0x80, 0x2B, 0xF5, 0x12,
                        0x00, 0x00, 0x00, 0x00,
                    ],
                    fuses,
                    lock: map::UNLOCKED,
                    userrow: [0xFF; map::USERROW_SIZE],
                    // Only NVM_VERSION is confirmed; the rest is DS figure 36-20.
                    sib: *b"AVR     P:2D:1-3",
                    asleep: false,
                    breaks: 0,
                    sent: 0,
                    keys: 0,
                    data_accesses: 0,
                    stores_in: 0,
                    updi_disabled: false,
                    in_reset: false,
                    resets: 0,
                    progmode: false,
                    start_up: Duration::ZERO,
                    round_trip: Duration::from_millis(1),
                    // 12 bits at 115,200 baud.
                    byte_time: Duration::from_nanos(104_167),
                    elapsed: Duration::ZERO,
                    clock_start: Instant::now(),
                    started_at: Duration::ZERO,
                    eesave: false,
                    ignores_erases: false,
                    chip_erases: 0,
                    page_erases: 0,
                    word_writes: 0,
                    eeprom_writes: 0,
                    unerased_writes: 0,
                    statusa: 0x30, // UPDI revision 3
                    ignores_key: false,
                    sys_status_after_reset: None,
                    word_time: Duration::from_micros(70),
                    page_erase_time: Duration::from_millis(10),
                    chip_erase_time: Duration::from_millis(11),
                    eeprom_time: Duration::from_micros(10_070),
                    busy_polls: 0,
                    lost_while_busy: 0,
                    collisions: 0,
                    write_error_at: None,
                    stores: 0,
                    command: NVM_NO_COMMAND,
                    error: 0,
                    busy: 0,
                    busy_until: Duration::ZERO,
                    expect: Expect::Sync,
                    replies: VecDeque::new(),
                    ctrla: 0,
                    key_status: 0,
                    ptr: 0,
                    repeat: 1,
                }
            }

            pub fn nvm_operations(&self) -> usize {
                self.chip_erases + self.page_erases + self.word_writes + self.eeprom_writes
            }

            pub fn assert_let_go(&self) {
                assert!(!self.in_reset, "the chip was left in reset");
                assert!(!self.progmode, "the chip was left in programming mode");
                assert!(self.updi_disabled, "UPDI was left on");
            }

            pub fn clear_error(&mut self) {
                self.error = 0;
            }

            fn locked(&self) -> bool {
                self.lock != map::UNLOCKED
            }

            fn byte(&mut self, b: u8) {
                self.expect = match std::mem::replace(&mut self.expect, Expect::Sync) {
                    Expect::Sync => {
                        assert_eq!(b, SYNC, "every instruction starts with SYNC");
                        Expect::Opcode
                    }
                    Expect::Opcode => self.opcode(b),
                    Expect::CsValue(reg) => {
                        self.stcs(reg, b);
                        Expect::Sync
                    }
                    Expect::Key(mut key) => {
                        key.push(b);
                        if key.len() < 8 {
                            Expect::Key(key)
                        } else {
                            key.reverse();
                            assert_ne!(key, KEY_CHIP_ERASE, "the Chip Erase key went out");
                            assert_ne!(key, KEY_USERROW, "the USERROW-Write key went out");
                            if key == KEY_NVMPROG && !self.ignores_key {
                                self.key_status |= KEY_STATUS_NVMPROG;
                            }
                            Expect::Sync
                        }
                    }
                    Expect::LdsAddress(mut a) => {
                        a.push(b);
                        if a.len() < 3 {
                            Expect::LdsAddress(a)
                        } else {
                            let v = self.load(address24(&a));
                            self.replies.push_back(v);
                            Expect::Sync
                        }
                    }
                    Expect::StsAddress(mut a) => {
                        a.push(b);
                        if a.len() < 3 {
                            Expect::StsAddress(a)
                        } else {
                            self.ack();
                            Expect::StsData(address24(&a))
                        }
                    }
                    Expect::StsData(addr) => {
                        self.store(addr, &[b]);
                        self.ack();
                        Expect::Sync
                    }
                    Expect::PtrAddress(mut a) => {
                        a.push(b);
                        if a.len() < 3 {
                            Expect::PtrAddress(a)
                        } else {
                            self.ptr = address24(&a);
                            self.ack();
                            Expect::Sync
                        }
                    }
                    Expect::RepeatCount => {
                        self.repeat = usize::from(b) + 1;
                        Expect::Sync
                    }
                    Expect::StIncWord(left, None) => Expect::StIncWord(left, Some(b)),
                    Expect::StIncWord(left, Some(low)) => {
                        assert_eq!(self.ptr % 2, 0, "a word store at an odd address");
                        self.store(self.ptr, &[low, b]);
                        self.ptr += 2;
                        self.ack();
                        if left > 1 {
                            Expect::StIncWord(left - 1, None)
                        } else {
                            Expect::Sync
                        }
                    }
                };
            }

            fn opcode(&mut self, op: u8) -> Expect {
                let repeat = std::mem::replace(&mut self.repeat, 1);
                if matches!(op, 0x08 | 0x48 | 0x6A | 0x24 | 0x65) {
                    self.data_accesses += 1;
                }
                match op {
                    _ if op & 0xF0 == LDCS => {
                        let v = self.ldcs(op & 0x0F);
                        self.replies.push_back(v);
                        Expect::Sync
                    }
                    _ if op & 0xF0 == STCS => Expect::CsValue(op & 0x0F),
                    0xE0 => {
                        self.keys += 1;
                        Expect::Key(Vec::new())
                    }
                    0xE5 => {
                        self.replies.extend(self.sib);
                        Expect::Sync
                    }
                    0x08 => Expect::LdsAddress(Vec::new()),
                    0x48 => Expect::StsAddress(Vec::new()),
                    0x6A => Expect::PtrAddress(Vec::new()),
                    0xA0 => Expect::RepeatCount,
                    0x24 => {
                        for _ in 0..repeat {
                            let v = self.load(self.ptr);
                            self.replies.push_back(v);
                            self.ptr += 1;
                        }
                        Expect::Sync
                    }
                    0x65 => Expect::StIncWord(repeat, None),
                    _ => panic!("unexpected opcode 0x{op:02X}"),
                }
            }

            fn ack(&mut self) {
                if self.ctrla & CTRLA_RSD == 0 {
                    self.replies.push_back(ACK);
                }
            }

            fn ldcs(&self, reg: u8) -> u8 {
                match (reg, self.sys_status_after_reset) {
                    (ASI_SYS_STATUS, Some(status)) if self.resets > 0 => return status,
                    _ => {}
                }
                match reg {
                    CS_STATUSA => self.statusa,
                    ASI_KEY_STATUS => self.key_status,
                    ASI_SYS_STATUS if self.elapsed < self.started_at => SYS_STATUS_LOCKSTATUS,
                    ASI_SYS_STATUS => {
                        let lock = if self.locked() {
                            SYS_STATUS_LOCKSTATUS
                        } else {
                            0
                        };
                        let prog = if self.progmode {
                            SYS_STATUS_PROGSTART
                        } else {
                            0
                        };
                        lock | prog
                    }
                    _ => 0,
                }
            }

            /// DS §11.5.1, §11.5.3 and §11.3.3.
            fn system_reset(&mut self) {
                self.command = NVM_NO_COMMAND;
                self.error = 0;
                self.busy_until = self.elapsed;
            }

            fn stcs(&mut self, reg: u8, value: u8) {
                match reg {
                    CS_CTRLA => self.ctrla = value,
                    CS_CTRLB => {
                        self.updi_disabled = value & CTRLB_UPDIDIS != 0;
                        if self.updi_disabled {
                            // A System Reset (DS §36.3.2.2.2).
                            self.progmode = false;
                            self.key_status = 0;
                            self.system_reset();
                        }
                    }
                    ASI_RESET_REQ if value == RESET_REQ_APPLY => self.in_reset = true,
                    ASI_RESET_REQ => {
                        if self.in_reset {
                            self.resets += 1;
                            self.system_reset();
                        }
                        self.in_reset = false;
                        self.started_at = self.elapsed + self.start_up;
                        // NVMPROG needs the lock bits cleared (DS table 36-4),
                        // and the key status clears as PROGSTART sets (§36.5.5).
                        self.progmode = self.key_status & KEY_STATUS_NVMPROG != 0 && !self.locked();
                        self.key_status = 0;
                    }
                    _ => {}
                }
            }

            fn is_busy(&self) -> bool {
                self.elapsed < self.busy_until
            }

            fn nvm_status(&mut self) -> u8 {
                if self.is_busy() {
                    self.busy_polls += 1;
                    self.busy | self.error
                } else {
                    self.error
                }
            }

            fn load(&mut self, addr: u32) -> u8 {
                assert!(
                    !data(map::FLASH, map::FLASH_SIZE).contains(&addr),
                    "flash read through the data-space window at 0x{addr:04X}, not at 0x800000"
                );
                // Locked, only the device ID is valid (DS §8.6, table 8-6).
                if self.locked() && !SIGNATURE.contains(&addr) {
                    return 0;
                }
                match addr {
                    STATUS => self.nvm_status(),
                    _ if FLASH.contains(&addr) => self.flash[at(&FLASH, addr)],
                    _ if EEPROM.contains(&addr) => self.eeprom[at(&EEPROM, addr)],
                    _ if SIGNATURE.contains(&addr) => self.signature[at(&SIGNATURE, addr)],
                    _ if SERIAL.contains(&addr) => self.serial[at(&SERIAL, addr)],
                    _ if FUSES.contains(&addr) => self.fuses[at(&FUSES, addr)],
                    _ if LOCK.contains(&addr) => self.lock[at(&LOCK, addr)],
                    _ if USERROW.contains(&addr) => self.userrow[at(&USERROW, addr)],
                    _ if SRAM_SPACE.contains(&addr) => self.sram[at(&SRAM_SPACE, addr)],
                    _ => 0,
                }
            }

            fn store(&mut self, addr: u32, bytes: &[u8]) {
                self.stores_in += 1;
                assert!(
                    self.progmode,
                    "store to 0x{addr:06X} outside programming mode"
                );
                for range in [&FUSES, &LOCK, &USERROW, &SIGROW] {
                    assert!(!range.contains(&addr), "store to 0x{addr:06X}");
                }
                match addr {
                    CTRLA => self.nvm_command(bytes[0]),
                    STATUS => self.error = bytes[0] & NVM_ERROR,
                    _ if FLASH.contains(&addr) || EEPROM.contains(&addr) => {
                        self.program(addr, bytes);
                    }
                    _ => panic!("store to 0x{addr:06X}"),
                }
            }

            fn start(&mut self, flags: u8, time: Duration) {
                self.busy = flags;
                self.busy_until = self.elapsed.saturating_add(time);
            }

            /// Only CHER starts at once (DS §11.5.1).
            fn nvm_command(&mut self, command: u8) {
                if command == NVM_NO_COMMAND || command == NVM_NOOP {
                    self.command = command;
                    return;
                }
                let selected = self.command != NVM_NO_COMMAND && self.command != NVM_NOOP;
                if self.is_busy() || selected {
                    self.error = NVM_ERROR_COLLISION;
                    self.collisions += 1;
                    return;
                }
                self.command = command;
                match command {
                    NVM_CHIP_ERASE if self.ignores_erases => {}
                    NVM_CHIP_ERASE => {
                        // Which flags a real chip raises is unconfirmed.
                        self.start(NVM_FBUSY | NVM_EEBUSY, self.chip_erase_time);
                        self.flash.fill(0xFF);
                        if !self.eesave {
                            self.eeprom.fill(0xFF);
                        }
                        self.chip_erases += 1;
                    }
                    NVM_FLASH_WRITE | NVM_FLASH_PAGE_ERASE | NVM_EEPROM_ERASE_WRITE => {}
                    _ => panic!("unexpected NVM command 0x{command:02X}"),
                }
            }

            fn program(&mut self, addr: u32, bytes: &[u8]) {
                if self.is_busy() {
                    self.lost_while_busy += 1;
                    return;
                }
                let store = self.stores;
                self.stores += 1;
                if self.write_error_at == Some(store) {
                    self.error = NVM_ERROR_WRITE_PROTECT;
                    return;
                }
                let flash = FLASH.contains(&addr);
                match self.command {
                    NVM_NO_COMMAND | NVM_NOOP => self.error = NVM_ERROR_INVALID_COMMAND,
                    NVM_FLASH_WRITE if flash => {
                        let start = at(&FLASH, addr);
                        for (cell, &b) in
                            self.flash[start..start + bytes.len()].iter_mut().zip(bytes)
                        {
                            if *cell != 0xFF {
                                self.unerased_writes += 1;
                            }
                            *cell &= b;
                        }
                        self.word_writes += 1;
                        self.start(NVM_FBUSY, self.word_time);
                    }
                    NVM_FLASH_PAGE_ERASE if flash => {
                        self.start(NVM_FBUSY, self.page_erase_time);
                        if !self.ignores_erases {
                            let page = at(&FLASH, addr) / map::FLASH_PAGE * map::FLASH_PAGE;
                            self.flash[page..page + map::FLASH_PAGE].fill(0xFF);
                            self.page_erases += 1;
                        }
                    }
                    NVM_EEPROM_ERASE_WRITE if !flash => {
                        assert_eq!(bytes.len(), 1, "EEPROM writes a byte at a time");
                        self.eeprom[at(&EEPROM, addr)] = bytes[0];
                        self.eeprom_writes += 1;
                        self.start(NVM_EEBUSY, self.eeprom_time);
                    }
                    command => panic!(
                        "the fake doesn't model command 0x{command:02X} on a store to \
                         0x{addr:06X}"
                    ),
                }
                debug_assert_ne!(self.error, NVM_ERROR_RESERVED);
            }
        }

        fn address24(a: &[u8]) -> u32 {
            u32::from_le_bytes([a[0], a[1], a[2], 0])
        }

        impl Wire for Dd20 {
            fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
                self.sent += bytes.len();
                for &b in bytes {
                    self.elapsed += self.byte_time;
                    self.byte(b);
                }
                Ok(())
            }

            fn recv(&mut self, buf: &mut [u8]) -> io::Result<()> {
                self.elapsed += self.round_trip;
                if self.asleep {
                    self.replies.clear();
                    self.expect = Expect::Sync;
                    return Err(io::ErrorKind::TimedOut.into());
                }
                for slot in buf {
                    *slot = self
                        .replies
                        .pop_front()
                        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
                }
                Ok(())
            }

            fn double_break(&mut self) -> io::Result<()> {
                self.breaks += 1;
                self.asleep = false;
                self.expect = Expect::Sync;
                self.replies.clear();
                Ok(())
            }

            fn now(&self) -> Instant {
                self.clock_start + self.elapsed
            }
        }

        impl Wire for &mut Dd20 {
            fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
                (**self).send(bytes)
            }

            fn recv(&mut self, buf: &mut [u8]) -> io::Result<()> {
                (**self).recv(buf)
            }

            fn double_break(&mut self) -> io::Result<()> {
                (**self).double_break()
            }

            fn now(&self) -> Instant {
                (**self).now()
            }
        }
    }
}
