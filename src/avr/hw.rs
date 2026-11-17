//! The KR1K's Lume X1-C driver pins and the AVR32DD20 registers that drive
//! them.
//!
//! No PAC. The register constants are generated from Microchip's ATDF into
//! `src/regs.rs` at build time. The pin map is `kr1k::board`'s. Section
//! numbers are DS40002413B, the AVR32/16DD14/20 datasheet, and the errata is
//! DS80001026D.
//!
//! # Errata 2.2.3
//!
//! On every revision of this chip, a store to an address of 64 or more
//! followed straight away by a store to an address under 64, or to
//! `SLPCTRL.CTRLA`, loses the second store. The workaround is `out` for the
//! low addresses and a `nop` before `SLPCTRL.CTRLA`. So every write under 64
//! here (the VPORTs and the GPRs) goes through [`out!`], never [`write`], and
//! [`sleep_mode`] is the only write to `SLPCTRL.CTRLA`. That covers the
//! interrupt handlers too, since a handler's own epilogue only ever `out`s.

#![allow(unsafe_code)]

use core::arch::asm;
use core::ptr::{
    read_volatile, with_exposed_provenance, with_exposed_provenance_mut, write_volatile,
};

use kr1k::aw2016::{self, Update};
use kr1k::board::{
    BUTTON_LED, DAC, ENABLE, FET, HELD_LOW, PATHS, POWER_BANK, SWITCH, UNUSED_PC2, UNUSED_PC3,
    UNUSED_PD5,
};
use kr1k::ramp::{DAC_MAX, Output, Vref};
use kr1k::selftest::Fuses;
use kr1k::sensors::{Channel, Convert};
use kr1k::temp::Calibration;
use kr1k::watchdog::{CLOCK_HZ, Pit, Setting, TICK_DEADLINE_PERIODS, TIMER_PERIOD_CLOCKS, Wake};

use crate::regs::{self, addr as reg};

/// The PORTA pins the firmware drives, the three paths, the button LED and
/// PA1, which is only ever held low. PA0 (the FET gate, not fitted) and PA2
/// and PA3 (TWI to the AW2016) are never outputs.
const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW;
// The 0xF2 the stock build on this torch writes to VPORTA.DIR at reset.
const _: () = assert!(PORTA_OUTPUTS == 0xF2);
/// The PORTC pins the firmware drives, only PC1.
const PORTC_OUTPUTS: u8 = 1 << ENABLE;
/// The PORTD pins the firmware drives, only PD6, which DAC0's buffer takes
/// over while it's on.
const PORTD_OUTPUTS: u8 = 1 << DAC;

/// How far DAC0's 10-bit code sits up `DAC0.DATA`, which is left-aligned
/// (section 34.5.2).
const DAC_SHIFT: u32 = regs::DAC_DATA.trailing_zeros();
const _: () = assert!(DAC_MAX << DAC_SHIFT == regs::DAC_DATA);

/// TCB0's CCMP, one less than the period, since it counts 0 to CCMP.
const TIMER_TOP: u16 = TIMER_PERIOD_CLOCKS - 1;

/// TWI0's MBAUD for an SCL of 100 kHz at the 4 MHz main clock. Section
/// 29.3.2.2.1's equation 1 has SCL at `CLK_PER / (10 + 2 × BAUD + CLK_PER ×
/// tR)`, so 15 is 100 kHz with no rise time, and slower with one. Its
/// equation 3.2 puts SCL low at `(BAUD + 6) / CLK_PER − tOF`, 5.0 µs with the
/// longest fall time table 38-22 allows, over standard mode's 4.7 µs and the
/// AW2016's 1.3 µs. Standard mode is the slowest, so the bus has the most
/// margin for pull-ups nobody's measured. The main clock is forty times SCL,
/// over the four times that BUSERR needs (section 29.5.6).
const TWI_BAUD: u8 = 15;
const _: () = assert!(CLOCK_HZ == (10 + 2 * 15) * 100_000);

/// The most times [`set_aux`] reads TWI0.MSTATUS in one update, across
/// every wait.
///
/// The longest update, a colour's three transfers, is 14 bytes and three
/// STOPs, about 135 SCL periods, 5,400 main clocks. Each poll is about ten
/// instructions as compiled (`cargo xtask hex`'s loop figure with SCL held
/// low), and so at least ten clocks, so this is at least 9,000 clocks, over
/// one and a half times the longest update, and more on the chip, where
/// `lds` alone takes three. A bus that never answers then costs about 60 % of
/// the simulator's budget for a pass, which takes every instruction at four
/// clocks, and about a seventh of a real tick.
const TWI_POLLS: u16 = 900;

// The window `start_watchdog` sets is the one `kr1k::watchdog` counts on.
// WDT.CTRLA's fields count 8 << (n - 1) WDT clocks, and these are n = 3.
// The rest setting is n = 11, normal mode.
const _: () = {
    use kr1k::watchdog::{CLOSED_CLOCKS, OPEN_CLOCKS, REST_CLOCKS};
    assert!(8 << ((regs::WDT_WINDOW_32CLK >> 4) - 1) == CLOSED_CLOCKS);
    assert!(8 << (regs::WDT_PERIOD_32CLK - 1) == OPEN_CLOCKS);
    assert!(8 << (regs::WDT_PERIOD_8KCLK - 1) == REST_CLOCKS);
};

/// WDT.CTRLA for each of `kr1k::watchdog`'s settings.
const fn watchdog_setting(setting: Setting) -> u8 {
    match setting {
        Setting::Lit => regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK,
        Setting::Rest => regs::WDT_PERIOD_8KCLK,
    }
}

/// RTC.PITCTRLA for each of the PIT's periods, on.
const fn pit_setting(pit: Pit) -> u8 {
    match pit {
        Pit::Fast => regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN,
        Pit::Slow => regs::RTC_PERIOD_CYC32768 | regs::RTC_PITEN,
    }
}

/// Cycles of the 32 kHz clock in a PIT period, 2 << n for PERIOD n (section
/// 26.13.12's table).
const fn pit_cycles(value: u8) -> u32 {
    2 << (value >> regs::RTC_PERIOD.trailing_zeros())
}

// The slow period is 64 fast ones, `watchdog::SLOW_TICKS`.
const _: () = {
    let slow = kr1k::watchdog::SLOW_TICKS;
    assert!(pit_cycles(regs::RTC_PERIOD_CYC512) == 512);
    assert!(pit_cycles(regs::RTC_PERIOD_CYC32768) == 512 << slow.trailing_zeros());
    assert!(slow.is_power_of_two());
};

// Every peripheral (the ports, the timers, the clock controller) exposes
// its settings as bytes at fixed addresses, in the same address space as
// SRAM. Storing a byte at `VPORTC_OUT` doesn't put it in memory, it flips
// PORTC's pins. Loading from `VPORTD_IN` reads the voltage on PORTD's pins
// right now. These helpers are the only way the rest of the file does it.
//
// They're `volatile` because the compiler would otherwise treat these as
// ordinary memory, and could drop a write nobody reads back or read once
// and reuse the value in a loop. `read_volatile` and `write_volatile`
// promise each access happens, in order, exactly as written.

/// Writes a register at 64 or over. Anything lower goes through [`out!`]
/// (errata 2.2.3).
fn write(addr: usize, value: u8) {
    // SAFETY: every caller passes one of the `reg` constants, which are
    // memory-mapped I/O registers on this part.
    unsafe { write_volatile(with_exposed_provenance_mut::<u8>(addr), value) }
}

fn read(addr: usize) -> u8 {
    // SAFETY: as for `write`.
    unsafe { read_volatile(with_exposed_provenance::<u8>(addr)) }
}

/// Writes `$value` to the I/O register at `$addr` with `out`, which errata
/// 2.2.3 doesn't touch. `$addr` has to be a constant under 64, which the
/// build checks.
macro_rules! out {
    ($addr:expr, $value:expr) => {{
        const ADDR: usize = $addr;
        const _: () = assert!(ADDR < 0x40);
        let value: u8 = $value;
        // SAFETY: `out` writes one byte to an I/O register. It touches no
        // memory Rust knows about and no flags.
        unsafe {
            asm!(
                "out {addr}, {value}",
                addr = const ADDR,
                value = in(reg) value,
                options(nostack, preserves_flags),
            )
        }
    }};
}

/// 16-bit peripheral registers go through a shared TEMP byte. Write the low
/// byte first, then the high byte (section 8.10.3). Spelling it out keeps the
/// order from depending on the compiler. No interrupt handler here touches a
/// 16-bit register, so nothing can get between the two.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "addr is the low byte of a 16-bit register in data space, far under usize::MAX"
)]
fn write16(addr: usize, value: u16) {
    let [lo, hi] = value.to_le_bytes();
    write(addr, lo);
    write(addr + 1, hi);
}

/// Writes a register behind configuration change protection. The CPU only
/// accepts the write within four instructions of the CCP key, so it has to
/// be assembly (section 7.4.6).
///
/// Some registers, like the clock settings, could brick a running program
/// if a stray pointer hit them, so the chip guards them. Writing the magic
/// value 0xD8 (`CPU_CCP_IOREG`) to the CCP register unlocks them for the
/// next four instructions, and interrupts are held off for that window.
/// Rust gives no guarantee about how many instructions it puts between two
/// `write` calls, so the pair is written out by hand.
#[expect(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "usize is 16 bits on AVR, so addr fits Z whole, and try_from would need a \
              fallback address that doesn't exist"
)]
fn protected_write(addr: usize, value: u8) {
    // `out` only reaches the first 64 I/O addresses, which are also the
    // first 64 of data space.
    const _: () = assert!(reg::CPU_CCP < 0x40);
    // SAFETY: writes the IOREG key to CPU.CCP, then `value` to `addr`.
    // Touches no memory Rust knows about.
    unsafe {
        asm!(
            // Unlock. CCP sits at 0x34, low enough for `out` to reach.
            "out {ccp}, {key}",
            // Write the protected register, the very next instruction.
            // `st Z, r` stores register r at the address held in Z (the
            // r31:r30 pair), since the clock registers are past `out`'s reach.
            "st Z, {value}",
            ccp = const reg::CPU_CCP,
            key = in(reg) regs::CPU_CCP_IOREG,
            value = in(reg) value,
            in("Z") addr as u16,
            // The block doesn't touch the stack, and neither `out` nor `st`
            // changes the status flags in SREG.
            options(nostack, preserves_flags),
        );
    }
}

/// `PINnCTRL` for pin `pin` of the port whose `PIN0CTRL` is at `base`.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "base is a PIN0CTRL address and pin is under 8, far under usize::MAX"
)]
fn pin_ctrl(base: usize, pin: u8, value: u8) {
    write(base + usize::from(pin), value);
}

/// Whether PC1, the driver's enable, is high.
fn lit() -> bool {
    read(reg::VPORTC_OUT) & 1 << ENABLE != 0
}

/// Drives PC1 low, the driver off. `main` calls this before anything else,
/// since PC1 is a high-impedance input from reset until it's an output, and
/// the CRC scan in [`flash_ok`] stalls the CPU for about 12 ms before
/// [`init`] runs. [`init`] writes DIR again.
///
/// OUT goes first, then DIR. A reset leaves OUT at zero, and this doesn't
/// count on having had one. If the CPU ever got to the reset vector some
/// other way, by a stray jump, OUT could still have PC1 set from a light that
/// was on, and DIR alone would hold the driver on through the scan and
/// `init`. The brown-out detector is off on this torch, so a dip in the cell
/// doesn't reset the chip at all, which is docs/safety.md's to worry about.
///
/// PC1 isn't all such a start would find stale. DAC0, VREF and the paths keep
/// whatever they held until [`init`] sets them. With PC1 low the driver is
/// off whatever they say. TCB0 and its interrupt could still be running, so
/// the first [`wait_for_tick`] could count a period or two that belong to the
/// old light, which only clears GPR1 with PC1 low. The watchdog is still
/// running in window mode, and the datasheet doesn't say whether
/// [`start_watchdog`] writing the same setting again restarts its windows.
/// If it doesn't, the first pass's feed can land in the closed window and the
/// chip resets for real. Nobody's seen any of it happen.
///
/// PC1 is on the VDDIO2 domain (section 5.1, figure 5-1). If VDDIO2 were
/// missing it would be tri-stated whatever this writes (section 19.3.2.1). `main` checks for
/// that with [`vddio2_ok`].
pub fn hold_enable_low() {
    out!(reg::VPORTC_OUT, 0);
    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);
}

/// Whether MVIO says VDDIO2 is in range, so PORTC's pins, PC1 among them,
/// follow their PORT registers (section 19.5.3).
pub fn vddio2_ok() -> bool {
    read(reg::MVIO_STATUS) & regs::MVIO_VDDIO2S != 0
}

/// Sets the chip up for the firmware loop, [`init_pins`] then PD4's sense,
/// TCB0, the ADC and the PIT, which are the timers and interrupts the loop
/// runs on.
pub fn init() {
    init_pins();

    // PD4 is the switch, which pulls against an external 100k pull-up, so
    // the internal one stays off. Both edges raise PORTD's interrupt, which
    // wakes the chip from power-down, since every pin on this chip senses
    // asynchronously (section 18.3.3.1). The input buffer stays on, so
    // VPORTD.IN still reads the pin.
    pin_ctrl(reg::PORTD_PIN0CTRL, SWITCH, regs::PORT_ISC_BOTHEDGES);

    // TCB0 in periodic interrupt mode, its reset mode, off the main clock,
    // for the tick deadline. `set_output` starts and stops it with the light.
    // CCMP goes in with TCB0 stopped and interrupts still off from reset.
    write(reg::TCB0_CTRLB, regs::TCB_CNTMODE_INT);
    write16(reg::TCB0_CCMP, TIMER_TOP);

    // TWI0's baud rate, which only takes a write with the host off (section
    // 29.5.7), as it is from reset. [`set_aux`] turns the host on and off
    // round each update.
    write(reg::TWI0_MBAUD, TWI_BAUD);

    // ADC0, single 12-bit conversions, four accumulated. 4 MHz / 4 is a
    // 1 MHz ADC clock, inside the 125 kHz to 2 MHz of table 38-25. INITDLY
    // of 32 µs covers the 25 µs the temperature sensor wants after the ADC
    // starts (section 33.3.3.8), and SAMPLEN its 28 µs of sampling. Both
    // channels use the same timing, and `adc_channel` sets the reference and
    // the input for each. The ADC stays off until then.
    write(reg::ADC0_CTRLB, regs::ADC_SAMPNUM_ACC4);
    write(reg::ADC0_CTRLC, regs::ADC_PRESC_DIV4);
    write(reg::ADC0_CTRLD, regs::ADC_INITDLY_DLY32);
    write(reg::ADC0_SAMPCTRL, regs::ADC_SAMPLEN_32CLK);

    // The RTC's periodic interrupt timer, off the internal 32.768 kHz
    // oscillator (RTC.CLKSEL's reset value) divided by 512, which is 64 Hz,
    // with its interrupt on. RTC.CTRLA stays at its reset value.
    write(reg::RTC_PITINTCTRL, regs::RTC_PI);
    set_pit(Pit::Fast);
}

/// Gives the PIT `pit`'s period, 64 Hz or 1 Hz, waiting for any write still
/// syncing first (section 26.13.12) and for this one after. The sleep after
/// it can be power-down, and the datasheet doesn't say the write finishes
/// syncing with the peripheral clock stopped. If it didn't, there'd be no
/// ticks, so it's seen through.
///
/// The first period after a change of period could be anything up to a
/// whole one, by the prescaler's phase (section 26.5.2.2), and likely lands
/// on the prescaler's own edge. `kr1k::watchdog` counts on neither.
pub fn set_pit(pit: Pit) {
    while read(reg::RTC_PITSTATUS) & regs::RTC_CTRLBUSY != 0 {}
    write(reg::RTC_PITCTRLA, pit_setting(pit));
    while read(reg::RTC_PITSTATUS) & regs::RTC_CTRLBUSY != 0 {}
}

/// The clock, the pins and DAC0, with no timer and no interrupt. [`init`]
/// starts with this, and examples/first_light.rs runs it alone.
///
/// PD4, the switch, is left as reset has it, its input buffer on, no
/// pull-up and no sense, so it reads but raises nothing. [`init`] gives it
/// its sense.
pub fn init_pins() {
    // The internal high-frequency oscillator at 4 MHz, no prescaler, which is
    // what reset leaves (sections 12.5.7 and 12.5.2). Written anyway, so a
    // start that wasn't a reset still runs at the clock
    // `watchdog::CLOCK_HZ` assumes. With no PWM on the main LED, nothing
    // here needs more, and the main clock is specified to 24 MHz over the
    // whole supply range (table 38-15), so a lower one buys no voltage
    // margin, only current. AUTOTUNE stays off, since it tunes against a
    // 32.768 kHz crystal (section 12.3.6) this board most likely doesn't
    // have.
    protected_write(reg::CLKCTRL_OSCHFCTRLA, regs::CLKCTRL_FRQSEL_4M);
    protected_write(reg::CLKCTRL_MCLKCTRLB, 0);

    // The pins, as `kr1k::board` names them.
    //
    // PA0 (the FET gate, not fitted), PC2, PC3, PD5 and PD7 are unused, so
    // they get a pull-up and nothing floats, as the stock firmware has them.
    // PA2 and PA3 are the AW2016's TWI lines, and TWI0 takes them over while
    // its host is on. Their PORT settings are left as reset has them, inputs
    // with no pull-up, since the board likely has its own (the stock
    // firmware's lines for internal ones are commented out, and its TWI
    // works), and they're never outputs, since SCL held low for 130 ms shuts
    // the AW2016 down.
    //
    // PA1 goes to something unknown, and the stock build on this torch holds
    // it as an output at 0 (`kr1k::board` has why), so it's that here, with
    // no pull-up. Its PINCTRL is written to 0 in case anything left one.
    //
    // PC1 is the driver's enable, and never gets a pull-up, so it can't creep
    // the driver on before it's an output. Its PINCTRL is written to 0 in
    // case anything left one.
    //
    // PD6 is DAC0's output, and its input buffer is off, as section 34.3.1
    // asks.
    pin_ctrl(reg::PORTA_PIN0CTRL, FET, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, 0);
    pin_ctrl(reg::PORTC_PIN0CTRL, ENABLE, 0);
    pin_ctrl(reg::PORTC_PIN0CTRL, UNUSED_PC2, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTC_PIN0CTRL, UNUSED_PC3, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTD_PIN0CTRL, UNUSED_PD5, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTD_PIN0CTRL, DAC, regs::PORT_ISC_INPUT_DISABLE);
    pin_ctrl(reg::PORTD_PIN0CTRL, POWER_BANK, regs::PORT_PULLUPEN);

    // Everything the firmware drives comes up low, the paths off, the button
    // LED off (the stock firmware's off for it is an output at 0), PA1 low,
    // PC1 off, and PD6 at 0 under DAC0. Nothing ever writes PA1's OUT high,
    // since every later VPORTA.OUT write is 0 or a paths mask. PD6 is an output as the stock firmware has it,
    // so if DAC0's buffer lets go of the pin in power-down (section 34.3.3),
    // PORT holds it low. The datasheet doesn't say which drives the pad then,
    // and it's unconfirmed. OUT before DIR, as for PC1. PORTA's OUT has 0 for
    // PA0, PA2 and PA3 too, which are inputs and stay so.
    out!(reg::VPORTA_OUT, 0);
    out!(reg::VPORTA_DIR, PORTA_OUTPUTS);
    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);
    out!(reg::VPORTD_OUT, 0);
    out!(reg::VPORTD_DIR, PORTD_OUTPUTS);

    // DAC0 on, at 0, against 1.024 V, in section 34.3.1's order. It stays on
    // from here, at 0 whenever the light's off, and power-down turns it off
    // by itself.
    write(reg::VREF_DAC0REF, regs::VREF_REFSEL_1V024);
    write16(reg::DAC0_DATA, 0);
    write(reg::DAC0_CTRLA, regs::DAC_ENABLE | regs::DAC_OUTEN);
}

/// The bit in GPR0 that [`rtc_pit`] sets each tick. The GPRs sit in I/O
/// space, which keeps the flag out of RAM and makes it one cycle to reach.
const TICK: u8 = 1 << 0;

/// What [`tcb0_int`] writes to GPR1 at the end of each TCB0 period. Nothing
/// else lives in GPR1, so the handler writes it whole.
const PERIOD: u8 = 1 << 0;

/// What [`portd_port`] writes to GPR2 on an edge on the switch. Nothing else
/// lives in GPR2, so the handler writes it whole.
const SWITCHED: u8 = 1 << 0;

/// Writes `SLPCTRL.CTRLA` with a `nop` before it, errata 2.2.3's workaround.
fn sleep_mode(value: u8) {
    // SAFETY: a `nop`, then one store to SLPCTRL.CTRLA. Touches no memory
    // Rust knows about and no flags.
    unsafe {
        asm!(
            "nop",
            "sts {addr}, {value}",
            addr = const reg::SLPCTRL_CTRLA,
            value = in(reg) value,
            options(nostack, preserves_flags),
        )
    }
}

/// Sets up the sleep that [`wait_for_tick`] goes on to do. `rest` is
/// `watchdog::End::power_down`.
///
/// Power-down stops the main clock, and with it TCB0 and the tick deadline,
/// and DAC0 (sections 24.3.6 and 34.3.3), so it only happens with PC1 low. If
/// the torch ever says it's resting with the driver on, this idles instead.
/// The light stays as it was, and the loop keeps running, so the protection
/// does too. The firmware never asks for that, so only examples/guards.rs
/// runs this.
///
/// It also turns the ADC off, since a conversion caught by power-down comes
/// back invalid (section 33.3.8). Its reference is only on while something
/// asks for it (section 21.2). [`adc_channel`] turns it back on.
///
/// Otherwise it's idle, which stops only the CPU, so TCB0 keeps counting and
/// the ADC keeps converting.
///
/// SEN has to stay set in both, because with it clear, `sleep` does nothing.
fn prepare_sleep(rest: bool) {
    let mode = if rest && !lit() {
        write(reg::ADC0_CTRLA, 0);
        regs::SLPCTRL_SMODE_PDOWN
    } else {
        regs::SLPCTRL_SMODE_IDLE
    };
    sleep_mode(mode | regs::SLPCTRL_SEN);
}

/// Sleeps until the next tick, in power-down if `rest` allows it, and says
/// what ended the sleep. The PIT carries on in there (section 26.9), so the
/// loop runs every tick, on or off, 64 times a second, or once a second
/// with the PIT slowed for a rest (`kr1k::watchdog::Pacer`).
///
/// A press on PD4 wakes the chip too, through PORTD's interrupt, which sets
/// [`SWITCHED`]. With `switch_wakes`, which `main` only asks for with the PIT
/// slow, that ends the wait as [`Wake::Switch`], and the loop runs a pass on
/// the press rather than up to a second later. Otherwise it's cleared and
/// slept through, as any other wake that isn't a tick, so at 64 Hz the loop
/// looks at the switch once a tick and a press that comes and goes between
/// two ticks is still missed. Nobody's measured how short a press this switch
/// can give. A tick that's due as well wins, and its pass reads the switch
/// anyway, through [`take_switch`], which clears [`SWITCHED`] as it reads.
///
/// The PIT interrupt sets [`TICK`]; this checks and clears it with interrupts
/// off, so a tick can't land between the check and the clear. If it isn't set
/// yet, `sei` and `sleep` go back to back. The instruction after `sei` always
/// runs before a pending interrupt, so a tick that arrived after the check
/// wakes the `sleep` at once rather than being slept through.
///
/// While the light's on, TCB0 sets [`PERIOD`] once a millisecond, timed by
/// the main oscillator. The tick and the watchdog share the 32 kHz
/// oscillator, so if that stopped with the light on, nothing else would ever
/// turn it off. After `watchdog::TICK_DEADLINE_PERIODS` of those flags and no
/// tick, 103 to 104 ms, this gives up and returns `None`, and `main` calls
/// [`halt`]. It counts flags, not wakes, so a bouncing switch can't run the
/// deadline down. src/watchdog.rs has the sums. The PIT is never slow with
/// the light on.
///
/// With PC1 low there's no deadline, since there's no light to turn off.
/// TCB0 is stopped then. A dead oscillator leaves the chip asleep, in idle or
/// power-down, whichever `rest` chose, with the driver off, until the cell
/// comes out or, with `switch_wakes`, until a press runs a pass, which finds
/// a tick that never comes.
///
/// Interrupts are on when it returns, either way.
pub fn wait_for_tick(rest: bool, switch_wakes: bool) -> Option<Wake> {
    prepare_sleep(rest);
    let mut periods = TICK_DEADLINE_PERIODS;
    let woke = loop {
        // SAFETY: `cli` only clears SREG.I. With no `nomem`, it's also a
        // compiler barrier, so the GPR reads can't move above it.
        unsafe { asm!("cli", options(nostack)) };
        if read(reg::GPR_GPR0) & TICK != 0 {
            out!(reg::GPR_GPR0, 0);
            break Some(Wake::Tick);
        }
        if read(reg::GPR_GPR2) != 0 {
            out!(reg::GPR_GPR2, 0);
            if switch_wakes {
                break Some(Wake::Switch);
            }
        }
        if read(reg::GPR_GPR1) != 0 {
            out!(reg::GPR_GPR1, 0);
            if lit() {
                periods = periods.saturating_sub(1);
                if periods == 0 {
                    break None;
                }
            }
        }
        // SAFETY: sets SREG.I and sleeps until an interrupt, whose handler
        // returns here. The handler saves what it touches.
        unsafe { asm!("sei", "sleep", options(nostack)) };
    };
    // SAFETY: `sei` only sets SREG.I.
    unsafe { asm!("sei", options(nostack)) };
    woke
}

/// Turns the light off and stops for good. `main` calls this when
/// [`wait_for_tick`]'s deadline runs out, and it's the panic handler too.
///
/// An interrupt with no handler ends up here as well. src/avr/rt.rs's
/// `__unhandled_interrupt` zeroes r1 and jumps in, from inside the interrupt.
/// CPUINT still has that interrupt marked as running then (LVL0EX, which
/// only a `reti` clears, section 15.5.2), and this never `reti`s. That's fine,
/// since all it does after turning the light off is sleep. That interrupt's
/// own flag and enable stay up, since nothing here knows which peripheral it
/// was, and the loop round the `sleep` covers it waking anyway.
///
/// Interrupts go off first, so nothing else runs from here on. Then the
/// output goes off in the usual order ([`set_output`]), which stops TCB0 and
/// its interrupt too. The PIT's and PORTD's interrupts go off after it, which
/// leaves none enabled, and the last step is power-down, with the ADC off as
/// for any rest ([`prepare_sleep`]).
///
/// With no interrupt enabled and SREG.I clear, only a reset wakes the chip.
/// The `sleep` is in a loop in case something wakes it all the same.
///
/// What comes next depends on the 32 kHz oscillator. If it's running, so is
/// the watchdog, which counts in every sleep mode. Nothing here feeds it, so
/// it resets the chip, within 64 WDT clocks of the last feed under the lit
/// setting (about 63 ms), or within 8,192 under the rest setting (about 8 s,
/// `kr1k::watchdog::REST_CLOCKS`), and that boot comes up off and as a fault.
/// If the oscillator's stopped there's no reset. The chip stays here, off, at
/// power-down current, until the cell comes out.
pub fn halt() -> ! {
    // SAFETY: `cli` only clears SREG.I. With no `nomem`, it's also a compiler
    // barrier, so nothing below can move above it.
    unsafe { asm!("cli", options(nostack)) };
    set_output(Output::OFF);
    // TWI0's host off, in case an interrupt with no handler caught
    // [`set_aux`] mid-update. The host stops in every sleep mode (section
    // 29.3.5), and off it lets go of both lines. The aux stay as the AW2016
    // has them.
    write(reg::TWI0_MCTRLA, 0);
    write(reg::RTC_PITINTCTRL, 0);
    pin_ctrl(reg::PORTD_PIN0CTRL, SWITCH, regs::PORT_ISC_INTDISABLE);
    // INTDISABLE stops new edges raising the flag, and doesn't clear one
    // that's already up, which could otherwise wake every `sleep` below.
    out!(reg::VPORTD_INTFLAGS, 1 << SWITCH);
    prepare_sleep(true);
    loop {
        // SAFETY: `sleep` stops the CPU. It touches no memory and no flags.
        unsafe { asm!("sleep", options(nostack, preserves_flags)) };
    }
}

/// RTC_PIT, the 64 Hz tick. It clears the PIT's flag and sets [`TICK`], and
/// that's all, since the loop only reads or clears that with interrupts off.
#[unsafe(no_mangle)]
pub extern "avr-interrupt" fn rtc_pit() {
    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);
    out!(reg::GPR_GPR0, TICK);
}

/// TCB0_INT, the end of each TCB0 period while the light's on. It clears the
/// flag and sets [`PERIOD`] for [`wait_for_tick`] to count.
#[unsafe(no_mangle)]
pub extern "avr-interrupt" fn tcb0_int() {
    write(reg::TCB0_INTFLAGS, regs::TCB_CAPT);
    out!(reg::GPR_GPR1, PERIOD);
}

/// PORTD_PORT, an edge on PD4. It clears the flag and sets [`SWITCHED`],
/// which [`wait_for_tick`] reads with interrupts off. The wake is the point,
/// and the loop reads the switch itself.
#[unsafe(no_mangle)]
pub extern "avr-interrupt" fn portd_port() {
    out!(reg::VPORTD_INTFLAGS, 1 << SWITCH);
    out!(reg::GPR_GPR2, SWITCHED);
}

/// Points the ADC at `channel`, with that channel's reference, turning it on
/// if a rest turned it off. The cell's VDD / 10 reads against 1.024 V, as
/// the stock firmware has it, and the temperature sensor against 2.048 V,
/// which its factory calibration is for (section 33.3.3.8). Ground, the
/// check on the ADC itself, reads against 1.024 V like the cell. The
/// reference starts when the ADC asks for it, in about 10 µs (table 38-18).
fn adc_channel(channel: Channel) {
    let (reference, muxpos) = match channel {
        Channel::Voltage => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_VDDDIV10),
        Channel::Temperature => (regs::VREF_REFSEL_2V048, regs::ADC_MUXPOS_TEMPSENSE),
        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_GND),
        Channel::Vddio2 => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_VDDIO2DIV10),
    };
    write(reg::VREF_ADC0REF, reference);
    write(reg::ADC0_MUXPOS, muxpos);
    write(
        reg::ADC0_CTRLA,
        regs::ADC_ENABLE | regs::ADC_RESSEL_12BIT | regs::ADC_CONVMODE_SINGLEENDED,
    );
}

/// Starts a conversion, four accumulated, setting the ADC up for its channel
/// first if asked. `sensors::Schedule` only asks when the channel changes, or
/// after a rest, and throws the first result after a setup away. Four
/// conversions take about 200 µs and carry on through idle.
///
/// It never clears RESRDY. That works because [`adc_result`] runs every tick
/// and reading the result clears it, so a stale one is never read twice.
pub fn adc_start(convert: Convert) {
    if convert.setup {
        adc_channel(convert.channel);
    }
    write(reg::ADC0_COMMAND, regs::ADC_STCONV);
}

/// The last conversion's result, if it's finished and not been read yet, from
/// 0 to 16,380. Reading `RES` clears RESRDY (section 33.5.12), and it's read
/// low byte first, through TEMP (section 8.10.3).
pub fn adc_result() -> Option<u16> {
    if read(reg::ADC0_INTFLAGS) & regs::ADC_RESRDY == 0 {
        return None;
    }
    let lo = read(reg::ADC0_RES);
    let hi = read(reg::ADC0_RES + 1);
    Some(u16::from_le_bytes([lo, hi]))
}

/// The last raw readings of the cell and VDDIO2, as
/// `kr1k::sensors::Supplies::to_bytes` lays them out, for `cargo xtask peek`
/// to read over UPDI while the firmware runs. It's in its own section,
/// `.supplies`, which link.x puts first in SRAM, so it stays at
/// `kr1k::sensors::SUPPLIES_AT` whatever other statics the firmware gains,
/// and xtask's simulator checks the ELF has it there. That section is
/// outside .bss, so startup doesn't zero it. `main` does, with
/// [`record_supplies`], as soon as PC1 is held low, and zeros are what
/// `peek` reads as nothing read yet. Nothing in the firmware reads it back.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".supplies")]
static mut SUPPLIES: [u8; 4] = [0; 4];

/// Keeps `bytes` in [`SUPPLIES`]. `main` calls it every pass. It's four
/// stores to ordinary SRAM, which have no side effect, so writing the same
/// bytes again costs less code than checking for a change first, and errata
/// 2.2.3 doesn't come into it.
pub fn record_supplies(bytes: [u8; 4]) {
    let to = (&raw mut SUPPLIES).cast::<u8>();
    for (i, byte) in bytes.into_iter().enumerate() {
        // SAFETY: a volatile byte write inside the 4-byte static, through a
        // raw pointer, with no reference made. Only the main loop calls
        // this, and no handler touches it.
        unsafe { write_volatile(to.wrapping_add(i), byte) }
    }
}

/// A 16-bit word of the signature row, which is plain memory in data space.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "addr is a signature row word in data space"
)]
fn signature_word(addr: usize) -> u16 {
    u16::from_le_bytes([read(addr), read(addr + 1)])
}

/// The temperature sensor's factory calibration from the signature row
/// (section 8.6.2.2).
pub fn temperature_calibration() -> Calibration {
    Calibration {
        offset: signature_word(reg::SIGROW_TEMPSENSE1),
        slope: signature_word(reg::SIGROW_TEMPSENSE0),
    }
}

/// Starts the watchdog in window mode, 32 WDT clocks closed and 32 open. Until
/// the first feed there's no closed window, only the 32-clock timeout, and the
/// first pass of the loop feeds it.
///
/// It waits for the write to sync (section 22.3.6), so that feed can't land
/// while it's still syncing. The WDT shares its oscillator with the PIT, so if
/// that's dead these waits never end, and neither would the loop's first wait
/// for a tick, which has no deadline with the light off. The light's off at
/// this point, so that's where it stays. Never set WDT.STATUS.LOCK, and never
/// touch the WDTCFG fuse.
pub fn start_watchdog() {
    set_watchdog(Setting::Lit);
    watchdog_synced();
}

/// Writes one of `kr1k::watchdog`'s settings to WDT.CTRLA, once any write
/// before it has synced (section 22.3.6), and returns while this one syncs.
/// [`watchdog_synced`] waits for it. `kr1k::watchdog::Pacer` only asks for
/// this at the start of a pass a tick after the last `wdr`, which has long
/// synced by then. Whether the write restarts the count isn't in the
/// datasheet, and the pacer doesn't count on it. Never 0, which turns the
/// watchdog off, and never WDT.STATUS.LOCK.
pub fn set_watchdog(setting: Setting) {
    watchdog_synced();
    protected_write(reg::WDT_CTRLA, watchdog_setting(setting));
}

/// Waits for a write to WDT.CTRLA to sync. The WDT shares its oscillator
/// with the PIT, so if that's dead this never ends, and neither would the
/// wait for the next tick, which has no deadline with the light off. The
/// light's always off when it runs.
pub fn watchdog_synced() {
    while read(reg::WDT_STATUS) & regs::WDT_SYNCBUSY != 0 {}
}

/// Tells the watchdog the loop is still running. `main` calls this at the end
/// of a pass, after the output's been written, when `watchdog::Feeder` says
/// so. Too early or too late and the chip resets.
pub fn feed_watchdog() {
    // SAFETY: `wdr` only resets the watchdog's counter. It touches no memory
    // and no flags. No `nomem`, so the compiler can't move it above the
    // output's volatile stores.
    unsafe { asm!("wdr", options(nostack, preserves_flags)) }
}

/// What caused this reset, from `RSTCTRL.RSTFR`, leaving it clear for the
/// next one. The flags are sticky, and writing 1 clears any of them (section
/// 14.5.1), so this has to run on every boot before anything could reset the
/// chip.
pub fn take_reset_flags() -> u8 {
    let flags = read(reg::RSTCTRL_RSTFR);
    write(reg::RSTCTRL_RSTFR, flags);
    flags
}

/// Has CRCSCAN check all of flash against the CRC `cargo xtask hex` puts in
/// its last two bytes, and says whether it matched. Run it before turning
/// anything on, since the scan stalls the CPU until it's done, 16,384 words
/// at three clocks each (section 30.3.2), about 12 ms at the reset clock of
/// 4 MHz.
///
/// STATUS.OK reads 1 from reset, before any scan, and the scan only starts
/// three cycles after ENABLE while the CPU carries on (section 30.3.1). So
/// `nop`s go between the write and the first read, or a read could see the
/// reset value and pass a flash that's wrong. Three would do if the three
/// cycles count from the end of the `sts`; the datasheet doesn't say, so
/// there's a fourth. Then it waits for BUSY to clear, which it should
/// already have done, since the CPU was stalled.
///
/// SRC = FLASH is the whole of flash. Writing ENABLE starts a scan even when
/// it's already 1.
pub fn flash_ok() -> bool {
    write(reg::CRCSCAN_CTRLB, regs::CRCSCAN_SRC_FLASH);
    write(reg::CRCSCAN_CTRLA, regs::CRCSCAN_ENABLE);
    // SAFETY: four `nop`s, which touch nothing.
    unsafe {
        asm!(
            "nop",
            "nop",
            "nop",
            "nop",
            options(nostack, preserves_flags)
        )
    };
    while read(reg::CRCSCAN_STATUS) & regs::CRCSCAN_BUSY != 0 {}
    read(reg::CRCSCAN_STATUS) & regs::CRCSCAN_OK != 0
}

/// The fuse bytes `selftest::Fuses::ok` checks, read from data space. This
/// only ever reads them.
pub fn fuses() -> Fuses {
    Fuses {
        wdtcfg: read(reg::FUSE_WDTCFG),
        bodcfg: read(reg::FUSE_BODCFG),
        osccfg: read(reg::FUSE_OSCCFG),
        syscfg0: read(reg::FUSE_SYSCFG0),
        syscfg1: read(reg::FUSE_SYSCFG1),
        codesize: read(reg::FUSE_CODESIZE),
        bootsize: read(reg::FUSE_BOOTSIZE),
    }
}

/// Sends one of `kr1k::aw2016::Aw2016`'s steps to the AW2016 over TWI0, and
/// says whether every transfer went through. The button LED on PA4 stays an
/// output at 0, off.
///
/// TWI0's host is on only for this, polled with no interrupts, and off again
/// before it returns, whatever happened, so the loop never sleeps with it on.
/// It stops in every sleep mode (section 29.3.5), and turning it off lets go
/// of both lines. Turning it on leaves the bus state unknown, so it's forced
/// idle first (section 29.3.2.1.1).
///
/// Each transfer is the AW2016's address with the write bit, then the
/// register, then the data, then a STOP, and every wait counts down one
/// budget, [`TWI_POLLS`] reads of MSTATUS for the whole update. A NACK, lost
/// arbitration, a bus error or the budget running out ends the update there,
/// with nothing retried. After a NACK the host still owns the bus, so it
/// sends a STOP, as section 29.3.2.2.3 recommends. After lost arbitration or
/// a bus error it doesn't own it, and with the budget gone a byte may still
/// be on the bus, so it turns the host off without one. The AW2016 is left
/// however far the update got, and `kr1k::aw2016::Aw2016::failed` takes it
/// from there.
pub fn set_aux(update: Update) -> bool {
    write(reg::TWI0_MCTRLA, regs::TWI_ENABLE);
    write(reg::TWI0_MSTATUS, regs::TWI_BUSSTATE_IDLE);
    let mut polls = TWI_POLLS;
    let mut sent = true;
    for transfer in update {
        if !twi_transfer(transfer, &mut polls) {
            sent = false;
            break;
        }
    }
    write(reg::TWI0_MCTRLA, 0);
    sent
}

/// How a byte on the bus ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sent {
    /// Acknowledged.
    Acked,
    /// Not acknowledged, with the host still owning the bus.
    Nacked,
    /// Lost arbitration or a bus error, so the host no longer owns it.
    Lost,
    /// The update's budget ran out with no WIF.
    TimedOut,
}

/// One transfer, its address, register and data, then a STOP if the host
/// still owns the bus. True if it all went through.
fn twi_transfer(transfer: &aw2016::Write, polls: &mut u16) -> bool {
    write(reg::TWI0_MADDR, aw2016::ADDRESS << 1);
    let mut sent = twi_wait(polls);
    if sent == Sent::Acked {
        sent = twi_send(transfer.register, polls);
    }
    for &byte in transfer.data {
        if sent != Sent::Acked {
            break;
        }
        sent = twi_send(byte, polls);
    }
    match sent {
        Sent::Lost | Sent::TimedOut => false,
        Sent::Acked | Sent::Nacked => {
            write(reg::TWI0_MCTRLB, regs::TWI_MCMD_STOP);
            twi_idle(polls) && sent == Sent::Acked
        }
    }
}

/// Puts `byte` on the bus and waits for it.
fn twi_send(byte: u8, polls: &mut u16) -> Sent {
    write(reg::TWI0_MDATA, byte);
    twi_wait(polls)
}

/// Waits for WIF, which comes up at the end of an address or a byte however
/// it went (section 29.5.6), out of what's left of `polls`.
fn twi_wait(polls: &mut u16) -> Sent {
    loop {
        let status = read(reg::TWI0_MSTATUS);
        if status & regs::TWI_WIF != 0 {
            return if status & (regs::TWI_ARBLOST | regs::TWI_BUSERR) != 0 {
                Sent::Lost
            } else if status & regs::TWI_RXACK != 0 {
                Sent::Nacked
            } else {
                Sent::Acked
            };
        }
        match polls.checked_sub(1) {
            Some(left) => *polls = left,
            None => return Sent::TimedOut,
        }
    }
}

/// Waits for the bus to go idle after a STOP, which raises no flag, out of
/// what's left of `polls`. False if it never does.
fn twi_idle(polls: &mut u16) -> bool {
    loop {
        if read(reg::TWI0_MSTATUS) & regs::TWI_BUSSTATE == regs::TWI_BUSSTATE_IDLE {
            return true;
        }
        match polls.checked_sub(1) {
            Some(left) => *polls = left,
            None => return false,
        }
    }
}

/// Whether the switch is down. It pulls PD4 low against the external
/// pull-up. The loop calls this once a tick.
///
/// IN stops following the pin in power-down, since the clock that syncs it
/// has stopped, and the datasheet doesn't say how many clocks it takes to
/// catch up after a wake. The loop gets here once the PIT's handler has run
/// and returned, dozens of clocks on, which ought to be plenty. Nobody's
/// measured it.
pub fn switch_pressed() -> bool {
    read(reg::VPORTD_IN) & (1 << SWITCH) == 0
}

/// [`switch_pressed`] for a pass of the loop, which also clears
/// [`SWITCHED`], with interrupts off so no edge lands between the two.
///
/// An edge before the read is one this read already shows. Left up, it
/// would end the next slow sleep at once, for a press the pass has had, and
/// cost a trip back to 64 Hz, and an edge that came while a tick was already
/// due did just that, since [`wait_for_tick`] takes the tick first. An edge
/// after the read sets it again, and ends the next slow sleep as it should,
/// so a press that comes during the pass isn't left for the next slow tick.
/// One held off by the `cli` runs its handler straight after the `sei`, so
/// it counts as after.
pub fn take_switch() -> bool {
    // SAFETY: `cli` only clears SREG.I. With no `nomem`, it's also a
    // compiler barrier, so the clear and the read stay after it.
    unsafe { asm!("cli", options(nostack)) };
    out!(reg::GPR_GPR2, 0);
    let pressed = switch_pressed();
    // SAFETY: `sei` only sets SREG.I, and is a compiler barrier too.
    unsafe { asm!("sei", options(nostack)) };
    pressed
}

/// Spins for about `loops` × 3 main clocks, 0.75 µs a loop at 4 MHz.
fn spin(loops: u8) {
    // SAFETY: counts a register down to zero. Touches no memory. `dec`
    // changes the status flags, so there's no `preserves_flags`, and with no
    // `nomem` it's a compiler barrier, so the DAC and VREF stores either side
    // stay either side.
    unsafe {
        asm!(
            "1:",
            "dec {n}",
            "brne 1b",
            n = inout(reg) loops => _,
            options(nostack),
        )
    };
}

/// Passes of [`spin_ms`]'s loop in a millisecond. Each pass is eight
/// instructions, so a millisecond is 4,000 of them, which is the 4,000 main
/// clocks of a millisecond at 4 MHz if every instruction took one clock, and
/// that's how the simulator counts it. On the chip a pass is ten clocks (six
/// `nop`s, `sbiw` and a taken `brne` at two each), so it's about 1.25 ms.
const PASSES_PER_MS: u16 = 500;

/// Spins for `ms` milliseconds by the simulator's count, about 1.25 × `ms`
/// on the chip (see [`PASSES_PER_MS`]), with no timer and no interrupt.
/// Only examples/first_light.rs waits like this; the firmware times
/// everything off its tick.
#[allow(
    dead_code,
    reason = "examples/first_light.rs's waits, which the firmware doesn't use"
)]
pub fn spin_ms(ms: u16) {
    for _ in 0..ms {
        // SAFETY: counts a register pair down to zero. Touches no memory.
        // `sbiw` changes the status flags, so there's no `preserves_flags`,
        // and with no `nomem` it's a compiler barrier, so the stores either
        // side of a wait stay either side.
        unsafe {
            asm!(
                "1:",
                "nop",
                "nop",
                "nop",
                "nop",
                "nop",
                "nop",
                "sbiw {n}, 1",
                "brne 1b",
                n = inout(reg_iw) PASSES_PER_MS => _,
                options(nostack),
            )
        };
    }
}

/// Loops of [`spin`] for DAC0's output to fall to 0 before a reference or a
/// path changes, about 20 µs. The datasheet's settling time is 7 µs typical
/// for a full-scale step into 50 pF (table 38-23), and the DAC can sink only
/// 0.001 mA, so how fast PD6 really falls depends on what the board pulls it
/// down with, which nobody's measured. This is about three times the typical
/// figure, a guess.
const DAC_FALLS: u8 = 27;

/// Loops of [`spin`] for a change of reference to settle, about 3 µs against
/// the 2 µs of table 38-18.
const REFERENCE_SETTLES: u8 = 4;

/// Sets the driver for `output`, in an order meant never to ask for more
/// current than the old setting or the new one does.
///
/// - Off: DAC0 to 0, its reference to 1.024 V, PC1 low, the paths low, and
///   TCB0 stopped. That's the stock firmware's off order, the only one known
///   to have run on this board.
/// - On, with PC1 low: DAC0 to 0 at 1.024 V and the paths low first, TCB0
///   started, then PC1 high, and nothing else. PC1 only ever rises with the
///   DAC at 0, whatever `output` says. `torch::Torch` sends
///   [`Output::STARTING`] for the first tick of any light, so the level comes
///   a tick later, 15.6 ms with the driver on and asked for nothing, which is
///   the stock firmware's order (PC1, then 8 ms, then the DAC).
/// - On, with PC1 high: if the reference or the paths change, DAC0 goes to 0
///   first, a wait of [`DAC_FALLS`] for it to get there, then the reference
///   and a wait of [`REFERENCE_SETTLES`], then the paths,
///   then the new code. A new path, or a higher reference, with the old code
///   still in DAC0 could ask for a hundred times the current for a moment.
///   If only the code changes, it's written straight in. How fast PD6
///   really falls to 0 is unmeasured, so the wait is a guess (H3 in
///   docs/safety.md).
///
/// DAC0's output follows its high byte (section 34.5.2), and [`write16`]
/// writes that second, so a code changes in one step.
///
/// The pin and DAC0 sequencing is in [`output_off`], [`output_start`] and
/// [`output_level`], which examples/first_light.rs calls without TCB0. This
/// adds TCB0, started just before PC1 rises and stopped once it's low.
pub fn set_output(output: Output) {
    if !output.enable {
        output_off();
        write(reg::TCB0_CTRLA, 0);
        write(reg::TCB0_INTCTRL, 0);
        return;
    }

    if !lit() {
        output_start(|| {
            // TCB0 from the start of a period, its flag cleared before its
            // interrupt goes on, and GPR1 cleared of anything stale.
            out!(reg::GPR_GPR1, 0);
            write16(reg::TCB0_CNT, 0);
            write(reg::TCB0_INTFLAGS, regs::TCB_CAPT);
            write(reg::TCB0_INTCTRL, regs::TCB_CAPT);
            write(reg::TCB0_CTRLA, regs::TCB_CLKSEL_DIV1 | regs::TCB_ENABLE);
        });
        return;
    }

    output_level(output);
}

/// The driver off, in the stock firmware's order, DAC0 to 0, its reference
/// to 1.024 V, PC1 low, then the paths low. [`set_output`]'s off.
pub fn output_off() {
    write16(reg::DAC0_DATA, 0);
    write(reg::VREF_DAC0REF, regs::VREF_REFSEL_1V024);
    out!(reg::VPORTC_OUT, 0);
    out!(reg::VPORTA_OUT, 0);
}

/// The driver on and asked for nothing, DAC0 to 0 at 1.024 V and the paths
/// low, then `just_before`, then PC1 high. Call it with PC1 low, and
/// [`output_level`] no sooner than 8 ms after. [`set_output`] starts TCB0 in
/// `just_before`; examples/first_light.rs passes nothing.
pub fn output_start(just_before: impl FnOnce()) {
    write16(reg::DAC0_DATA, 0);
    write(reg::VREF_DAC0REF, regs::VREF_REFSEL_1V024);
    out!(reg::VPORTA_OUT, 0);
    just_before();
    out!(reg::VPORTC_OUT, 1 << ENABLE);
}

/// A level for the driver with PC1 already high, DAC0 to 0 and the waits
/// before a change of reference or paths, then the new code (see
/// [`set_output`]).
pub fn output_level(output: Output) {
    let reference = match output.vref {
        Vref::V1024 => regs::VREF_REFSEL_1V024,
        Vref::V2500 => regs::VREF_REFSEL_2V500,
    };
    let paths = output.paths.mask();
    if read(reg::VREF_DAC0REF) != reference || read(reg::VPORTA_OUT) != paths {
        write16(reg::DAC0_DATA, 0);
        spin(DAC_FALLS);
        write(reg::VREF_DAC0REF, reference);
        spin(REFERENCE_SETTLES);
        out!(reg::VPORTA_OUT, paths);
    }
    write16(reg::DAC0_DATA, output.dac.min(DAC_MAX) << DAC_SHIFT);
}
