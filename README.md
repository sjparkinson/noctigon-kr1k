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

## Using it

- **Off.** 1C turns it on, at the same level each time, and 1H turns it on at the lowest level. The aux glows dim blue, and goes dark once the cell's under 3.30 V.
- **On.** 1C turns it off, and 1H ramps, each 1H going the other way from the last. Every click acts as it's let go, so quick clicks flash it on and off.
- **Lockout.** 2H locks it from on or off, and 2H again unlocks it to off. Nothing else lights it, and the aux shows bright red for five seconds after a press.
- **Yellow** for three seconds after a press means the cell's flat, and it won't light until the cell's charged. **White** means a fault, and it won't light again until the cell's taken out.

## Acknowledgements

- [Anduril](https://github.com/ToyKeeper/anduril), by ToyKeeper, the stock firmware, for the click notation and most of what's known about this hardware.
- [loneoceans](https://loneoceans.com/labs/temp/anduril/), who designed the Lume X1-C, for the hardware reference.
- [pymcuprog](https://github.com/microchip-pic-avr-tools/pymcuprog), which the UPDI programmer's sequences follow.
- [AVRDUDE](https://github.com/avrdudes/avrdude), whose flashing logic was reviewed when writing the UPDI programmer.
- Microchip, for the ATDF in `vendor/` and the datasheets.
