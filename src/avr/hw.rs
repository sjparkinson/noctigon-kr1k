//! The KR1K's Lume X1-C driver pins and the AVR32DD20 registers that drive
//! them. Section and table numbers are DS40002413B's, errata DS80001026D's.

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
use kr1k::watchdog::{
    CLOCK_HZ, Pit, Setting, Sleep, TICK_DEADLINE_PERIODS, TIMER_PERIOD_CLOCKS, Wake,
};

use crate::regs::{self, addr as reg};

/// 0xF2 is what the stock build on this torch writes to VPORTA.DIR at reset.
const PORTA_OUTPUTS: u8 = PATHS | 1 << BUTTON_LED | 1 << HELD_LOW;
const _: () = assert!(PORTA_OUTPUTS == 0xF2);
const PORTC_OUTPUTS: u8 = 1 << ENABLE;
const PORTD_OUTPUTS: u8 = 1 << DAC;

/// DAC0's 10-bit code is left-aligned in `DAC0.DATA` (section 34.5.2).
const DAC_SHIFT: u32 = regs::DAC_DATA.trailing_zeros();
const _: () = assert!(DAC_MAX << DAC_SHIFT == regs::DAC_DATA);

const TIMER_TOP: u16 = TIMER_PERIOD_CLOCKS - 1;

/// MBAUD for a 100 kHz SCL at the 4 MHz main clock (section 29.3.2.2.1).
const TWI_BAUD: u8 = 15;
const _: () = assert!(CLOCK_HZ == (10 + 2 * 15) * 100_000);

/// Reads of TWI0.MSTATUS, for a whole [`set_aux`] update.
const TWI_POLLS: u16 = 900;

const _: () = {
    use kr1k::watchdog::{CLOSED_CLOCKS, OPEN_CLOCKS, REST_CLOCKS};
    assert!(8 << ((regs::WDT_WINDOW_32CLK >> 4) - 1) == CLOSED_CLOCKS);
    assert!(8 << (regs::WDT_PERIOD_32CLK - 1) == OPEN_CLOCKS);
    assert!(8 << (regs::WDT_PERIOD_8KCLK - 1) == REST_CLOCKS);
};

const fn watchdog_setting(setting: Setting) -> u8 {
    match setting {
        Setting::Lit => regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK,
        Setting::Rest => regs::WDT_PERIOD_8KCLK,
    }
}

const fn pit_setting(pit: Pit) -> u8 {
    match pit {
        Pit::Fast => regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN,
        Pit::Slow => regs::RTC_PERIOD_CYC32768 | regs::RTC_PITEN,
    }
}

/// Cycles of the 32 kHz clock in a PIT period (section 26.13.12's table).
const fn pit_cycles(value: u8) -> u32 {
    2 << (value >> regs::RTC_PERIOD.trailing_zeros())
}

const _: () = {
    let slow = kr1k::watchdog::SLOW_TICKS;
    assert!(pit_cycles(regs::RTC_PERIOD_CYC512) == 512);
    assert!(pit_cycles(regs::RTC_PERIOD_CYC32768) == 512 << slow.trailing_zeros());
    assert!(slow.is_power_of_two());
};

/// For addresses of 64 or over. Anything lower goes through [`out!`].
fn write(addr: usize, value: u8) {
    // SAFETY: every caller passes a `reg` constant, a memory-mapped register.
    unsafe { write_volatile(with_exposed_provenance_mut::<u8>(addr), value) }
}

fn read(addr: usize) -> u8 {
    // SAFETY: as for `write`.
    unsafe { read_volatile(with_exposed_provenance::<u8>(addr)) }
}

/// Errata 2.2.3 loses a store under 64, or to `SLPCTRL.CTRLA`, that comes
/// straight after a store at 64 or over. `out` is the workaround.
macro_rules! out {
    ($addr:expr, $value:expr) => {{
        const ADDR: usize = $addr;
        const _: () = assert!(ADDR < 0x40);
        let value: u8 = $value;
        // SAFETY: `out` writes one I/O register, no Rust memory and no flags.
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

/// Low byte first, then high, through the shared TEMP byte (section 8.10.3).
#[expect(
    clippy::arithmetic_side_effects,
    reason = "addr is the low byte of a 16-bit register in data space, far under usize::MAX"
)]
fn write16(addr: usize, value: u16) {
    let [lo, hi] = value.to_le_bytes();
    write(addr, lo);
    write(addr + 1, hi);
}

/// The write only lands within four instructions of the key (section 7.4.6).
#[expect(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "usize is 16 bits on AVR, so addr fits Z whole, and try_from would need a \
              fallback address that doesn't exist"
)]
fn protected_write(addr: usize, value: u8) {
    const _: () = assert!(reg::CPU_CCP < 0x40);
    // SAFETY: writes the key to CPU.CCP and `value` to `addr`, no Rust memory.
    unsafe {
        asm!(
            "out {ccp}, {key}",
            "st Z, {value}",
            ccp = const reg::CPU_CCP,
            key = in(reg) regs::CPU_CCP_IOREG,
            value = in(reg) value,
            in("Z") addr as u16,
            options(nostack, preserves_flags),
        );
    }
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "base is a PIN0CTRL address and pin is under 8, far under usize::MAX"
)]
fn pin_ctrl(base: usize, pin: u8, value: u8) {
    write(base + usize::from(pin), value);
}

fn lit() -> bool {
    read(reg::VPORTC_OUT) & 1 << ENABLE != 0
}

/// OUT before DIR, since a start that wasn't a reset could find PC1 set.
/// Without VDDIO2, PC1 is tri-stated whatever this writes (section 19.3.2.1).
pub fn hold_enable_low() {
    out!(reg::VPORTC_OUT, 0);
    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);
}

/// Whether PORTC's pins follow their PORT registers (section 19.5.3).
pub fn vddio2_ok() -> bool {
    read(reg::MVIO_STATUS) & regs::MVIO_VDDIO2S != 0
}

pub fn init() {
    init_pins();

    // No internal pull-up, since PD4 has an external one. Every pin senses
    // asynchronously (section 18.3.3.1), so both edges wake power-down.
    pin_ctrl(reg::PORTD_PIN0CTRL, SWITCH, regs::PORT_ISC_BOTHEDGES);

    write(reg::TCB0_CTRLB, regs::TCB_CNTMODE_INT);
    write16(reg::TCB0_CCMP, TIMER_TOP);

    // MBAUD only takes a write with the host off (section 29.5.7).
    write(reg::TWI0_MBAUD, TWI_BAUD);

    // A 1 MHz ADC clock (table 38-25 allows 125 kHz to 2 MHz). The temperature
    // sensor wants 25 µs of INITDLY and 28 µs of sampling (section 33.3.3.8).
    write(reg::ADC0_CTRLB, regs::ADC_SAMPNUM_ACC4);
    write(reg::ADC0_CTRLC, regs::ADC_PRESC_DIV4);
    write(reg::ADC0_CTRLD, regs::ADC_INITDLY_DLY32);
    write(reg::ADC0_SAMPCTRL, regs::ADC_SAMPLEN_32CLK);

    write(reg::RTC_PITINTCTRL, regs::RTC_PI);
    set_pit(Pit::Fast);
}

/// Waits for any write still syncing (section 26.13.12), and for this one,
/// since the datasheet doesn't say it finishes in power-down. The first
/// period after a change is anything up to a whole one (section 26.5.2.2).
pub fn set_pit(pit: Pit) {
    while read(reg::RTC_PITSTATUS) & regs::RTC_CTRLBUSY != 0 {}
    write(reg::RTC_PITCTRLA, pit_setting(pit));
    while read(reg::RTC_PITSTATUS) & regs::RTC_CTRLBUSY != 0 {}
}

pub fn init_pins() {
    // Reset's values (sections 12.5.7 and 12.5.2), for a start that wasn't one.
    protected_write(reg::CLKCTRL_OSCHFCTRLA, regs::CLKCTRL_FRQSEL_4M);
    protected_write(reg::CLKCTRL_MCLKCTRLB, 0);

    // Unused pins get a pull-up, as the stock firmware has them, and PA1 and
    // PC1 never get one. PA2 and PA3 (TWI0) are never outputs, since SCL held
    // low for 130 ms shuts the AW2016 down, and get no pull-up, since the
    // board likely has its own. PD6's input buffer is off (section 34.3.1).
    pin_ctrl(reg::PORTA_PIN0CTRL, FET, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTA_PIN0CTRL, HELD_LOW, 0);
    pin_ctrl(reg::PORTC_PIN0CTRL, ENABLE, 0);
    pin_ctrl(reg::PORTC_PIN0CTRL, UNUSED_PC2, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTC_PIN0CTRL, UNUSED_PC3, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTD_PIN0CTRL, UNUSED_PD5, regs::PORT_PULLUPEN);
    pin_ctrl(reg::PORTD_PIN0CTRL, DAC, regs::PORT_ISC_INPUT_DISABLE);
    pin_ctrl(reg::PORTD_PIN0CTRL, POWER_BANK, regs::PORT_PULLUPEN);

    // OUT before DIR. PD6 is an output at 0, as the stock firmware has it, in
    // case DAC0 lets go of it in power-down (section 34.3.3, unconfirmed).
    out!(reg::VPORTA_OUT, 0);
    out!(reg::VPORTA_DIR, PORTA_OUTPUTS);
    out!(reg::VPORTC_DIR, PORTC_OUTPUTS);
    out!(reg::VPORTD_OUT, 0);
    out!(reg::VPORTD_DIR, PORTD_OUTPUTS);

    // DAC0 on at 0, in section 34.3.1's order.
    write(reg::VREF_DAC0REF, regs::VREF_REFSEL_1V024);
    write16(reg::DAC0_DATA, 0);
    write(reg::DAC0_CTRLA, regs::DAC_ENABLE | regs::DAC_OUTEN);
}

// One flag each, in GPR0, GPR1 and GPR2, which the handlers write whole.
const TICK: u8 = 1 << 0;

const PERIOD: u8 = 1 << 0;

const SWITCHED: u8 = 1 << 0;

/// The only write to `SLPCTRL.CTRLA`. The `nop` is errata 2.2.3's workaround.
fn sleep_mode(value: u8) {
    // SAFETY: a `nop` and one store to SLPCTRL.CTRLA, no Rust memory or flags.
    unsafe {
        asm!(
            "nop",
            "sts {addr}, {value}",
            addr = const reg::SLPCTRL_CTRLA,
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

/// Power-down stops TCB0 and DAC0 (sections 24.3.6 and 34.3.3), so with PC1
/// high this idles instead. The ADC goes off first, since a conversion caught
/// by power-down comes back invalid (section 33.3.8). SEN has to stay set.
fn prepare_sleep(rest: bool) {
    let mode = if rest && !lit() {
        write(reg::ADC0_CTRLA, 0);
        regs::SLPCTRL_SMODE_PDOWN
    } else {
        regs::SLPCTRL_SMODE_IDLE
    };
    sleep_mode(mode | regs::SLPCTRL_SEN);
}

/// `None` means the light's on and no tick came in the deadline. With PC1 low
/// there's none on purpose, so a dead 32 kHz oscillator leaves the driver off.
pub fn wait_for_tick(sleep: Sleep) -> Option<Wake> {
    prepare_sleep(sleep != Sleep::Idle);
    let mut periods = TICK_DEADLINE_PERIODS;
    let woke = loop {
        // SAFETY: `cli` only clears SREG.I. No `nomem`, so the reads stay
        // below it.
        unsafe { asm!("cli", options(nostack)) };
        if read(reg::GPR_GPR0) & TICK != 0 {
            out!(reg::GPR_GPR0, 0);
            break Some(Wake::Tick);
        }
        if read(reg::GPR_GPR2) != 0 {
            out!(reg::GPR_GPR2, 0);
            if sleep == Sleep::SwitchWakes {
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
        // SAFETY: sleeps until an interrupt, whose handler saves what it uses.
        unsafe { asm!("sei", "sleep", options(nostack)) };
    };
    // SAFETY: `sei` only sets SREG.I.
    unsafe { asm!("sei", options(nostack)) };
    woke
}

/// Also reached from an unhandled interrupt with no `reti`, so CPUINT still
/// has it running (section 15.5.2) and its flag up. The `sleep` loops.
#[unsafe(export_name = "__halt")]
pub fn halt() -> ! {
    // SAFETY: `cli` only clears SREG.I. No `nomem`, so nothing moves above it.
    unsafe { asm!("cli", options(nostack)) };
    set_output(Output::OFF);
    // In case an interrupt with no handler caught [`set_aux`] mid-update.
    write(reg::TWI0_MCTRLA, 0);
    write(reg::RTC_PITINTCTRL, 0);
    pin_ctrl(reg::PORTD_PIN0CTRL, SWITCH, regs::PORT_ISC_INTDISABLE);
    // INTDISABLE leaves a flag that's already up, which could wake the `sleep`.
    out!(reg::VPORTD_INTFLAGS, 1 << SWITCH);
    prepare_sleep(true);
    loop {
        // SAFETY: `sleep` stops the CPU. It touches no memory and no flags.
        unsafe { asm!("sleep", options(nostack, preserves_flags)) };
    }
}

pub extern "avr-interrupt" fn rtc_pit() {
    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);
    out!(reg::GPR_GPR0, TICK);
}

pub extern "avr-interrupt" fn tcb0_int() {
    write(reg::TCB0_INTFLAGS, regs::TCB_CAPT);
    out!(reg::GPR_GPR1, PERIOD);
}

pub extern "avr-interrupt" fn portd_port() {
    out!(reg::VPORTD_INTFLAGS, 1 << SWITCH);
    out!(reg::GPR_GPR2, SWITCHED);
}

/// The cell reads against 1.024 V, as the stock firmware has it. The
/// temperature sensor's factory calibration is for 2.048 V (section 33.3.3.8).
fn adc_channel(channel: Channel) {
    let (reference, muxpos) = match channel {
        Channel::Voltage => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_VDDDIV10),
        Channel::Temperature => (regs::VREF_REFSEL_2V048, regs::ADC_MUXPOS_TEMPSENSE),
        Channel::Ground => (regs::VREF_REFSEL_1V024, regs::ADC_MUXPOS_GND),
    };
    write(reg::VREF_ADC0REF, reference);
    write(reg::ADC0_MUXPOS, muxpos);
    write(
        reg::ADC0_CTRLA,
        regs::ADC_ENABLE | regs::ADC_RESSEL_12BIT | regs::ADC_CONVMODE_SINGLEENDED,
    );
}

/// Never clears RESRDY. Reading the result in [`adc_result`] does, every tick.
pub fn adc_start(convert: Convert) {
    if convert.setup {
        adc_channel(convert.channel);
    }
    write(reg::ADC0_COMMAND, regs::ADC_STCONV);
}

/// From 0 to 16,380. Reading `RES` clears RESRDY (section 33.5.12), and it's
/// read low byte first, through TEMP (section 8.10.3).
pub fn adc_result() -> Option<u16> {
    if read(reg::ADC0_INTFLAGS) & regs::ADC_RESRDY == 0 {
        return None;
    }
    let lo = read(reg::ADC0_RES);
    let hi = read(reg::ADC0_RES + 1);
    Some(u16::from_le_bytes([lo, hi]))
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "addr is a signature row word in data space"
)]
fn signature_word(addr: usize) -> u16 {
    u16::from_le_bytes([read(addr), read(addr + 1)])
}

/// The factory calibration in the signature row (section 8.6.2.2).
pub fn temperature_calibration() -> Calibration {
    Calibration {
        offset: signature_word(reg::SIGROW_TEMPSENSE1),
        slope: signature_word(reg::SIGROW_TEMPSENSE0),
    }
}

/// Waits for the sync (section 22.3.6), so the first feed can't land in it.
pub fn start_watchdog() {
    set_watchdog(Setting::Lit);
    watchdog_synced();
}

/// Returns while the write syncs. Whether it restarts the count isn't in the
/// datasheet. Never 0, which turns the watchdog off, nor WDT.STATUS.LOCK.
pub fn set_watchdog(setting: Setting) {
    watchdog_synced();
    protected_write(reg::WDT_CTRLA, watchdog_setting(setting));
}

/// Never ends if the 32 kHz oscillator is dead. The light's always off here.
pub fn watchdog_synced() {
    while read(reg::WDT_STATUS) & regs::WDT_SYNCBUSY != 0 {}
}

pub fn feed_watchdog() {
    // SAFETY: `wdr` touches no memory and no flags. No `nomem`, so the
    // compiler can't move it above the output's volatile stores.
    unsafe { asm!("wdr", options(nostack, preserves_flags)) }
}

/// The flags are sticky, and writing 1 clears them (section 14.5.1).
pub fn take_reset_flags() -> u8 {
    let flags = read(reg::RSTCTRL_RSTFR);
    write(reg::RSTCTRL_RSTFR, flags);
    flags
}

/// Stalls the CPU for about 12 ms (section 30.3.2). STATUS.OK reads 1 until
/// the scan starts three cycles after ENABLE (section 30.3.1), hence the
/// `nop`s, plus one since the datasheet doesn't say where the three count from.
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
        );
    }
    while read(reg::CRCSCAN_STATUS) & regs::CRCSCAN_BUSY != 0 {}
    read(reg::CRCSCAN_STATUS) & regs::CRCSCAN_OK != 0
}

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

/// TWI0's host is on only inside this, and nothing is retried. Turning it on
/// leaves the bus state unknown, hence forcing idle (section 29.3.2.1.1).
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sent {
    Acked,
    Nacked,
    Lost,
    TimedOut,
}

/// A STOP after a NACK, while the host still owns the bus (section
/// 29.3.2.2.3), and none after lost arbitration, a bus error or a timeout.
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

fn twi_send(byte: u8, polls: &mut u16) -> Sent {
    write(reg::TWI0_MDATA, byte);
    twi_wait(polls)
}

/// WIF comes up after an address or a byte however it went (section 29.5.6).
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

/// A STOP raises no flag, so this polls the bus state.
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

/// Active low. IN stops following the pin in power-down, and how many clocks
/// it takes to catch up after a wake isn't in the datasheet and is unmeasured.
pub fn switch_pressed() -> bool {
    read(reg::VPORTD_IN) & (1 << SWITCH) == 0
}

/// Clears [`SWITCHED`] before the read, with interrupts off, so an edge the
/// read already shows doesn't end the next slow sleep, and one after it does.
pub fn take_switch() -> bool {
    // SAFETY: `cli` only clears SREG.I. No `nomem`, so the rest stays below it.
    unsafe { asm!("cli", options(nostack)) };
    out!(reg::GPR_GPR2, 0);
    let pressed = switch_pressed();
    // SAFETY: `sei` only sets SREG.I, and is a compiler barrier too.
    unsafe { asm!("sei", options(nostack)) };
    pressed
}

/// Spins for about `loops` × 3 main clocks, 0.75 µs a loop at 4 MHz.
fn spin(loops: u8) {
    // SAFETY: counts a register down, touching no memory. `dec` changes the
    // flags, and no `nomem` keeps the stores either side where they are.
    unsafe {
        asm!(
            "1:",
            "dec {n}",
            "brne 1b",
            n = inout(reg) loops => _,
            options(nostack),
        );
    }
}

/// Loops of [`spin`] for DAC0's output to fall to 0, about 20 µs, against a
/// typical settling time of 7 µs (table 38-23). How fast PD6 really falls is
/// unmeasured, so this is a guess.
const DAC_FALLS: u8 = 27;

/// Loops of [`spin`] for a change of reference to settle, about 3 µs against
/// the 2 µs of table 38-18.
const REFERENCE_SETTLES: u8 = 4;

/// PC1 only ever rises with DAC0 at 0, whatever `output` says, so the level
/// comes a tick after [`Output::STARTING`].
pub fn set_output(output: Output) {
    if !output.enable {
        output_off();
        write(reg::TCB0_CTRLA, 0);
        write(reg::TCB0_INTCTRL, 0);
        return;
    }

    if !lit() {
        output_start(|| {
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

/// The stock firmware's off order, the only one known to have run on this
/// board.
pub fn output_off() {
    write16(reg::DAC0_DATA, 0);
    write(reg::VREF_DAC0REF, regs::VREF_REFSEL_1V024);
    out!(reg::VPORTC_OUT, 0);
    out!(reg::VPORTA_OUT, 0);
}

/// PC1 rises with DAC0 at 0 and the paths low. Call it with PC1 low, and
/// [`output_level`] no sooner than 8 ms after, the stock firmware's wait.
pub fn output_start(just_before: impl FnOnce()) {
    write16(reg::DAC0_DATA, 0);
    write(reg::VREF_DAC0REF, regs::VREF_REFSEL_1V024);
    out!(reg::VPORTA_OUT, 0);
    just_before();
    out!(reg::VPORTC_OUT, 1 << ENABLE);
}

/// For PC1 already high. DAC0 goes to 0 before the reference or paths change,
/// since the old code on a new path could ask for a hundred times the current.
/// DAC0's output follows its high byte (section 34.5.2), written second.
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
