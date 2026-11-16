# Noctigon KR1K

Rust firmware for the Noctigon KR1K torch with the Lume X1-C boost driver, an AVR32DD20 setting the LED current through its DAC. It has zero crate dependencies and is deliberately small.

It started as firmware for the Wurkkos TS26 and is being ported, so parts of it still describe that light. Nothing has run on a KR1K yet. [docs/hardware.md](docs/hardware.md) has what's known about the board, [docs/safety.md](docs/safety.md) what could go wrong, and [docs/roadmap.md](docs/roadmap.md) where the port has got to.

## Building

You need [rustup](https://rustup.rs).

```sh
cargo xtask hex
cargo test --workspace
```

See [docs/toolchain.md](docs/toolchain.md) for how it builds and links.

## Flashing

The KR1K's driver very likely has three UPDI pads, though nobody's published where they are or in what order. Take the cell out, measure the pads, put a plain 4.5 V serial UPDI programmer on them, and `cargo xtask` does the rest. Never a high-voltage programmer, and never write fuses. [docs/flashing.md](docs/flashing.md) has the whole procedure, and the first flash is a first-light image, not this.

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
cargo xtask backup /dev/cu.usbserial-XXXX backup/torch
cargo xtask flash /dev/cu.usbserial-XXXX
```

## Acknowledgements

- [Anduril](https://github.com/ToyKeeper/anduril), by ToyKeeper, the stock firmware, for the click notation and most of what's known about this hardware.
- [loneoceans](https://loneoceans.com/labs/temp/anduril/), who designed the Lume X1-C, for the hardware reference.
- [pymcuprog](https://github.com/microchip-pic-avr-tools/pymcuprog), which the UPDI programmer's sequences follow.
- [AVRDUDE](https://github.com/avrdudes/avrdude), whose flashing logic was reviewed when writing the UPDI programmer.
- Microchip, for the ATDF in `vendor/` and the datasheets.
- This firmware started as this project's own firmware for the Wurkkos TS26, and its history is that light's.
