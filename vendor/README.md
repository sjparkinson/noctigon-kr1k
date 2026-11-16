# vendor

Third-party files kept in the repo so register checks work offline and against a fixed copy. Nothing here is compiled into the firmware or xtask.

## AVR32DD20.atdf

Microchip's device description for the AVR32DD20, the MCU on the KR1K's driver. It's the source for every register address, bit mask, interrupt vector, memory segment and fuse in `src/regs.rs`, which `build.rs` generates from it, and so for `src/avr/hw.rs`, `src/avr/rt.rs`, `link.x` and `xtask/src/updi.rs`.

- From Microchip's AVR Dx device pack, [`Microchip.AVR-Dx_DFP.2.8.343.atpack`](https://packs.download.microchip.com/Microchip.AVR-Dx_DFP.2.8.343.atpack) (released 2026-04-11, SHA-256 `d81af59072e5ac188431e828461eee5c08e6d5f3ace32461328b4da0ec348746`), file `atdf/AVR32DD20.atdf`, unchanged.
- SHA-256 `5b7fc47eed2d281b3c2b6553b2629a8d19195fa62dd9a0288caa92752a8f9d32`.
- Copyright Microchip Technology Inc., Apache License 2.0. `LICENSE` is byte for byte the pack's `LICENSE.txt`, and `LICENSE-APACHE-2.0.txt` is the full licence text. This file isn't covered by the repo's MIT licence.

The rest of the pack is C and assembler support (headers, startup objects, linker specs) and MPLAB device files, all generated from the same data, so it isn't vendored. `include/avr/ioavr32dd20.h` is handy if you want the avr-libc names for something.

To update, the latest version is in the `Microchip.AVR-Dx_DFP.pdsc` entry of [the pack index](https://packs.download.microchip.com/index.idx). An `.atpack` is a zip. Replace the file with `atdf/AVR32DD20.atdf` from the new pack, update the version and both hashes above, and run `cargo test --workspace`, which checks every constant in the repo against it.
