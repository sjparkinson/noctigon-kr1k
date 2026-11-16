//! AVR32DD20 register addresses and bitfield values, generated at build time.
//!
//! They come from Microchip's ATDF in `vendor/`, by `build.rs`, with
//! `generate` in `xtask/src/atdf.rs`. Nothing in here that the ATDF can give
//! is typed by hand. The generated file is `regs.rs` in the package's
//! `OUT_DIR`.
//!
//! Only plain constants live here, so the file compiles anywhere. It's a
//! module of the library, where `selftest` checks the fuse fields with it.
//! The firmware uses it from `src/avr/hw.rs` and `src/avr/rt.rs`, and xtask
//! from its simulator and UPDI programmer. A constant nothing uses costs
//! nothing.
//!
//! The names follow Microchip's C headers, so each is the ATDF's own, in these
//! shapes.
//!
//! - `addr::RTC_PITCTRLA`, an instance and a register, is a data-space address.
//! - `mem::EEPROM_START`, `_SIZE`, `_END` and `_PAGE` describe a memory
//!   segment.
//! - `RTC_PITEN`, a module and a bitfield, is the field's mask.
//! - `RTC_PERIOD_CYC512`, a module, a bitfield and a named value, is the value,
//!   already shifted into place, so it can be OR'd straight into the register.
//! - `VECTOR_RTC_PIT`, an instance and an interrupt, and `VECTORS`, how many
//!   entries the vector table has.
//!
//! Masks and values are `u8` unless they don't fit. The ATDF's reset values
//! aren't here, since it disagrees with the datasheet on some of them.

include!(concat!(env!("OUT_DIR"), "/regs.rs"));

/// `ADC0.SAMPCTRL` for a sample of 34 ADC clocks.
///
/// That's 34 µs at the 1 MHz ADC clock `hw::init` sets. The ATDF has `SAMPLEN` as a plain 8-bit field with
/// no named values, so this is typed by hand. The sample lasts `2 + SAMPLEN`
/// ADC clocks (DS40002413B section 33.3.3.4), and the temperature sensor
/// wants at least 28 µs of it (section 33.3.3.8), so 32 gives 34 µs.
pub const ADC_SAMPLEN_32CLK: u8 = 32;

// SAMPLEN is the whole register, so any byte is a length.
const _: () = assert!(ADC_SAMPLEN == u8::MAX);
