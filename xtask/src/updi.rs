//! A UPDI programmer for the AVR32DD20, spoken over a serial adapter.
//!
//! UPDI is Microchip's one-wire debug and programming interface. A "serial
//! UPDI" programmer is a USB-serial chip with TX and RX joined onto that one
//! wire, so everything sent comes straight back as an echo before the
//! chip's reply. [`Wire`] hides the echo; this module only sees replies.
//!
//! The protocol is in the AVR DD datasheet (DS40002413B, chapter 36, UPDI),
//! and the NVM controller in chapter 11 (NVMCTRL). The DD has 24-bit UPDI
//! addressing with flash at 0x80_0000 (§36.2.2), and the NVM controller
//! pymcuprog calls NVM P:2, which writes flash a word at a time with no page
//! buffer. The sequences here (link set-up, NVM key, reset, chip erase, page
//! erase, word writes, EEPROM erase-writes) follow Microchip's pymcuprog
//! (`serialupdi/nvmp2.py`), the reference serial UPDI implementation, and
//! agree with avrdude's serialupdi. Register addresses, the memory map and NVM
//! commands come from the AVR32DD20 ATDF in `vendor/`, through `kr1k::regs`.
//! The UPDI instruction set, and where flash sits in UPDI's address space,
//! aren't in the ATDF, so those stay hand-written.
//!
//! This deliberately can't write fuses, the lock bits or the user row, and
//! can't unlock a locked chip. None of those are needed to flash the KR1K,
//! and a wrong fuse can leave it unrecoverable without a high-voltage
//! programmer.

use std::fmt;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

// NVMCTRL.CTRLA commands and NVMCTRL.STATUS fields.
use kr1k::regs::{
    NVMCTRL_CMD_CHER as NVM_CHIP_ERASE, NVMCTRL_CMD_EEERWR as NVM_EEPROM_ERASE_WRITE,
    NVMCTRL_CMD_FLPER as NVM_FLASH_PAGE_ERASE, NVMCTRL_CMD_FLWR as NVM_FLASH_WRITE,
    NVMCTRL_CMD_NONE as NVM_NO_COMMAND, NVMCTRL_EEBUSY as NVM_EEBUSY, NVMCTRL_ERROR as NVM_ERROR,
    NVMCTRL_ERROR_DOUBLESELECT as NVM_ERROR_COLLISION,
    NVMCTRL_ERROR_ILLEGALCMD as NVM_ERROR_INVALID_COMMAND,
    NVMCTRL_ERROR_ILLEGALSADDR as NVM_ERROR_WRITE_PROTECT, NVMCTRL_FBUSY as NVM_FBUSY,
};

/// The AVR32DD20's memories. Everything but [`UPDI_FLASH`] is a data-space
/// address, which UPDI reaches at the same number (DS40002413B §36.2.2).
pub mod map {
    use kr1k::regs::{self, addr, mem};

    /// Flash in UPDI's own address space, where "the code space always
    /// starts at the offset address 0x80_0000" (DS §36.2.2, figure 36-2).
    /// The ATDF only has the CPU's view, so this one is typed by hand. Every
    /// flash read and write the programmer makes goes here.
    pub const UPDI_FLASH: u32 = 0x80_0000;
    /// Flash seen through the CPU's data-space window, all 32 KB of it. The
    /// programmer doesn't use it; the simulator and `layout.rs` do.
    pub const FLASH: u16 = mem::MAPPED_PROGMEM_START as u16;
    pub const FLASH_SIZE: usize = mem::PROGMEM_SIZE;
    /// The erase granularity. Writes go by the word (DS §11.3.1.1).
    pub const FLASH_PAGE: usize = mem::PROGMEM_PAGE;

    /// EEPROM, which erases and writes a byte at a time (DS §11.3.1.2).
    pub const EEPROM: u16 = mem::EEPROM_START as u16;
    pub const EEPROM_SIZE: usize = mem::EEPROM_SIZE;

    /// Device ID, SIGROW.DEVICEID0 to DEVICEID2.
    pub const SIGNATURE: u16 = mem::SIGNATURES_START as u16;
    /// What the device ID reads on an AVR32DD20 (DS table 8-4), and the only
    /// chip this programmer writes to.
    pub const AVR32DD20: [u8; 3] = regs::SIGNATURE;

    /// The chip's serial number, SIGROW.SERNUM0 to SERNUM15 (DS §8.6.2).
    /// Read only, since UPDI can't write the signature row (DS table 8-5),
    /// and nothing here tries.
    pub const SERIAL: u16 = addr::SIGROW_SERNUM0 as u16;
    pub const SERIAL_SIZE: usize = addr::SIGROW_SERNUM15 - addr::SIGROW_SERNUM0 + 1;

    /// FUSE.WDTCFG to the last reserved fuse byte. Read for `backup`, and
    /// nothing here writes them.
    pub const FUSES: u16 = mem::FUSES_START as u16;
    pub const FUSES_SIZE: usize = mem::FUSES_SIZE;

    /// LOCK.KEY, 32 bits low byte first, which sits before the fuses on the
    /// DD. Read for `backup`, and nothing here writes it.
    pub const LOCK: u16 = mem::LOCKBITS_START as u16;
    pub const LOCK_SIZE: usize = mem::LOCKBITS_SIZE;
    /// What LOCK.KEY reads on an unlocked chip, in the order UPDI reads it.
    /// "Other" values lock it (DS §8.9.2.1). The programmer goes by
    /// ASI_SYS_STATUS.LOCKSTATUS; the fake chip keeps LOCK.KEY.
    #[cfg(test)]
    pub const UNLOCKED: [u8; LOCK_SIZE] = regs::LOCK_KEY_NOLOCK.to_le_bytes();

    /// The user row. Read for `backup`, and nothing here writes it.
    pub const USERROW: u16 = mem::USER_SIGNATURES_START as u16;
    pub const USERROW_SIZE: usize = mem::USER_SIGNATURES_SIZE;

    pub const NVMCTRL_CTRLA: u16 = addr::NVMCTRL_CTRLA as u16;
    pub const NVMCTRL_STATUS: u16 = addr::NVMCTRL_STATUS as u16;
}

/// One serial UPDI connection. `send` writes bytes and swallows their echo;
/// `recv` reads the chip's reply.
pub trait Wire {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<()>;
    /// Holds the line low for two long frames, which resets UPDI's own
    /// state machine (not the chip) so it listens for a fresh SYNC.
    fn double_break(&mut self) -> io::Result<()>;
    /// The time, which the programmer's timeouts count against. A real
    /// connection takes the clock's; the fake chip in the tests keeps its
    /// own, a frame per byte and a round trip per reply, so a timeout can't
    /// pass on a count of polls and the tests don't wait for real.
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub type Result<T> = std::result::Result<T, String>;

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

// Instruction modifiers.
const PTR_INC: u8 = 0x04;
const PTR_ADDRESS: u8 = 0x08;
/// Size A of three bytes, in bits 3:2 of LDS and STS.
const ADDRESS_24: u8 = 0x08;
/// The same size in bits 1:0 of an ST to the pointer, which is where that
/// instruction takes it (DS §36.3.3.4).
const PTR_SIZE_24: u8 = 0x02;
const DATA_8: u8 = 0x00;
const DATA_16: u8 = 0x01;
const KEY_SIB: u8 = 0x04;
const KEY_64: u8 = 0x00;
const SIB_16: u8 = 0x01;

// Control and status registers.
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

/// SIB bytes 8 to 10, NVM_VERSION (DS figure 36-20), on a chip with the NVM
/// controller this programmer drives. avrdude read "NVM type 2" from the
/// torch's SIB on 2026-11-16. A tinyAVR reads `P:0`.
const SIB_NVM_P2: &[u8; 3] = b"P:2";

/// The most a single REPEAT can cover, which is 256 bytes on a read and 256
/// words, one flash page, on a write.
const MAX_BLOCK: usize = 256;

/// How long the chip gets to come out of reset into programming mode.
const PROGMODE_TIMEOUT: Duration = Duration::from_millis(500);

/// The highest address UPDI's 24-bit addressing reaches.
const ADDRESS_END: u32 = 1 << 24;

/// A chip's serial number, SIGROW.SERNUM0 to SERNUM15 in that order.
///
/// The datasheet's words are "Each device has an individual serial number,
/// representing a unique ID. This number can be used to identify a specific
/// device in the field" (DS §8.6.2.3), and it "consists of the production
/// lot number, wafer number, and the device's wafer coordinates" (§11.3.1.3).
/// Nobody here has compared two chips'.
/// `backup` saves it beside the images, and `restore` checks it before it
/// erases anything, so a backup only goes back onto the chip it was read
/// from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialNumber([u8; map::SERIAL_SIZE]);

impl SerialNumber {
    /// From the bytes `backup` saved, which have to be all sixteen.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        match bytes.try_into() {
            Ok(bytes) => Ok(Self(bytes)),
            Err(_) => Err(format!(
                "a serial number is {} bytes, not {}",
                map::SERIAL_SIZE,
                bytes.len()
            )),
        }
    }

    pub const fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Whether every byte is 0xFF, or every byte is 0x00. Two chips that
    /// both read that way couldn't be told apart, so it can't say which chip
    /// a backup came from, and a check against it would pass for the wrong
    /// reason. Nothing says a real chip ever reads like this.
    pub fn is_blank(&self) -> bool {
        self.0 == [0xFF; map::SERIAL_SIZE] || self.0 == [0x00; map::SERIAL_SIZE]
    }

    /// Fails if this serial number, the one saved with the backup in `dir`,
    /// is blank. That needs no chip, so `restore` asks before it opens the
    /// port.
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

    /// Fails unless `chip`, just read from the chip on the wire, is this
    /// serial number, the one saved with the backup in `dir`. A blank one on
    /// either side fails too. Only [`Programmer::connect_for_restore`] asks,
    /// and it asks before anything's erased, which is what the errors say.
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

/// As `12 34 56`, the way the signature's shown.
impl fmt::Display for SerialNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

pub struct Programmer<W: Wire> {
    wire: W,
}

impl<W: Wire> Programmer<W> {
    /// Brings up the UPDI link, with a double break and a second try if the
    /// chip doesn't answer the first time.
    ///
    /// If the second try fails too, this gives up without sending UPDIDIS,
    /// though the chip may have heard enough to turn UPDI on. That's been
    /// looked at and left. By then the line has failed to answer twice, and
    /// it gets no more bytes sent down it. For one that's held low,
    /// docs/flashing.md says to stop and measure. What's left behind is UPDI
    /// and its clock request staying on, which costs power (DS §36.3.2.2.2),
    /// and only while the chip has any. The cell's out, so the adapter is its
    /// only supply, and that goes as the probe lifts.
    pub fn connect(wire: W) -> Result<Self> {
        let mut p = Self { wire };
        if p.init_link().is_err() {
            p.wire.double_break().map_err(io_error)?;
            p.init_link()?;
        }
        Ok(p)
    }

    /// Brings up the link, refuses a chip whose SIB doesn't say NVM P:2,
    /// enters programming mode and refuses anything but an AVR32DD20.
    ///
    /// The SIB needs no key (DS §36.3.7), so a tinyAVR such as the TS26's
    /// ATtiny1616 is turned away before the key or a reset goes out, with
    /// UPDIDIS and nothing else. If entering programming mode or the
    /// signature check fails, the NVM key and a reset may already have gone
    /// out, so this leaves the way [`Self::leave`] does before it reports
    /// what failed. Returning early would leave the chip with UPDI on, and
    /// perhaps held in programming mode, with nothing here to let it go.
    ///
    /// Leaving is a reset request and UPDIDIS, both writes to UPDI's own
    /// registers, which is all a locked chip accepts (DS §36.3.8.1). Neither
    /// erases anything, since that takes the chip erase key, which this never
    /// sends.
    pub fn connect_for_programming(wire: W) -> Result<Self> {
        let mut p = Self::connect(wire)?;
        if let Err(error) = p.check_sib() {
            // Whatever turning UPDI off says, the first error is the one to
            // report.
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

    /// As [`Self::connect_for_programming`], for `restore`. It then reads
    /// the chip's serial number and refuses any chip but the one the backup
    /// in `dir` was read from, whose serial number is `saved`. That's after
    /// the signature check and before anything's erased, so there's no
    /// programmer to erase with until the two match. A chip that's refused is
    /// left the way [`Self::leave`] does it, with a reset and then UPDI off.
    ///
    /// A blank `saved` is refused before a byte goes out. All this adds on
    /// the wire is one read of the signature row.
    pub fn connect_for_restore(wire: W, saved: &SerialNumber, dir: &Path) -> Result<Self> {
        saved.check_saved(dir)?;
        let mut p = Self::connect_for_programming(wire)?;
        match p
            .serial_number()
            .and_then(|chip| saved.check_chip(&chip, dir))
        {
            Ok(()) => Ok(p),
            Err(error) => {
                // Whatever leaving says, the first error is the one to report.
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

    /// The System Information Block, 16 bytes of ASCII naming the chip
    /// family, NVM and debug versions. Readable without any key, locked or
    /// not (DS §36.3.7).
    pub fn sib(&mut self) -> Result<[u8; 16]> {
        self.send(&[SYNC, KEY | KEY_SIB | SIB_16])?;
        let mut sib = [0; 16];
        self.recv(&mut sib)?;
        Ok(sib)
    }

    /// Fails unless the SIB's NVM_VERSION, bytes 8 to 10 (DS figure 36-20),
    /// says `P:2`, the NVM controller every write here is written for. The
    /// signature is the real check, read after the key and reset as the
    /// TS26's programmer did it, though UPDI can read it without (§8.6); this one
    /// turns away other families before the key goes out.
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

    /// Sends the NVM programming key and resets the chip into programming
    /// mode, as DS40002413B §36.3.8.2 has it, steps 2 to 7. The firmware
    /// stops running from here until [`Self::leave`].
    pub fn enter_progmode(&mut self) -> Result<()> {
        // Already there only counts on the same terms as the wait below. The
        // NVMPROG key needs the lock bits cleared (DS table 36-4), so a real
        // chip shouldn't read both bits set, but nothing in the datasheet
        // says it can't.
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

        // The chip isn't in programming mode until it's through reset and
        // ASI_SYS_STATUS.PROGSTART is set (§36.3.8.2 steps 6 and 7), and
        // pymcuprog and avrdude both expect it to read as locked until then,
        // so this waits for PROGSTART set and LOCKSTATUS clear. That's timed
        // by the clock, not by a count of polls, because a poll's round trip
        // depends on the adapter's USB latency, and a fast adapter could run
        // out of polls inside the start-up time. FUSE.SYSCFG1's SUT sets
        // that, and 64 ms is the longest it goes (ATDF), whatever this chip's
        // fuse holds. avrdude allows 100 ms to unlock and 500 ms more to
        // reach programming mode; one 500 ms window covers both here.
        self.reset()?;
        let deadline = self.wire.now() + PROGMODE_TIMEOUT;
        loop {
            let status = self.ldcs(ASI_SYS_STATUS)?;
            if status & SYS_STATUS_LOCKSTATUS == 0 && status & SYS_STATUS_PROGSTART != 0 {
                return Ok(());
            }
            if self.wire.now() >= deadline {
                break;
            }
        }
        Err(
            "the chip didn't enter programming mode. If it's locked, unlocking means \
             erasing it, which this tool won't do"
                .into(),
        )
    }

    /// Fails unless the chip reports itself as an AVR32DD20.
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

    /// Reads the chip's serial number, SERNUM0 to SERNUM15. It's the same
    /// kind of read as [`Self::check_signature`]'s, further along the
    /// signature row, which UPDI can read on an unlocked chip (DS table 8-5).
    pub fn serial_number(&mut self) -> Result<SerialNumber> {
        let bytes = self.read(map::SERIAL, map::SERIAL_SIZE)?;
        SerialNumber::from_bytes(&bytes)
    }

    /// Reads `len` bytes from `addr` in UPDI's address space, which is data
    /// space below 0x1_0000 and flash from [`map::UPDI_FLASH`].
    pub fn read(&mut self, addr: impl Into<u32>, len: usize) -> Result<Vec<u8>> {
        let addr = addr.into();
        let mut out = vec![0; len];
        for (i, chunk) in out.chunks_mut(MAX_BLOCK).enumerate() {
            self.st_ptr(offset(addr, i * MAX_BLOCK)?)?;
            let mut frame = repeat(chunk.len());
            frame.extend([SYNC, LD | PTR_INC | DATA_8]);
            self.send(&frame)?;
            self.recv(chunk)?;
        }
        Ok(out)
    }

    /// Erases all of flash, and EEPROM unless the EESAVE fuse is set, with
    /// NVMCTRL's CHER, which "can only be started from the UPDI" (DS
    /// §11.3.2.3.8), and then clears the command, as pymcuprog's
    /// `chip_erase` does. This is on an unlocked chip in programming mode,
    /// which is the only kind this programmer gets that far with, so it's an
    /// erase of what the chip holds and never an unlock.
    pub fn chip_erase(&mut self) -> Result<()> {
        self.wait_nvm()?;
        self.nvm_command(NVM_CHIP_ERASE)?;
        let erased = self.wait_nvm();
        self.clear_command(erased)
    }

    /// Chip-erases, then writes `data` from the start of flash a page at a
    /// time, each page erased with FLPER and then written by the word with
    /// FLWR. Blank pages are skipped, since the chip erase left them blank.
    /// Erases EEPROM too unless EESAVE is set.
    ///
    /// The page erase straight after a chip erase repeats work, about 10 ms
    /// a page (DS table 38-8). It's there so a word only ever lands on a page
    /// this call erased itself, since "before a write is performed to an
    /// address, its content needs to be erased" (§11.3.2.3.1). The chip erase stays because that's what
    /// clears the pages the image leaves blank, and EEPROM.
    ///
    /// Follow with [`Self::verify`], which catches an erase that didn't
    /// take. Refuses an image bigger than flash before erasing anything.
    pub fn erase_and_write_flash(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > map::FLASH_SIZE {
            return Err(format!(
                "the image is {} bytes; the AVR32DD20 has {}",
                data.len(),
                map::FLASH_SIZE
            ));
        }
        self.chip_erase()?;
        for (i, chunk) in data.chunks(map::FLASH_PAGE).enumerate() {
            if chunk.iter().all(|&b| b == 0xFF) {
                continue;
            }
            let page = offset(map::UPDI_FLASH, i * map::FLASH_PAGE)?;
            self.erase_flash_page(page)?;
            self.write_flash_page(page, chunk)?;
        }
        Ok(())
    }

    /// Erases the 512-byte page at `page` with FLPER, which "will allow each
    /// write to the memory array to erase a page" (DS §11.3.2.3.2), so one
    /// byte stored anywhere in it erases the lot. pymcuprog's
    /// `erase_flash_page` stores 0xFF at the page's address.
    fn erase_flash_page(&mut self, page: u32) -> Result<()> {
        self.wait_nvm()?;
        self.nvm_command(NVM_FLASH_PAGE_ERASE)?;
        let erased = self.sts(page, 0xFF).and_then(|()| self.wait_nvm());
        self.clear_command(erased)
    }

    /// Writes `data`, at most a page, by the word from `page` with FLWR (DS
    /// §11.3.2.3.1), as pymcuprog's `write_nvm` does with word access. An odd
    /// length gets a 0xFF to make the last word, which leaves that erased
    /// byte as it was.
    ///
    /// The words go out in one burst with the ACKs turned off, which is safe
    /// only because the controller keeps up. A word write takes 75 µs at
    /// most (tP_WRD, DS table 38-8), and a word is two 12-bit frames, 208 µs
    /// at 115,200 baud, so each is done before the next arrives. In FLWR mode
    /// "the ST* instructions write one byte at a time" (§11.3.2.3.1), so if a
    /// UPDI word store programs as two bytes it's 150 µs, still inside the
    /// 208 µs. That margin goes at about 160 kbaud, so don't raise the baud
    /// rate without moving to acknowledged stores. A store
    /// that came early would meet a bus wait, and with RSD set "a loss of
    /// data may occur" (§36.5.3). [`Self::verify`] would catch that.
    fn write_flash_page(&mut self, page: u32, data: &[u8]) -> Result<()> {
        assert!(data.len() <= map::FLASH_PAGE, "a page at most");
        let mut words = data.to_vec();
        if words.len() % 2 == 1 {
            words.push(0xFF);
        }

        self.wait_nvm()?;
        self.nvm_command(NVM_FLASH_WRITE)?;
        let written = self.st_ptr(page).and_then(|()| {
            let mut frame = vec![SYNC, STCS | CS_CTRLA, CTRLA_IBDLY | CTRLA_RSD];
            frame.extend(repeat(words.len() / 2));
            frame.extend([SYNC, ST | PTR_INC | DATA_16]);
            frame.extend(&words);
            frame.extend([SYNC, STCS | CS_CTRLA, CTRLA_IBDLY]);
            self.send(&frame)?;
            self.wait_nvm()
        });
        self.clear_command(written)
    }

    /// Writes `data` over EEPROM from its start, every byte of it, blank ones
    /// included, since EESAVE can keep the old contents through a chip
    /// erase. EEPROM erases and writes by the byte, with EEERWR, which
    /// enables "the erase operation directly followed by a write operation"
    /// (DS §11.3.2.3.5), as
    /// pymcuprog's `write_eeprom` uses.
    ///
    /// Each byte goes as its own STS, with its ACK, and then a wait for
    /// EEBUSY to clear, since an erase-write takes about 10 ms (tD_BEW, DS
    /// table 38-8) and only the bus wait would hold back the next one.
    /// Bytes past `data` keep what they held.
    ///
    /// Fuse programming "is identical to the EEPROM programming" (§11.3.2.2),
    /// so this only ever stores inside EEPROM, and refuses more bytes than
    /// EEPROM has before it sends any.
    pub fn write_eeprom(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > map::EEPROM_SIZE {
            return Err(format!(
                "the EEPROM image is bigger than {} bytes",
                map::EEPROM_SIZE
            ));
        }
        self.wait_nvm()?;
        self.nvm_command(NVM_EEPROM_ERASE_WRITE)?;
        let written = data.iter().enumerate().try_for_each(|(i, &byte)| {
            self.sts(offset(map::EEPROM.into(), i)?, byte)?;
            self.wait_nvm()
        });
        self.clear_command(written)
    }

    /// Reads `data.len()` bytes from `addr` and compares them with `data`.
    /// Run it after [`Self::erase_and_write_flash`] and
    /// [`Self::write_eeprom`].
    pub fn verify(&mut self, addr: impl Into<u32>, data: &[u8]) -> Result<()> {
        let addr = addr.into();
        let actual = self.read(addr, data.len())?;
        match actual.iter().zip(data).position(|(a, b)| a != b) {
            None => Ok(()),
            Some(i) => Err(format!(
                "verify failed at 0x{:06X}: wrote {:02X}, read {:02X}",
                addr as usize + i,
                data[i],
                actual[i]
            )),
        }
    }

    /// Resets the chip out of programming mode and turns UPDI off, so the
    /// firmware starts (DS §36.3.8.2 steps 9 and 10).
    pub fn leave(mut self) -> Result<W> {
        self.reset()?;
        self.disconnect()
    }

    /// Turns UPDI off. The firmware sends no reset of its own here, but the
    /// datasheet says writing UPDIDIS issues a system reset (§36.3.2.2.2),
    /// so expect the chip to restart. Nobody's seen that on a real one yet.
    pub fn disconnect(mut self) -> Result<W> {
        self.stcs(CS_CTRLB, CTRLB_UPDIDIS | CTRLB_CCDETDIS)?;
        Ok(self.wire)
    }

    /// Polls until the NVM controller's idle, and fails on an error code in
    /// STATUS.ERROR, bits 6:4 (DS §11.5.3). pymcuprog's `nvmp2.py` masks
    /// 0x30, two of the three bits; this takes all three, the ATDF's mask.
    ///
    /// It gives up after a count of polls where [`Self::enter_progmode`] goes
    /// by the clock. That's been looked at and left. A count is only wrong
    /// when a fast adapter can get through it before the chip's done, and
    /// this one is an upper bound with room to spare. The longest operation
    /// is a page erase at 11.7 ms or a chip erase at 11.6 ms (DS table
    /// 38-8's maximums), and an EEPROM erase-write is 10.07 ms typical with
    /// no maximum given. 10,000 polls take a second even at 0.1 ms a round
    /// trip, and `serial.rs` can't be that quick. A poll is six 12-bit
    /// frames, about 0.6 ms at 115,200 baud before any USB latency, so it's
    /// six seconds or more.
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

    /// Clears the NVM command after an operation, whether `done` says it
    /// worked or not, since "a change from one command to another must
    /// always go through" NOCMD or NOOP (DS §11.5.1). pymcuprog clears it on
    /// failure too. The first error is the one to report.
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

/// `addr` as the three bytes UPDI takes, low byte first.
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

/// A REPEAT instruction, to be sent ahead of the one it repeats.
fn repeat(count: usize) -> Vec<u8> {
    assert!((1..=MAX_BLOCK).contains(&count), "REPEAT covers 1 to 256");
    vec![SYNC, REPEAT, (count - 1) as u8]
}

/// What a non-zero STATUS.ERROR means, in the datasheet's names (DS §11.5.3).
/// The ATDF's names differ, and it gives a fourth code the datasheet calls
/// reserved.
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

/// `[0x1E, 0x95]` as `1E 95`.
fn hex(bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    hex.join(" ")
}

fn io_error(error: io::Error) -> String {
    match error.kind() {
        io::ErrorKind::TimedOut => "no reply from the chip. Check the probe's contact, \
                                    the pad order and that the cell is out"
            .into(),
        _ => format!("serial: {error}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fake::Dd20;

    /// Where the tests say a backup is. Nothing's read from it.
    const BACKUP: &str = "backup/breakout";

    /// What `map` assumes about the ATDF's segments. Its constants come from
    /// the ATDF, but the UPDI instruction set and UPDI's own address space
    /// aren't in there, so those stay checked by review against the
    /// datasheet and pymcuprog.
    #[test]
    fn map_assumes_what_the_atdf_says() {
        use kr1k::regs::{addr, mem};

        // SERIAL and SERIAL_SIZE take SERNUM0 and SERNUM15 as the ends of
        // one run of bytes.
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
        // The lock bits and the fuses are two reads, since they don't touch.
        const { assert!(mem::LOCKBITS_END <= mem::FUSES_START) };
    }

    /// This drives a real chip, so the numbers it writes to are pinned to
    /// the datasheet's memory map (DS40002413B §8 and §36.2.2), and a
    /// generator that goes wrong can't move them.
    #[test]
    fn map_is_the_datasheets() {
        assert_eq!(map::UPDI_FLASH, 0x80_0000, "flash in UPDI space");
        assert_eq!(map::FLASH_SIZE, 32 * 1024, "flash size");
        assert_eq!(map::FLASH_PAGE, 512, "flash page");
        assert_eq!(map::EEPROM, 0x1400, "EEPROM");
        assert_eq!(map::EEPROM_SIZE, 256, "EEPROM size");
        assert_eq!(map::SIGNATURE, 0x1100, "signature");
        assert_eq!(map::AVR32DD20, [0x1E, 0x95, 0x3A], "AVR32DD20");
        // SIGROW is at 0x1100 (§8.2) and SERNUM0 is 0x10 into it (§8.6.1).
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

    /// The NVM commands and status fields, pinned to DS40002413B §11.5.1 and
    /// §11.5.3 and to pymcuprog's `nvmp2.py`, for the same reason. The fake
    /// chip decodes them with these same names, so a wrong alias, CHER in
    /// particular, would pass every other test.
    #[test]
    fn nvm_commands_are_the_datasheets() {
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
    fn enters_progmode_and_checks_the_signature() {
        let mut p = Programmer::connect(Dd20::new()).unwrap();
        p.enter_progmode().unwrap();
        p.check_signature().unwrap();
    }

    /// More polls than the old fixed count of 100 allowed, which a fast
    /// adapter could get through inside a 64 ms start-up time.
    #[test]
    fn waits_for_the_chip_to_start_up_on_a_fast_or_slow_adapter() {
        // SUT is 64 ms at most; 400 ms is near the end of the window. A fast
        // adapter fits over a thousand polls in that, more than any fixed
        // count this used to have, and a slow one 25.
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
            // A poll is two frames out and a round trip back. Before the
            // first come 20 frames and two round trips: a poll, the key, its
            // status and the reset.
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
        // An AVR32DD32 (DS table 8-4) has the same SIB, and the TS26's
        // ATtiny1616 shouldn't get this far, but its signature is refused too.
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
    fn refuses_a_locked_chip() {
        let mut dd = Dd20::new();
        dd.lock = fake::LOCKED;
        let mut p = Programmer::connect(dd).unwrap();
        assert!(p.enter_progmode().unwrap_err().contains("locked"));
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

    /// Gives a chip something in flash and EEPROM to lose, shaped like what
    /// avrdude read off the torch on 2026-11-16 (flash used up to 0x3B7D,
    /// the first 58 bytes of EEPROM used), without any of its bytes.
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

    /// Checks a chip that was refused was let go the way `leave` does it,
    /// which is reset a second time, then UPDI off, with the chip out of
    /// programming mode and the flash and EEPROM [`with_contents`] gave it as
    /// they were.
    fn assert_let_go(dd: &Dd20) {
        assert_eq!(dd.resets, 2, "one reset to enter and one to leave");
        assert!(!dd.in_reset, "the chip was left in reset");
        assert!(!dd.progmode, "the chip was left in programming mode");
        assert!(dd.updi_disabled, "UPDI was left on");
        assert_contents_kept(dd);
    }

    /// Connects for programming to a chip that should be refused, and checks
    /// it was let go. Returns the error.
    fn refused(dd: Dd20) -> String {
        let mut dd = with_contents(dd);
        let Err(error) = Programmer::connect_for_programming(&mut dd) else {
            panic!("connected for programming");
        };
        assert_let_go(&dd);
        error
    }

    /// The same for a restore, of a backup that was saved with `saved`.
    fn refused_a_restore(dd: Dd20, saved: &SerialNumber) -> String {
        let mut dd = with_contents(dd);
        let Err(error) = Programmer::connect_for_restore(&mut dd, saved, Path::new(BACKUP)) else {
            panic!("connected for a restore");
        };
        assert_let_go(&dd);
        error
    }

    #[test]
    fn a_wrong_signature_ends_with_updi_off() {
        let mut dd = Dd20::new();
        dd.signature = [0x1E, 0x95, 0x38];
        let err = refused(dd);
        assert!(err.contains("1E 95 38"), "{err}");
    }

    /// The TS26's chip, or any tinyAVR, is turned away on its SIB before the
    /// key goes out, so it's never reset into programming mode. Only UPDIDIS
    /// follows, and nothing's sent to its data space at all.
    #[test]
    fn a_tinyavr_is_refused_on_its_sib_before_the_key() {
        let mut dd = with_contents(Dd20::new());
        // What another real ATtiny1616 reported.
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
        // Ten was the ATtiny1616's.
        for wrong in [0, 10, 15, 17] {
            let err = SerialNumber::from_bytes(&vec![0x12; wrong]).unwrap_err();
            assert!(err.contains(&format!("16 bytes, not {wrong}")), "{err}");
        }
    }

    #[test]
    fn a_serial_number_of_all_ff_or_all_00_is_blank() {
        for fill in [0xFF, 0x00] {
            assert!(SerialNumber([fill; map::SERIAL_SIZE]).is_blank());
            // One byte that isn't is enough to tell a chip by.
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
        // A serial number that differs in any one byte is another chip's.
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
            // The chip reads the same, so comparing the two would pass.
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
        let image: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut p = Programmer::connect(with_contents(Dd20::new())).unwrap();
        p.enter_progmode().unwrap();
        p.erase_and_write_flash(&image).unwrap();
        p.verify(map::UPDI_FLASH, &image).unwrap();
        let dd = p.leave().unwrap();

        assert_eq!(&dd.flash[..image.len()], &image[..]);
        assert!(dd.flash[image.len()..].iter().all(|&b| b == 0xFF));
        // 1000 bytes is two pages, each erased and then written by the word.
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

    /// Pages the image leaves blank are skipped, so the chip erase is what
    /// clears them. One that silently didn't happen leaves the old bytes
    /// there, and verify reads them back.
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

    /// And a page erase that didn't happen leaves words written over old
    /// bytes, which only clear bits, so verify catches that too.
    #[test]
    fn verify_catches_a_page_erase_that_did_not_take() {
        let image: Vec<u8> = (0..200u32).map(|i| (i * 7 + 3) as u8).collect();
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
        // A backup whose second half is blank, over EEPROM that EESAVE kept.
        let mut eeprom: Vec<u8> = (0..=255).collect();
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

    /// Fuse programming works the way EEPROM's does (DS §11.3.2.2), so
    /// `write_eeprom` refuses anything that would run past EEPROM before it
    /// sends a byte.
    #[test]
    fn refuses_an_eeprom_image_bigger_than_eeprom_before_writing() {
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        let err = p.write_eeprom(&[0x12; 257]).unwrap_err();
        assert_eq!(err, "the EEPROM image is bigger than 256 bytes");
        assert_eq!(p.wire.nvm_operations(), 0);
    }

    #[test]
    fn writes_eeprom_and_reads_fuses_lock_bits_and_user_row() {
        let eeprom: Vec<u8> = (0..=255).collect();
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.erase_and_write_flash(&[0x12]).unwrap();
        p.write_eeprom(&eeprom).unwrap();
        p.verify(map::EEPROM, &eeprom).unwrap();
        let fuses = p.read(map::FUSES, map::FUSES_SIZE).unwrap();
        // FUSE.SYSCFG0, as the torch's read.
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

    /// UPDI's own registers, their bits and its instructions, pinned to the
    /// numbers in the datasheet's UPDI chapter and pymcuprog's
    /// `constants.py`. None of them are in the ATDF, and the fake chip
    /// decodes most with these same names, so a slip in one would pass every
    /// other test. Each has been read against DS40002413B: SYNCH in
    /// §36.3.1.3, ACK in figure 36-4, the opcodes and size fields in figure
    /// 36-8 and §36.3.3, the addresses in §36.4, the bits in §36.5.3 to
    /// §36.5.9, the reset signature in §36.5.6, and the key and its order,
    /// least significant byte first, in table 36-5.
    #[test]
    fn updi_registers_and_instructions_are_the_datasheets() {
        assert_eq!(SYNC, 0x55, "SYNCH");
        assert_eq!(ACK, 0x40, "ACK");

        assert_eq!(
            [LDS, LD, STS, ST, LDCS, REPEAT, STCS, KEY],
            [0x00, 0x20, 0x40, 0x60, 0x80, 0xA0, 0xC0, 0xE0],
            "opcodes"
        );
        // What goes on the wire: LDS and STS with a 3-byte address and a
        // byte of data, ST to the pointer with a 3-byte address, LD of bytes
        // and ST of words with the pointer incremented, and the 16-byte SIB.
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
        // pymcuprog's 0x30 mask would miss this one.
        assert!(nvm_error(0x40).contains("error 4 (a code the datasheet calls reserved)"));
    }

    /// Stands in for a line that fails the same way every time, with no chip
    /// behind it. `fails` makes the error, on `send` if `on_send` and on
    /// `recv` if not.
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

    /// The words docs/flashing.md tells the reader to look for.
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

    /// `serial.rs` says "nothing echoed back" when the adapter can't hear its
    /// own bytes, which on the pads can mean a short. That mustn't come out as
    /// "no reply", which means poor contact and invites pressing harder.
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

        // On its own, entering programming mode stops at the key.
        let mut p = Programmer::connect(&mut dd).unwrap();
        let err = p.enter_progmode().unwrap_err();
        assert_eq!(err, "the chip didn't accept the NVM programming key");
        assert_eq!(dd.resets, 0, "the chip was reset without the key");

        // Connecting for programming lets the chip go afterwards, so the
        // only reset is the one that leaves.
        let mut dd = with_contents(dd);
        let Err(err) = Programmer::connect_for_programming(&mut dd) else {
            panic!("connected for programming");
        };
        assert_eq!(err, "the chip didn't accept the NVM programming key");
        assert_eq!(dd.resets, 1, "only the reset that leaves");
        assert!(!dd.in_reset, "the chip was left in reset");
        assert!(!dd.progmode, "the chip was left in programming mode");
        assert!(dd.updi_disabled, "UPDI was left on");
        assert_contents_kept(&dd);
    }

    /// The programmer's rule is that a chip is ready when it reads unlocked
    /// and PROGSTART, both (DS §36.3.8.2 step 7). Either alone is refused,
    /// and so is neither, however long it's given.
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

        // Other bits in the register don't matter. 0x02 is BOOTDONE and 0x20
        // is SYSRST (§36.5.9).
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

    /// A chip that's somehow in programming mode and locked at once isn't
    /// taken as already in programming mode. It's sent the key, and stays
    /// locked, and is refused.
    #[test]
    fn already_in_programming_mode_but_locked_is_refused() {
        let mut p = Programmer::connect(Dd20::new()).unwrap();
        p.wire.progmode = true;
        p.wire.lock = fake::LOCKED;
        let err = p.enter_progmode().unwrap_err();
        assert!(err.contains("unlocking means erasing it"), "{err}");
        assert_eq!(p.wire.resets, 1, "never sent the key and reset");
    }

    /// An image over two flash pages, and the EEPROM to go with it.
    fn images() -> (Vec<u8>, Vec<u8>) {
        let flash = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let eeprom = (0..=255).collect();
        (flash, eeprom)
    }

    /// Erases, writes and verifies both memories on `dd`, and leaves.
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
        // The datasheet's typical times, then its maximums with erases a
        // slow chip might take, each on a fast adapter and a slow one. A word
        // write can't be slower than the wire without losing words in the
        // burst, which is the next test's.
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

    /// Why the burst is safe, and what would make it not. At 115,200 baud a
    /// word takes 208 µs to arrive, against a 75 µs write at most. On a wire
    /// ten times quicker, words arrive while the last is still being
    /// written, and the fake loses them, as RSD's warning says a real one may
    /// (DS §36.5.3).
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

    /// Flash is busy on FBUSY and EEPROM on EEBUSY, and each has to be waited
    /// for by itself.
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
        // Eight bytes, each busy for 30 ms, and a poll is five frames out
        // and a 1 ms round trip back, about 1.5 ms, so 20 polls a byte.
        assert!(dd.busy_polls >= 8 * 15, "{} busy polls", dd.busy_polls);
        assert!(
            dd.flash[..0x3B7E].iter().all(|&b| b == 0x42),
            "flash changed"
        );
    }

    /// What the tests above rest on: the fake loses a store that arrives
    /// while it's busy, so a programmer that doesn't wait can't pass them.
    /// The datasheet says a store during an operation meets a bus wait
    /// (§11.3.2.1, §11.3.2.3.5), and with RSD set "a loss of data may occur"
    /// (§36.5.3), so losing it is the strict reading rather than a fact.
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

        // A store sent without waiting goes nowhere.
        p.wire.clear_error();
        p.sts(map::UPDI_FLASH, 0x12).unwrap();
        assert_eq!(p.wire.lost_while_busy, 1);
        assert_eq!(p.wire.word_writes, 0);

        // After the wait the same page goes in.
        p.wait_nvm().unwrap();
        p.write_flash_page(map::UPDI_FLASH, &[0x12]).unwrap();
        assert_eq!(p.read(map::UPDI_FLASH, 2).unwrap(), [0x12, 0xFF]);
        assert_eq!(p.wire.lost_while_busy, 1);
    }

    /// A programming command written while flash or EEPROM is busy is a
    /// command collision (DS §11.5.1), and the programmer reports it.
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

    /// "A change from one command to another must always go through" NOCMD
    /// or NOOP (DS §11.5.1). The datasheet doesn't say what happens when it
    /// doesn't; the ATDF's caption for code 3 is "Selecting new write command
    /// while write command already seleted" (sic), so the fake reports that.
    #[test]
    fn a_change_of_command_without_nocmd_between_is_a_collision() {
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.nvm_command(NVM_FLASH_PAGE_ERASE).unwrap();
        p.nvm_command(NVM_FLASH_WRITE).unwrap();
        let err = p.wait_nvm().unwrap_err();
        assert!(err.contains("CMDCOLLISION"), "{err}");
        // The error stays until it's written to 0 (§11.5.3).
        p.nvm_command(NVM_NO_COMMAND).unwrap();
        assert!(p.wait_nvm().is_err());
    }

    /// A store to flash with no command selected writes nothing. The ATDF
    /// captions code 1 "Write command not selected".
    #[test]
    fn a_store_with_no_command_selected_is_refused() {
        let mut p = Programmer::connect_for_programming(Dd20::new()).unwrap();
        p.sts(map::UPDI_FLASH, 0x12).unwrap();
        let err = p.wait_nvm().unwrap_err();
        assert!(err.contains("INVALIDCMD"), "{err}");
        assert_eq!(p.read(map::UPDI_FLASH, 1).unwrap(), [0xFF]);
    }

    /// Writing only clears bits, and "before a write is performed to an
    /// address, its content needs to be erased" (DS §11.3.2.3.1). The fake
    /// ANDs and counts each byte that wasn't erased, and the tests above
    /// check the programmer leaves that at 0.
    #[test]
    fn the_fake_counts_words_written_over_flash_that_was_not_erased() {
        let mut dd = Dd20::new();
        dd.flash[..2].copy_from_slice(&[0xF0, 0x0F]);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        p.write_flash_page(map::UPDI_FLASH, &[0x3C, 0x3C]).unwrap();
        assert_eq!(p.read(map::UPDI_FLASH, 2).unwrap(), [0x30, 0x0C]);
        assert_eq!(p.wire.unerased_writes, 2);
    }

    /// A page erase clears the whole 512-byte page the store lands in, and
    /// no more (DS §11.3.1.1, §11.3.2.3.2).
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

    /// On a locked chip UPDI can read the device ID (DS §8.6), and other
    /// reads "may appear to be successful, but the data is not valid" (DS
    /// table 8-6). The fake reads them as 0.
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
        // The quickest adapter `wait_nvm`'s notes allow for, which still has
        // to give the chip a second.
        let mut dd = with_contents(Dd20::new());
        dd.chip_erase_time = Duration::MAX;
        dd.round_trip = Duration::from_micros(100);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        let before = p.wire.elapsed;
        let err = p.erase_and_write_flash(&[0x12; 200]).unwrap_err();
        assert_eq!(err, "timed out waiting for the NVM controller");
        let waited = p.wire.elapsed.checked_sub(before).unwrap();
        assert!(waited >= Duration::from_secs(1), "gave up after {waited:?}");

        // It stopped there, so no page went out, and it can still leave.
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
        // The first page is an erase and 256 words, so this is the second
        // page's erase.
        dd.write_error_at = Some(1 + map::FLASH_PAGE / 2);
        let mut p = Programmer::connect_for_programming(dd).unwrap();
        let err = p.erase_and_write_flash(&image).unwrap_err();
        assert!(err.contains("error 2 (WRITEPROTECT"), "{err}");

        // One page went in, the second's erase failed, and its words were
        // never sent.
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

    /// A pretend AVR32DD20 on the far end of the wire, enough to exercise
    /// the instruction framing, 24-bit addressing, the ACK rules and the NVM
    /// controller. It's written from the same datasheet as the programmer,
    /// so it catches slips, not misreadings. Only a real chip catches those.
    /// `main.rs`'s tests back it up and restore it too.
    ///
    /// It panics on anything the programmer must never do: the Chip Erase or
    /// USERROW-Write key, a store to the fuses, lock bits, user row or
    /// signature row, a store outside programming mode, and the 16-bit
    /// address forms, which would reach the wrong memory on this chip.
    pub(crate) mod fake {
        use super::super::*;
        use kr1k::regs::{
            LOCK_KEY_RWLOCK, NVMCTRL_CMD_NOOP as NVM_NOOP,
            NVMCTRL_ERROR_ONGOINGPROG as NVM_ERROR_RESERVED,
        };
        use std::collections::VecDeque;
        use std::ops::Range;

        /// LOCK.KEY set to the ATDF's RWLOCK, one of the values that lock
        /// the chip (DS §8.9.2.1).
        pub const LOCKED: [u8; map::LOCK_SIZE] = LOCK_KEY_RWLOCK.to_le_bytes();

        /// The other two keys in DS table 36-5, which the programmer never
        /// sends.
        const KEY_CHIP_ERASE: &[u8; 8] = b"NVMErase";
        const KEY_USERROW: &[u8; 8] = b"NVMUs&te";

        const fn span(start: u32, size: usize) -> Range<u32> {
            start..start + size as u32
        }
        /// The same for a data-space address.
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
        /// SIGROW, all of it, which UPDI can't write (DS table 8-5).
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
            /// Words left in an ST ptr++ of words, and the low byte of the
            /// one arriving.
            StIncWord(usize, Option<u8>),
        }

        pub struct Dd20 {
            pub flash: Vec<u8>,
            pub eeprom: Vec<u8>,
            /// SRAM, all 4 KB, which reads 0 until a test fills it. UPDI
            /// reads it while the firmware runs.
            pub sram: Vec<u8>,
            pub signature: [u8; 3],
            /// SIGROW.SERNUM0 to SERNUM15. The bytes are made up.
            pub serial: [u8; map::SERIAL_SIZE],
            /// FUSE.WDTCFG on, as avrdude read them off the torch on
            /// 2026-11-16.
            pub fuses: [u8; map::FUSES_SIZE],
            /// LOCK.KEY, low byte first. Anything but [`map::UNLOCKED`] is
            /// locked.
            pub lock: [u8; map::LOCK_SIZE],
            pub userrow: [u8; map::USERROW_SIZE],
            /// What the SIB reads.
            pub sib: [u8; 16],
            pub asleep: bool,
            pub breaks: usize,
            /// Bytes the programmer has sent, to anything.
            pub sent: usize,
            /// KEY instructions that arrived, of any key.
            pub keys: usize,
            /// LDS, LD, STS and ST instructions that reached data space.
            pub data_accesses: usize,
            /// Stores that reached data space, of anything, NVMCTRL's
            /// registers included. A read-only command leaves this at 0.
            pub stores_in: usize,
            pub updi_disabled: bool,
            pub in_reset: bool,
            /// Reset requests that were applied and then released.
            pub resets: usize,
            /// PROGSTART, with the CPU held.
            pub progmode: bool,
            /// How long after a reset the chip's still starting up, reading
            /// as locked and without PROGSTART.
            pub start_up: Duration,
            /// How long each reply takes to come back, which is the adapter's
            /// USB latency, near enough. The fake clock moves on by this much
            /// per `recv`.
            pub round_trip: Duration,
            /// How long a byte takes on the wire, a 12-bit frame. The fake
            /// clock moves on by this much per byte sent, which is what lets
            /// it tell a burst of words from a slow trickle.
            pub byte_time: Duration,
            /// The fake clock, as time since `clock_start`.
            pub elapsed: Duration,
            clock_start: Instant,
            /// When the start-up after the last reset ends.
            started_at: Duration,
            /// The EESAVE fuse, which makes EEPROM survive a chip erase.
            pub eesave: bool,
            /// Chip and page erases that silently do nothing.
            pub ignores_erases: bool,
            /// Chip erases carried out.
            pub chip_erases: usize,
            /// Flash page erases carried out.
            pub page_erases: usize,
            /// Flash word writes carried out.
            pub word_writes: usize,
            /// EEPROM erase-writes carried out.
            pub eeprom_writes: usize,
            /// Flash bytes written that weren't erased first.
            pub unerased_writes: usize,
            /// What STATUSA reads. The UPDI revision is in its high nibble,
            /// so nothing with UPDI in it reads 0 there.
            pub statusa: u8,
            /// A chip that doesn't take the NVM programming key, so
            /// ASI_KEY_STATUS never shows it. Nothing here says why a real
            /// one wouldn't.
            pub ignores_key: bool,
            /// What ASI_SYS_STATUS reads once the chip's been through a
            /// reset, in place of what its state says. Nothing says a real
            /// chip reads any of these. It's for showing the programmer
            /// each reading it should refuse, such as one that's unlocked
            /// and never has PROGSTART.
            pub sys_status_after_reset: Option<u8>,
            /// How long a word write keeps FBUSY up. The defaults are DS
            /// table 38-8's typical times. `Duration::MAX` is a controller
            /// that never finishes.
            pub word_time: Duration,
            /// The same for a page erase.
            pub page_erase_time: Duration,
            /// The same for a chip erase.
            pub chip_erase_time: Duration,
            /// How long an EEPROM erase-write keeps EEBUSY up.
            pub eeprom_time: Duration,
            /// Reads of NVMCTRL.STATUS that came back busy.
            pub busy_polls: usize,
            /// Stores to flash or EEPROM that arrived while the controller
            /// was busy. The datasheet says they meet a bus wait (§11.3.2.1),
            /// and with RSD set "a loss of data may occur" (§36.5.3). The
            /// ATDF has a fourth error code, "Starting a new programming
            /// operation before previous is completed", which the datasheet
            /// calls reserved. This one drops them, so the memory ends up
            /// wrong, and counts them here.
            pub lost_while_busy: usize,
            /// Programming commands refused as collisions, while busy or
            /// without NOCMD between.
            pub collisions: usize,
            /// Which store to flash or EEPROM, counting from 0 and whatever
            /// the command, fails with WRITEPROTECT. Nothing here models what
            /// makes a real controller report one.
            pub write_error_at: Option<usize>,
            /// Stores so far, the failed one too.
            stores: usize,
            /// NVMCTRL.CTRLA.
            command: u8,
            /// NVMCTRL.STATUS's ERROR, which stays until it's written to 0
            /// (DS §11.5.3) or the chip resets.
            error: u8,
            /// The busy flags NVMCTRL.STATUS has up, until `busy_until` on
            /// the fake clock.
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
                fuses[..9].copy_from_slice(&[0x00, 0x00, 0x00, 0xFF, 0xFF, 0xD0, 0x08, 0x00, 0x00]);
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
                    // NVM_VERSION is what avrdude read off the torch ("NVM
                    // type 2"). The rest is the layout of DS figure 36-20 as
                    // pymcuprog shows it for AVR Dx parts, and unconfirmed on
                    // this chip.
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
                    // A full-speed USB adapter's frame, about the best a
                    // round trip gets.
                    round_trip: Duration::from_millis(1),
                    // 12 bits at 115,200 baud, `serial.rs`'s rate.
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

            /// Erases and writes of any kind that were carried out.
            pub fn nvm_operations(&self) -> usize {
                self.chip_erases + self.page_erases + self.word_writes + self.eeprom_writes
            }

            /// Writes STATUS.ERROR to 0, as a test's shortcut.
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

            /// What a system reset does to the NVM controller: the command
            /// and error go back to 0 (DS §11.5.1, §11.5.3) and an operation
            /// under way is "aborted" (§11.3.3).
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
                            // Turning UPDI off resets the system, puts the
                            // CPU back in the run state whatever it was in,
                            // and resets any keys (DS §36.3.2.2.2).
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
                        // NVMPROG needs the lock bits cleared (table 36-4),
                        // and the key status clears as PROGSTART sets
                        // (§36.5.5).
                        self.progmode = self.key_status & KEY_STATUS_NVMPROG != 0 && !self.locked();
                        self.key_status = 0;
                    }
                    _ => {}
                }
            }

            /// Whether an erase or a write is still going on.
            fn is_busy(&self) -> bool {
                self.elapsed < self.busy_until
            }

            /// NVMCTRL.STATUS, counting the reads that find it busy.
            fn nvm_status(&mut self) -> u8 {
                if self.is_busy() {
                    self.busy_polls += 1;
                    self.busy | self.error
                } else {
                    self.error
                }
            }

            fn load(&mut self, addr: u32) -> u8 {
                // The data-space window onto flash reads flash on a real chip
                // too, but the programmer's meant to use UPDI's own address
                // for it, so a read there is a slip.
                assert!(
                    !data(map::FLASH, map::FLASH_SIZE).contains(&addr),
                    "flash read through the data-space window at 0x{addr:04X}, not at 0x800000"
                );
                // A locked chip shows UPDI its device ID (DS §8.6) and
                // nothing else that's valid (table 8-6).
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

            /// One store, a byte or a word, from STS or ST.
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

            /// Raises `flags` in NVMCTRL.STATUS for the next `time` of the
            /// fake clock.
            fn start(&mut self, flags: u8, time: Duration) {
                self.busy = flags;
                self.busy_until = self.elapsed.saturating_add(time);
            }

            /// A write to NVMCTRL.CTRLA (DS §11.5.1). CHER starts at once;
            /// the others only enable stores to start operations.
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
                        // Flash and EEPROM both, so both flags go up here.
                        // Which a real chip raises is unconfirmed.
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

            /// A store to flash or EEPROM, which does whatever the selected
            /// command says.
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
                        // A write only clears bits; only an erase sets them.
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
                // Not a code the programmer should ever see from the fake.
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
                    // Each byte arrives once its frame's on the wire.
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

        /// Lets a test keep hold of the chip while a programmer drives it, to
        /// see how a connection that failed left it.
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
