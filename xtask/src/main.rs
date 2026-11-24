//! `cargo xtask <command>` is the AVR build and the steps around it that cargo
//! can't do.

#![cfg_attr(
    test,
    expect(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "a test that overflows or truncates fails, which is what a test is for"
    )
)]

#[cfg(test)]
mod atdf;
mod avr;
mod crc;
mod elf;
mod ihex;
mod int;
mod layout;
#[cfg(test)]
mod mistakes;
mod serial;
mod sim;
mod simulate;
mod stamp;
mod switching;
mod terminal;
mod twi;
mod updi;
mod world;

use std::env;
use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serial::Port;
use updi::{Programmer, SerialNumber, Wire, map};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  hex                     build the firmware, run it on the simulator, write
                          target/noctigon-kr1k.hex and print flash, RAM,
                          stack, loop and handler use
  ping <port>           check the programmer can see the chip, writing nothing,
                          though expect it to reset as UPDI turns off
  backup <port> <dir>     save the chip's flash, EEPROM, fuses, lock bits, user
                          row and serial number into <dir>
  flash <port> [file.hex] build and flash the firmware, or flash file.hex
  restore <port> <dir>    put <dir>/flash.hex and <dir>/eeprom.hex back, on the
                          chip whose serial number is in <dir>/serial.hex and
                          no other
  simulate [script...]    try src/ui.rs live in the terminal, or on a script
                          such as `1C 2H:1.5s wait:1s 3C`

<port> is the serial UPDI adapter, e.g. /dev/cu.usbserial-1410 on macOS
or /dev/ttyUSB0 on Linux. <dir> names one chip's backup, e.g. backup/torch.
";

const ELF: &str = "target/avr-none/release/kr1k.elf";
const PROBE: &str = "target/avr-none/release/examples/link.elf";
const GUARDS: &str = "target/avr-none/release/examples/guards.elf";
const HEX: &str = "target/noctigon-kr1k.hex";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["hex"] => hex(),
        ["ping", port] => ping(port),
        ["backup", port, dir] => backup(port, Path::new(dir)),
        ["flash", port] => build()
            .and_then(|(elf, _)| crc::sign(&elf.flash_image(), &stamp::current(&root())))
            .and_then(|image| flash(port, &image)),
        ["flash", port, file] => this_boards_hex(Path::new(file))
            .and_then(|()| read_hex(Path::new(file)))
            .and_then(|image| signed(&image).map(|()| image))
            .and_then(|image| flash(port, &image)),
        ["restore", port, dir] => restore(port, Path::new(dir)),
        ["simulate"] => simulate::interactive(),
        ["simulate", script @ ..] => simulate::script(script),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_default(Path::to_path_buf)
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn build() -> Result<(elf::Elf, sim::Report), String> {
    run(&mut firmware_build(&root()))?;
    checked(&root())
}

fn firmware_build(root: &Path) -> Command {
    let mut build = Command::new(cargo());
    build.current_dir(root).args([
        "build",
        "--package",
        "kr1k",
        "--bins",
        "--example",
        "link",
        "--example",
        "guards",
        "--release",
        "--target",
        "avr-none",
        "-Zbuild-std=core",
    ]);
    build
}

fn checked(root: &Path) -> Result<(elf::Elf, sim::Report), String> {
    let path = root.join(ELF);
    let elf = load(&path)?;
    layout::check(&elf).map_err(at(&path))?;
    let report = sim::check(&elf).map_err(|e| format!("{}: simulator: {e}", path.display()))?;

    let probe = root.join(PROBE);
    let probe_elf = load(&probe)?;
    probe_sections(&probe_elf).map_err(at(&probe))?;
    layout::memory(&probe_elf).map_err(at(&probe))?;
    sim::probe(&probe_elf).map_err(|e| format!("{}: simulator: {e}", probe.display()))?;

    guards_checked(root)?;
    Ok((elf, report))
}

fn guards_checked(root: &Path) -> Result<(), String> {
    let guards = root.join(GUARDS);
    let guards_elf = load(&guards)?;
    layout::check(&guards_elf).map_err(at(&guards))?;
    sim::guards(&guards_elf).map_err(|e| format!("{}: simulator: {e}", guards.display()))
}

fn load(path: &Path) -> Result<elf::Elf, String> {
    let bytes = fs::read(path).map_err(at(path))?;
    elf::Elf::parse(&bytes).map_err(at(path))
}

/// `layout::memory` passes an empty .data without looking, so the probe has
/// to have something in each section.
fn probe_sections(elf: &elf::Elf) -> Result<(), String> {
    for name in [".rodata", ".data", ".bss"] {
        match elf.section(name) {
            Some(range) if !range.is_empty() => {}
            _ => return Err(format!("the link probe has no {name}")),
        }
    }
    Ok(())
}

fn hex() -> Result<(), String> {
    const FLASH_SIZE: u64 = int::u64_from_usize(map::FLASH_SIZE);
    let (elf, sim) = build()?;
    let firmware = elf.flash_image();
    let image = crc::sign(&firmware, &stamp::current(&root()))?;
    write(&root().join(HEX), &ihex::encode(&image))?;
    eprintln!("wrote {HEX}");
    println!("stamp {}", stamp::read(&image).unwrap_or_default());

    let flash = int::u64_from_usize(firmware.len());
    let ram = elf.ram_bytes().into();
    let ram_size = (elf::RAM.end - elf::RAM.start).into();
    let (tick, handler) = (Some(sim::TICK_BUDGET), Some(sim::HANDLER_BUDGET));
    let per_tick = "instructions per 64 Hz tick";
    let per_handler = "instructions per handler, against a TCB0 period";
    for (label, used, of, unit) in [
        ("flash", flash, Some(FLASH_SIZE), "bytes"),
        ("ram", ram, Some(ram_size), "bytes"),
        ("stack", sim.stack.into(), None, "bytes"),
        ("loop", sim.loop_instructions, tick, per_tick),
        ("off", sim.off_instructions, tick, per_tick),
        ("isr", sim.handler_instructions, handler, per_handler),
    ] {
        println!("{}", row(label, used, of, unit));
    }
    Ok(())
}

#[expect(
    clippy::option_if_let_else,
    reason = "a match reads better than map_or_else with two closures"
)]
fn row(label: &str, used: u64, of: Option<u64>, unit: &str) -> String {
    let (of, percent) = match of {
        Some(of) => (
            format!("/ {:>6}", commas(of)),
            format!(
                "{:>3}%",
                used.saturating_mul(100).checked_div(of).unwrap_or(0)
            ),
        ),
        None => (String::new(), String::new()),
    };
    format!(
        "{label:<5} {:>6} {of:<8} {percent:>4}  {unit}",
        commas(used)
    )
}

fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && digits.len().abs_diff(i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn open(port: &str) -> Result<Port, String> {
    eprintln!("connecting on {port}");
    Port::open(port)
}

fn connect(port: &str) -> Result<Programmer<Port>, String> {
    Programmer::connect(open(port)?)
}

fn ping(port: &str) -> Result<(), String> {
    let mut p = connect(port)?;
    let sib = p.sib()?;
    p.disconnect()?;
    println!("found {}", String::from_utf8_lossy(&sib).trim_end());
    Ok(())
}

/// An image has no model in it, so the name is all there is to check, and a
/// renamed file gets through.
fn this_boards_hex(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .map_or_default(|name| name.to_string_lossy().to_lowercase());
    if name.contains("kr1k") {
        return Ok(());
    }
    Err(format!(
        "{} doesn't say it's for the KR1K, so it won't be flashed. Flash the file \
         `cargo xtask hex` writes, or use `restore <port> <dir>` for a backup",
        path.display()
    ))
}

/// Not for `restore`, since a factory backup has no CRC from this repo.
fn signed(image: &[u8]) -> Result<(), String> {
    updi::fits_flash(image)?;
    if image.len() < map::FLASH_SIZE {
        eprintln!(
            "warning: the image isn't the full {} bytes with a CRC, so it isn't a signed build \
             of the full firmware. That's right for the stock firmware, which checks no CRC. If \
             it's a build from main, it'll leave the light off on every boot, so \
             flash the file `cargo xtask hex` writes instead",
            map::FLASH_SIZE
        );
        return Ok(());
    }
    if crc::crc16(image.iter().copied()) != 0 {
        return Err(
            "the image's CRC doesn't check, so the firmware would leave the light off on \
             every boot. Flash the file `cargo xtask hex` writes"
                .to_string(),
        );
    }
    Ok(())
}

fn backup(port: &str, dir: &Path) -> Result<(), String> {
    backup_from(|| open(port), dir)
}

fn backup_from<W: Wire>(
    open: impl FnOnce() -> Result<W, String>,
    dir: &Path,
) -> Result<(), String> {
    let flash_path = dir.join("flash.hex");
    if flash_path.exists() {
        return Err(format!(
            "{} already exists. Move it somewhere safe first; if it's the factory \
             firmware, it's the only copy",
            flash_path.display()
        ));
    }

    let mut p = Programmer::connect_for_programming(open()?)?;
    let read = read_chip(&mut p);
    // Leave programming mode even after a failed read, and report the first error.
    let left = p.leave();
    let chip = read?;
    report_stamp("the chip's flash", &chip.flash);

    fs::create_dir_all(dir).map_err(at(dir))?;
    write(&flash_path, &ihex::encode(&chip.flash))?;
    write(&dir.join("eeprom.hex"), &ihex::encode(&chip.eeprom))?;
    write(&dir.join("fuses.hex"), &ihex::encode(&chip.fuses))?;
    write(&dir.join("lock.hex"), &ihex::encode(&chip.lock))?;
    write(&dir.join("userrow.hex"), &ihex::encode(&chip.userrow))?;
    write(&dir.join("serial.hex"), &ihex::encode(chip.serial.bytes()))?;
    eprintln!(
        "saved flash, EEPROM, fuses, lock bits and user row (those three for reference, never \
         written back) and the chip's serial number, {}, to {}",
        chip.serial,
        dir.display()
    );
    if chip.serial.is_blank() {
        eprintln!(
            "warning: that serial number can't tell one chip from another. The backup is \
             saved all the same, but `restore` will refuse it, since it couldn't check it \
             was writing to the chip this came from"
        );
    }
    // A backup that read whole is saved whatever leaving said.
    left?;
    Ok(())
}

struct Chip {
    flash: Vec<u8>,
    eeprom: Vec<u8>,
    fuses: Vec<u8>,
    lock: Vec<u8>,
    userrow: Vec<u8>,
    serial: SerialNumber,
}

fn read_chip<W: Wire>(p: &mut Programmer<W>) -> Result<Chip, String> {
    eprintln!("reading flash");
    let flash = p.read(map::UPDI_FLASH, map::FLASH_SIZE)?;
    eprintln!("reading EEPROM, fuses, lock bits, user row and the serial number");
    let eeprom = p.read(map::EEPROM, map::EEPROM_SIZE)?;
    let fuses = p.read(map::FUSES, map::FUSES_SIZE)?;
    let lock = p.read(map::LOCK, map::LOCK_SIZE)?;
    let userrow = p.read(map::USERROW, map::USERROW_SIZE)?;
    let serial = p.serial_number()?;
    Ok(Chip {
        flash,
        eeprom,
        fuses,
        lock,
        userrow,
        serial,
    })
}

fn report_stamp(what: &str, flash: &[u8]) {
    match stamp::read(flash) {
        Some(text) => eprintln!("{what} has the stamp `{text}`"),
        None => eprintln!(
            "{what} has no stamp, so it isn't a full build from this repo (it may be the \
             stock firmware or an empty chip)"
        ),
    }
}

fn flash(port: &str, image: &[u8]) -> Result<(), String> {
    report_stamp("the image", image);
    let connect = || Programmer::connect_for_programming(open(port)?);
    program(connect, image, None)
}

fn restore(port: &str, dir: &Path) -> Result<(), String> {
    restore_onto(|| open(port), dir)
}

fn restore_onto<W: Wire>(
    open: impl FnOnce() -> Result<W, String>,
    dir: &Path,
) -> Result<(), String> {
    let backup = read_backup(dir)?;
    let connect = || {
        let p = Programmer::connect_for_restore(open()?, &backup.serial, dir)?;
        eprintln!(
            "this chip's serial number, {}, is the one saved in {}",
            backup.serial,
            dir.display()
        );
        Ok(p)
    };
    program(connect, &backup.flash, Some(&backup.eeprom))
}

struct Backup {
    flash: Vec<u8>,
    eeprom: Vec<u8>,
    serial: SerialNumber,
}

/// Fuses aren't read, because nothing writes them back.
fn read_backup(dir: &Path) -> Result<Backup, String> {
    let serial_path = dir.join("serial.hex");
    if !serial_path.try_exists().map_err(at(&serial_path))? {
        return Err(format!(
            "{} doesn't exist, so nothing says which chip this backup was read from. \
             `restore` only writes a backup onto the chip it was read from, and the serial \
             number in that file is how it tells, so it won't write this one",
            serial_path.display()
        ));
    }
    let serial = SerialNumber::from_bytes(&read_hex(&serial_path)?).map_err(at(&serial_path))?;
    serial.check_saved(dir)?;
    Ok(Backup {
        flash: read_hex(&dir.join("flash.hex"))?,
        eeprom: read_hex(&dir.join("eeprom.hex"))?,
        serial,
    })
}

/// The chip erase wipes EEPROM too unless the EESAVE fuse is set, so pass
/// `Some` to put contents back.
fn program<W: Wire>(
    connect: impl FnOnce() -> Result<Programmer<W>, String>,
    flash: &[u8],
    eeprom: Option<&[u8]>,
) -> Result<(), String> {
    updi::fits_flash(flash)?;
    if let Some(eeprom) = eeprom {
        updi::fits_eeprom(eeprom)?;
    }

    let mut p = connect()?;
    let written = write_and_verify(&mut p, flash, eeprom);
    // Leave programming mode even after a failure, and report the first error.
    let left = p.leave();
    written?;
    left?;
    eprintln!("verified and restarted");
    Ok(())
}

fn write_and_verify<W: Wire>(
    p: &mut Programmer<W>,
    flash: &[u8],
    eeprom: Option<&[u8]>,
) -> Result<(), String> {
    eprintln!("erasing and writing {} bytes of flash", flash.len());
    p.erase_and_write_flash(flash)?;
    p.verify(map::UPDI_FLASH, flash)?;
    if let Some(eeprom) = eeprom {
        eprintln!("writing EEPROM");
        p.write_eeprom(eeprom)?;
        p.verify(map::EEPROM, eeprom)?;
    }
    Ok(())
}

fn read_hex(path: &Path) -> Result<Vec<u8>, String> {
    let text = fs::read_to_string(path).map_err(at(path))?;
    ihex::decode(&text).map_err(at(path))
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(at(path))
}

fn at<E: Display>(path: &Path) -> impl Fn(E) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

#[cfg(test)]
struct TempDir(PathBuf);

#[cfg(test)]
impl TempDir {
    fn new(name: &str) -> Result<Self, String> {
        let dir = env::temp_dir().join(format!("kr1k-{name}-{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(at(&dir))?;
        }
        fs::create_dir_all(&dir).map_err(at(&dir))?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TempDir {
    fn drop(&mut self) {
        _ = fs::remove_dir_all(&self.0);
    }
}

fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| format!("couldn't run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed ({status})"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::updi::tests::fake::Dd20;
    use super::updi::tests::images;
    use super::updi::{Programmer, SerialNumber};
    use super::{
        TempDir, backup_from, cargo, commas, crc, ihex, layout, load, program, read_hex,
        restore_onto, root, row, signed, stamp, this_boards_hex, write,
    };

    /// Built for the chip the way `firmware_build` builds the firmware.
    fn build_example(name: &str) -> (bool, String) {
        let output = Command::new(cargo())
            .current_dir(root())
            .args([
                "build",
                "--package",
                "kr1k",
                "--example",
                name,
                "--release",
                "--target",
                "avr-none",
                "-Zbuild-std=core",
            ])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        (output.status.success(), stderr)
    }

    #[test]
    fn a_build_that_indexes_out_of_bounds_is_one_the_panic_check_refuses() {
        let (built, stderr) = build_example("panics");
        assert!(built, "{stderr}");
        let elf = load(&root().join("target/avr-none/release/examples/panics.elf")).unwrap();
        let err = layout::no_panics(elf.symbols()).unwrap_err();
        assert!(err.contains("panic_bounds_check"), "{err}");
    }

    #[test]
    fn a_u16_divided_by_ten_fails_to_link_for_want_of_mulsi3() {
        let (built, stderr) = build_example("multiplies");
        assert!(!built, "a wide multiply links now");
        assert!(stderr.contains("undefined symbol: __mulsi3"), "{stderr}");
    }

    struct Scratch(TempDir);

    impl Scratch {
        fn new(test: &str) -> Self {
            Self(TempDir::new(&format!("xtask-{test}")).unwrap())
        }

        fn backup(&self) -> PathBuf {
            self.0.path().join("backup").join("torch")
        }
    }

    fn chip() -> Dd20 {
        let mut dd = Dd20::new();
        let (flash, eeprom) = images();
        dd.flash[..flash.len()].copy_from_slice(&flash);
        dd.eeprom.copy_from_slice(&eeprom);
        dd
    }

    fn on<'a>(dd: &'a mut Dd20) -> impl FnOnce() -> Result<&'a mut Dd20, String> {
        move || Ok(dd)
    }

    fn no_port() -> Result<Dd20, String> {
        panic!("the port was opened");
    }

    fn shown(serial: &[u8]) -> String {
        SerialNumber::from_bytes(serial).unwrap().to_string()
    }

    #[test]
    fn backup_saves_the_serial_number_and_restore_puts_the_backup_back() {
        let scratch = Scratch::new("restores");
        let dir = scratch.backup();
        let mut dd = chip();
        let (flash, eeprom) = (dd.flash.clone(), dd.eeprom.clone());

        backup_from(on(&mut dd), &dir).unwrap();
        assert_eq!(read_hex(&dir.join("flash.hex")).unwrap(), flash);
        assert_eq!(read_hex(&dir.join("eeprom.hex")).unwrap(), eeprom);
        assert_eq!(read_hex(&dir.join("serial.hex")).unwrap(), dd.serial);
        assert_eq!(read_hex(&dir.join("fuses.hex")).unwrap(), dd.fuses);
        assert_eq!(read_hex(&dir.join("lock.hex")).unwrap(), dd.lock);
        assert_eq!(read_hex(&dir.join("userrow.hex")).unwrap(), dd.userrow);
        assert_eq!(dd.stores_in, 0, "backup stored something");
        assert_eq!(dd.resets, 2, "one reset to enter and one to leave");

        dd.flash.fill(0x42);
        dd.eeprom.fill(0x24);
        restore_onto(on(&mut dd), &dir).unwrap();
        assert!(dd.flash == flash, "flash isn't the backup's");
        assert!(dd.eeprom == eeprom, "EEPROM isn't the backup's");
        dd.assert_let_go();
    }

    #[test]
    fn restore_refuses_a_backup_from_another_chip_and_writes_nothing() {
        let scratch = Scratch::new("another-chip");
        let dir = scratch.backup();
        let mut breakout = chip();
        backup_from(on(&mut breakout), &dir).unwrap();

        let mut torch = Dd20::new();
        torch.serial[4] ^= 0x80;
        torch.flash.fill(0x42);
        torch.eeprom.fill(0x24);
        let err = restore_onto(on(&mut torch), &dir).unwrap_err();

        assert!(err.contains(&shown(&torch.serial)), "{err}");
        assert!(err.contains(&shown(&breakout.serial)), "{err}");
        assert!(err.contains(&dir.display().to_string()), "{err}");
        assert!(torch.flash.iter().all(|&b| b == 0x42), "flash changed");
        assert!(torch.eeprom.iter().all(|&b| b == 0x24), "EEPROM changed");
        assert_eq!(torch.nvm_operations(), 0);
        torch.assert_let_go();
    }

    #[test]
    fn restore_refuses_a_backup_with_no_serial_number_before_it_opens_the_port() {
        let scratch = Scratch::new("no-serial");
        let dir = scratch.backup();
        backup_from(on(&mut chip()), &dir).unwrap();
        fs::remove_file(dir.join("serial.hex")).unwrap();

        let err = restore_onto(no_port, &dir).unwrap_err();
        assert!(err.contains("serial.hex doesn't exist"), "{err}");
        assert!(err.contains("which chip"), "{err}");
    }

    #[test]
    fn restore_refuses_a_serial_number_of_the_wrong_size_before_it_opens_the_port() {
        let scratch = Scratch::new("short-serial");
        let dir = scratch.backup();
        backup_from(on(&mut chip()), &dir).unwrap();
        write(&dir.join("serial.hex"), &ihex::encode(&[0x12; 9])).unwrap();

        let err = restore_onto(no_port, &dir).unwrap_err();
        assert!(err.contains("serial.hex"), "{err}");
        assert!(err.contains("16 bytes, not 9"), "{err}");
    }

    #[test]
    fn backup_saves_a_blank_serial_number_and_restore_refuses_it_before_it_opens_the_port() {
        for blank in [0xFF, 0x00] {
            let scratch = Scratch::new(&format!("blank-{blank:02x}"));
            let dir = scratch.backup();
            let mut dd = chip();
            dd.serial.fill(blank);

            backup_from(on(&mut dd), &dir).unwrap();
            assert_eq!(read_hex(&dir.join("flash.hex")).unwrap(), dd.flash);
            assert_eq!(read_hex(&dir.join("serial.hex")).unwrap(), dd.serial);

            let err = restore_onto(no_port, &dir).unwrap_err();
            assert!(err.contains(&format!("all {blank:02X}")), "{err}");
            assert!(err.contains(&dir.display().to_string()), "{err}");
        }
    }

    #[test]
    fn backup_refuses_to_overwrite_one_before_it_opens_the_port() {
        let scratch = Scratch::new("overwrite");
        let dir = scratch.backup();
        backup_from(on(&mut chip()), &dir).unwrap();

        let err = backup_from(no_port, &dir).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn flash_takes_only_a_signed_image() {
        let image = crc::sign(&[0x12, 0x34, 0x56], &stamp::TEST).unwrap();
        let round_trip = ihex::decode(&ihex::encode(&image)).unwrap();
        assert!(signed(&round_trip).is_ok());

        let mut broken = image;
        broken[1] ^= 1;
        assert!(signed(&broken).is_err());
    }

    #[test]
    fn a_stamp_survives_a_flash_and_a_backup() {
        let scratch = Scratch::new("stamp");
        let dir = scratch.backup();
        let image = crc::sign(&[0x12, 0x34, 0x56], &stamp::format("abc1234567", true)).unwrap();
        let mut dd = Dd20::new();

        let open = on(&mut dd);
        program(
            || Programmer::connect_for_programming(open()?),
            &image,
            None,
        )
        .unwrap();
        assert!(dd.flash == image, "flash isn't the image");

        backup_from(on(&mut dd), &dir).unwrap();
        let saved = read_hex(&dir.join("flash.hex")).unwrap();
        assert_eq!(
            stamp::read(&saved).as_deref(),
            Some("kr1k abc1234567+"),
            "the backup lost the stamp"
        );
    }

    #[test]
    fn flash_still_takes_a_short_image_such_as_the_stock_firmware() {
        assert!(signed(&[0x12, 0x34, 0x56]).is_ok());
    }

    #[test]
    fn flash_takes_this_boards_hex_and_refuses_another_torchs() {
        for good in [
            "target/noctigon-kr1k.hex",
            "release.noctigon-kr1k.hex",
            "NOCTIGON-KR1K.HEX",
            "stock-kr1k-0284.hex",
        ] {
            assert!(this_boards_hex(Path::new(good)).is_ok(), "{good}");
        }
        for bad in [
            "target/wurkkos-ts26-0721.hex",
            "backup/torch/flash.hex",
            "kr1k/firmware.hex",
            "anduril.hank-lume-x1-c-hwid0283-2026-10-22.hex",
            "lume-x1-c-0284.hex",
        ] {
            let err = this_boards_hex(Path::new(bad)).unwrap_err();
            assert!(err.contains("for the KR1K"), "{bad}: {err}");
            assert!(err.contains("`restore <port> <dir>`"), "{bad}: {err}");
        }
    }

    #[test]
    fn rows_line_up_with_and_without_a_total() {
        assert_eq!(
            row("flash", 3978, Some(32768), "bytes"),
            "flash  3,978 / 32,768  12%  bytes"
        );
        assert_eq!(
            row("stack", 107, None, "bytes"),
            "stack    107                bytes"
        );
    }

    #[test]
    fn commas_group_thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(107), "107");
        assert_eq!(commas(3978), "3,978");
        assert_eq!(commas(39062), "39,062");
        assert_eq!(commas(1_234_567), "1,234,567");
    }

    const FLASH_SIZE: usize = super::updi::map::FLASH_SIZE;
    const EEPROM_SIZE: usize = super::updi::map::EEPROM_SIZE;

    fn no_connection() -> Result<Programmer<Dd20>, String> {
        Programmer::connect_for_programming(no_port()?)
    }

    #[test]
    fn an_image_bigger_than_flash_is_refused_as_too_big_before_its_crc_is_checked() {
        let err = signed(&vec![0x12; FLASH_SIZE + 1]).unwrap_err();
        assert_eq!(err, "the image is 32769 bytes; the AVR32DD20 has 32768");
    }

    #[test]
    fn an_image_bigger_than_flash_is_refused_before_the_port_is_opened() {
        let err = program(no_connection, &vec![0x12; FLASH_SIZE + 1], None).unwrap_err();
        assert_eq!(err, "the image is 32769 bytes; the AVR32DD20 has 32768");
        let eeprom = vec![0x34; EEPROM_SIZE];
        let err = program(no_connection, &vec![0x12; 2 * FLASH_SIZE], Some(&eeprom)).unwrap_err();
        assert_eq!(err, "the image is 65536 bytes; the AVR32DD20 has 32768");
    }

    #[test]
    fn an_eeprom_image_bigger_than_eeprom_is_refused_before_the_port_is_opened() {
        let eeprom = vec![0x34; EEPROM_SIZE + 1];
        let err = program(no_connection, &[0x12], Some(&eeprom)).unwrap_err();
        assert_eq!(err, "the EEPROM image is bigger than 256 bytes");
    }

    #[test]
    fn images_that_fill_flash_and_eeprom_or_fall_short_of_them_are_written() {
        for (flash_len, eeprom_len) in [
            (FLASH_SIZE, EEPROM_SIZE),
            (FLASH_SIZE - 1, EEPROM_SIZE - 1),
            (1, 1),
            (0, 0),
        ] {
            let flash: Vec<u8> = (0..flash_len).map(|i| (i * 5 + 1) as u8 & 0x7F).collect();
            let eeprom: Vec<u8> = (0..eeprom_len).map(|i| (i * 3 + 2) as u8 & 0x7F).collect();
            let mut dd = Dd20::new();
            dd.flash.fill(0x42);

            let open = on(&mut dd);
            program(
                || Programmer::connect_for_programming(open()?),
                &flash,
                Some(&eeprom),
            )
            .unwrap_or_else(|e| panic!("{flash_len} and {eeprom_len} bytes: {e}"));
            assert!(dd.flash[..flash_len] == flash, "{flash_len} bytes");
            assert!(
                dd.flash[flash_len..].iter().all(|&b| b == 0xFF),
                "{flash_len} bytes: the rest of flash isn't erased"
            );
            assert!(dd.eeprom[..eeprom_len] == eeprom, "{eeprom_len} bytes");
            assert_eq!(dd.lost_while_busy, 0);
            assert!(dd.updi_disabled, "UPDI was left on");
        }
    }

    #[test]
    fn a_locked_chip_is_refused_and_keeps_what_it_holds() {
        let mut dd = chip();
        dd.lock = super::updi::tests::fake::LOCKED;
        let (flash, eeprom) = (dd.flash.clone(), dd.eeprom.clone());

        let open = on(&mut dd);
        let err = program(
            || Programmer::connect_for_programming(open()?),
            &[0x12; 200],
            Some(&[0x34; 40]),
        )
        .unwrap_err();
        assert!(err.contains("If it's locked"), "{err}");
        assert!(err.contains("which this tool won't do"), "{err}");
        assert!(dd.flash == flash, "flash changed");
        assert!(dd.eeprom == eeprom, "EEPROM changed");
        assert_eq!(dd.nvm_operations(), 0);
        assert_eq!(dd.resets, 2, "one reset to enter and one to leave");
        dd.assert_let_go();
    }

    #[test]
    fn a_write_error_stops_the_flash_and_lets_the_chip_go() {
        let mut dd = chip();
        dd.write_error_at = Some(1);

        let open = on(&mut dd);
        let err = program(
            || Programmer::connect_for_programming(open()?),
            &[0x12; 200],
            Some(&[0x34; 40]),
        )
        .unwrap_err();
        assert!(err.contains("WRITEPROTECT"), "{err}");
        assert_eq!(dd.page_erases, 1);
        assert_eq!(dd.word_writes, 99);
        assert_eq!(dd.eeprom_writes, 0);
        assert_eq!(dd.resets, 2, "one reset to enter and one to leave");
        dd.assert_let_go();
    }

    #[test]
    fn a_chip_that_stays_busy_is_given_up_on_and_let_go() {
        let mut dd = chip();
        dd.chip_erase_time = std::time::Duration::MAX;

        let open = on(&mut dd);
        let err = program(
            || Programmer::connect_for_programming(open()?),
            &[0x12; 200],
            None,
        )
        .unwrap_err();
        assert_eq!(err, "timed out waiting for the NVM controller");
        assert_eq!(dd.page_erases + dd.word_writes, 0);
        assert_eq!(dd.lost_while_busy, 0);
        assert_eq!(dd.resets, 2, "one reset to enter and one to leave");
        dd.assert_let_go();
    }
}
