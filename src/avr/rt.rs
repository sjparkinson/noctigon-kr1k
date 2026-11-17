//! The vector table and what runs between reset and `main`.

#![allow(unsafe_code)]

use core::arch::global_asm;

use crate::regs;

const _: () = assert!(
    0 < regs::VECTOR_RTC_PIT
        && regs::VECTOR_RTC_PIT < regs::VECTOR_TCB0_INT
        && regs::VECTOR_TCB0_INT < regs::VECTOR_PORTD_PORT
        && regs::VECTOR_PORTD_PORT < regs::VECTORS
);

global_asm!(
    // 36 slots, slot 30 among them though nothing uses it (DS40002413B table
    // 9-3), each a two-word `jmp`.
    ".section .vectors, \"ax\", @progbits",
    ".global __vectors",
    "__vectors:",
    "  jmp __reset",
    "  .rept {before_pit}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "  jmp {rtc_pit}",
    "  .rept {before_tcb0}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "  jmp {tcb0_int}",
    "  .rept {before_portd}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "  jmp {portd_port}",
    "  .rept {after_portd}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "",
    // No `reti`, which would return with the interrupt's flag still up and
    // take it again after every instruction (DS40002413B section 9.2). r1 is
    // zeroed since an interrupt can land straight after a `mul`.
    ".section .text.__unhandled_interrupt, \"ax\", @progbits",
    "__unhandled_interrupt:",
    "  eor r1, r1",
    "  jmp {halt}",
    "",
    ".section .text.__reset, \"ax\", @progbits",
    ".global __reset",
    "__reset:",
    // Compiled code assumes r1 is zero. Reset leaves SP at the top of SRAM
    // (DS40002413B section 7.6.2).
    "  clr r1",
    "  out {sreg}, r1",
    "  ldi r26, lo8(__data_start)",
    "  ldi r27, hi8(__data_start)",
    "  ldi r30, lo8(__data_load)",
    "  ldi r31, hi8(__data_load)",
    "  ldi r24, lo8(__data_end)",
    "  ldi r25, hi8(__data_end)",
    // The test comes first, so an empty .data copies nothing.
    "  rjmp 2f",
    "1:",
    "  ld r0, Z+",
    "  st X+, r0",
    "2:",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 1b",
    "  ldi r26, lo8(__bss_start)",
    "  ldi r27, hi8(__bss_start)",
    "  ldi r24, lo8(__bss_end)",
    "  ldi r25, hi8(__bss_end)",
    "  rjmp 4f",
    "3:",
    "  st X+, r1",
    "4:",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 3b",
    "  call main",
    // `main` never returns. This keeps a return out of what's next in flash.
    "5:",
    "  rjmp 5b",
    before_pit = const regs::VECTOR_RTC_PIT - 1,
    rtc_pit = sym super::hw::rtc_pit,
    before_tcb0 = const regs::VECTOR_TCB0_INT - regs::VECTOR_RTC_PIT - 1,
    tcb0_int = sym super::hw::tcb0_int,
    before_portd = const regs::VECTOR_PORTD_PORT - regs::VECTOR_TCB0_INT - 1,
    portd_port = sym super::hw::portd_port,
    after_portd = const regs::VECTORS - regs::VECTOR_PORTD_PORT - 1,
    halt = sym super::hw::halt,
    sreg = const regs::addr::CPU_SREG,
);
