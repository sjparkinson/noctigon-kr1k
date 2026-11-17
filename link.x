/*
 * AVR ELF puts data space at 0x800000. Flash is mapped into data space at
 * 0x8000 (DS40002413A §11.3.2), so .rodata sits at 0x808000 and up and is
 * read with ordinary loads. With 32 KB of flash every FLMAP value shows all
 * of it (§11.5.2).
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

  /*
   * First in SRAM so `cargo xtask peek` finds it at kr1k::sensors::SUPPLIES_AT.
   * NOLOAD and outside .bss, so rt.rs leaves it alone and `main` zeroes it.
   */
  .supplies (NOLOAD) : {
    KEEP(*(.supplies))
  } > RAM

  .data : AT(LOADADDR(.rodata) + SIZEOF(.rodata)) {
    __data_start = .;
    *(.data .data.*)
    . = ALIGN(2);
    __data_end = .;
  } > RAM

  /* A mapped-flash data address. */
  __data_load = LOADADDR(.data) + 0x8000;

  .bss (NOLOAD) : {
    __bss_start = .;
    *(.bss .bss.*)
    . = ALIGN(2);
    __bss_end = .;
  } > RAM

  /DISCARD/ : {
    *(.eh_frame*)
  }
}
