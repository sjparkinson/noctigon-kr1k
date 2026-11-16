/*
 * AVR32DD20: 32 KB flash, 4 KB SRAM, 256 B EEPROM.
 *
 * AVR ELF files put data space at 0x800000, so SRAM (data address 0x7000)
 * is 0x807000 here. The chip also maps flash into data space at 0x8000, a
 * 32 KB window (DS40002413A section 11.3.2), so read-only data is linked at
 * its flash address + 0x808000 and read with ordinary loads, no progmem
 * dance needed. NVMCTRL.CTRLB.FLMAP picks which 32 KB of flash the window
 * shows, and on a part with 32 KB of flash every value shows all of it
 * (DS40002413A section 11.5.2). Its reset value is 0x30, SECTION3, and
 * nothing in the firmware writes it.
 */
MEMORY {
  FLASH  (rx)   : ORIGIN = 0x000000, LENGTH = 32K
  MAPPED (r)    : ORIGIN = 0x808000, LENGTH = 32K
  RAM    (rw!x) : ORIGIN = 0x807000, LENGTH = 4K
}

ENTRY(__reset)

SECTIONS {
  .text : {
    KEEP(*(.vectors))
    *(.text .text.*)
  } > FLASH

  .rodata (ADDR(.text) + SIZEOF(.text) + 0x808000) : AT(ADDR(.text) + SIZEOF(.text)) {
    *(.rodata .rodata.*)
  } > MAPPED

  .data : AT(LOADADDR(.rodata) + SIZEOF(.rodata)) {
    __data_start = .;
    *(.data .data.*)
    . = ALIGN(2);
    __data_end = .;
  } > RAM

  /* Where src/avr/rt.rs copies .data from, as a mapped-flash data address. */
  __data_load = LOADADDR(.data) + 0x8000;

  .bss (NOLOAD) : {
    __bss_start = .;
    *(.bss .bss.*)
    . = ALIGN(2);
    __bss_end = .;
  } > RAM

  /* The stack grows down from the top of SRAM, where the CPU starts SP. */

  /DISCARD/ : {
    *(.eh_frame*)
  }
}
