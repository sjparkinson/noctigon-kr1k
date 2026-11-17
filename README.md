# Noctigon KR1K

Rust firmware for the Noctigon KR1K torch with the Lume X1-C boost driver, an AVR32DD20 setting the LED current through its DAC.

## Building

You need [rustup](https://rustup.rs).

```sh
cargo xtask hex
cargo test --workspace
```

See [docs/toolchain.md](docs/toolchain.md) for how it builds and links.

## Flashing

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
cargo xtask backup /dev/cu.usbserial-XXXX backup/torch
cargo xtask flash /dev/cu.usbserial-XXXX
cargo xtask restore /dev/cu.usbserial-XXXX backup/torch
```

## Acknowledgements

- [Anduril](https://github.com/ToyKeeper/anduril), by ToyKeeper, the stock firmware, for the click notation and most of what's known about this hardware.
- [loneoceans](https://loneoceans.com/labs/temp/anduril/), who designed the Lume X1-C, for the hardware reference.
- [pymcuprog](https://github.com/microchip-pic-avr-tools/pymcuprog), which the UPDI programmer's sequences follow.
- [AVRDUDE](https://github.com/avrdudes/avrdude), whose flashing logic was reviewed when writing the UPDI programmer.
- Microchip, for the ATDF in `vendor/` and the datasheets.
