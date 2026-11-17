//! A serial UPDI adapter on a Unix serial port, set up with `stty`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use crate::updi::Wire;

/// The default of pymcuprog and avrdude.
const BAUD: u32 = 115_200;

const BREAK_BAUD: u32 = 300;

/// Both timeouts are in tenths of a second, as `stty time` counts.
const READ_TIMEOUT: u8 = 10;

const DRAIN_TIMEOUT: u8 = 1;

const DRAIN_READS: usize = 64;

const NOTHING_ECHOED: &str = "nothing echoed back. A serial UPDI adapter joins TX and RX, so \
     every byte sent should come straight back. Either this adapter doesn't, or something's \
     pulling the UPDI line down: the wrong pad, a short, or a damaged chip. Stop and check \
     the wiring with a meter before trying again";

const LINE_HELD_LOW: &str = "every byte came back as 0, so something's pulling the UPDI line \
     down: the wrong pad, a short, or a damaged chip. Stop and check the wiring with a meter \
     before trying again";

pub struct Port {
    file: File,
}

fn check_echo(sent: &[u8], echo: &[u8]) -> io::Result<()> {
    if echo == sent {
        return Ok(());
    }
    if echo.iter().all(|&b| b == 0) {
        return Err(io::Error::other(LINE_HELD_LOW));
    }
    Err(io::Error::other(
        "the echo didn't match what was sent, so something else is on the line",
    ))
}

fn read_until_quiet(port: &mut impl Read) -> io::Result<()> {
    let mut junk = [0; 64];
    for _ in 0..DRAIN_READS {
        if port.read(&mut junk)? == 0 {
            return Ok(());
        }
    }
    Err(io::Error::other(
        "the line never went quiet, so something is driving it",
    ))
}

impl Port {
    pub fn open(path: &str) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("{path}: {e}"))?;
        let mut port = Self { file };
        port.drain().map_err(|e| format!("{path}: {e}"))?;
        port.wake().map_err(|e| format!("{path}: {e}"))?;
        Ok(port)
    }

    /// A woken UPDI holds the line low itself (DS40002413B §36.3.2.1.1), so the
    /// echo is mangled and goes unchecked, as in pymcuprog and avrdude. Nothing
    /// waits after it, since with no SYNCH in time it turns off (§36.3.2.2.1).
    fn wake(&mut self) -> io::Result<()> {
        self.file.write_all(&[0])?;
        let mut echo = [0];
        let _ = self.file.read(&mut echo)?;
        Ok(())
    }

    fn configure(&self, baud: u32, timeout: u8) -> io::Result<()> {
        let status = Command::new("stty")
            // `raw` goes first, since on macOS it resets what `cs8` and `parenb` set.
            .args(["raw", "-echo", "clocal", "-crtscts", "-ixon", "-ixoff"])
            .args(["cs8", "parenb", "-parodd", "cstopb"])
            .args(["min", "0", "time", &timeout.to_string()])
            .arg(baud.to_string())
            .stdin(Stdio::from(self.file.try_clone()?))
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("stty failed ({status})")))
        }
    }

    /// A stale byte would fail the next echo check (avrdude issue #874).
    fn drain(&mut self) -> io::Result<()> {
        self.configure(BAUD, DRAIN_TIMEOUT)?;
        read_until_quiet(&mut self.file)?;
        self.configure(BAUD, READ_TIMEOUT)
    }

    fn read_full(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.file.read(&mut buf[filled..])? {
                0 => return Err(io::ErrorKind::TimedOut.into()),
                n => filled += n,
            }
        }
        Ok(())
    }
}

impl Wire for Port {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        let mut echo = vec![0; bytes.len()];
        self.read_full(&mut echo).map_err(|e| match e.kind() {
            io::ErrorKind::TimedOut => io::Error::other(NOTHING_ECHOED),
            _ => e,
        })?;
        check_echo(bytes, &echo)
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.read_full(buf)
    }

    fn double_break(&mut self) -> io::Result<()> {
        self.configure(BREAK_BAUD, READ_TIMEOUT)?;
        self.file.write_all(&[0, 0])?;
        // The break mangles its own echo, which may be short or late.
        let mut echo = [0; 2];
        let _ = self.read_full(&mut echo);
        self.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_echo_passes() {
        assert!(check_echo(&[0x55, 0xC0], &[0x55, 0xC0]).is_ok());
        assert!(check_echo(&[0, 0], &[0, 0]).is_ok());
    }

    #[test]
    fn an_echo_of_zeros_means_the_line_is_held_low() {
        let err = check_echo(&[0x55, 0xC0], &[0, 0]).unwrap_err();
        assert!(
            err.to_string().contains("pulling the UPDI line down"),
            "{err}"
        );
    }

    #[test]
    fn any_other_echo_means_something_else_is_on_the_line() {
        let err = check_echo(&[0x55, 0xC0], &[0x55, 0x00]).unwrap_err();
        assert!(err.to_string().contains("something else"), "{err}");
    }

    #[test]
    fn draining_reads_what_is_waiting_and_stops_at_the_first_quiet_read() {
        let mut waiting: &[u8] = &[0xA5; 150];
        read_until_quiet(&mut waiting).unwrap();
        assert!(waiting.is_empty());
    }

    #[test]
    fn draining_gives_up_on_a_line_that_never_goes_quiet() {
        let mut noisy = io::repeat(0);
        let err = read_until_quiet(&mut noisy).unwrap_err();
        assert!(err.to_string().contains("never went quiet"), "{err}");
    }

    #[test]
    fn silence_keeps_its_old_words_and_names_a_pulled_down_line() {
        assert!(NOTHING_ECHOED.starts_with("nothing echoed back"));
        assert!(NOTHING_ECHOED.contains("adapter"));
        assert!(NOTHING_ECHOED.contains("pulling the UPDI line down"));
    }
}
