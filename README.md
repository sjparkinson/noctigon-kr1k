# Noctigon KR1K

Rust firmware for the Noctigon KR1K torch with the Lume X1-C boost driver, an AVR32DD20 setting the LED current through its DAC. It has zero crate dependencies and is deliberately small.

It runs on one KR1K so far, where clicks, the ramp, lockout and the aux LEDs have been seen working. Thermal regulation, the low-voltage steps and standby current haven't been measured there, so it isn't proven firmware. [docs/hardware.md](docs/hardware.md) has what's known about the board and [docs/safety.md](docs/safety.md) what could go wrong.

## Using it

The notation is Anduril's, so 1C is one click, 3C three quick clicks and 1H press and hold.

| From | Action | Does |
|---|---|---|
| Off | 1C | On, at level 52 of 150 |
| Off | 3C | Lockout |
| On | 1C | Off |
| On | 1H | Ramp, up the first time and the other way each time after, between levels 13 and 120 |
| Lockout | 3C | Unlock |

Nothing is remembered between uses. The aux LEDs are dim blue when off, bright red for five seconds after a press in lockout, and dark while the light's on.

A low cell steps the light down and then turns it off, and heat fades it down. A flat cell or a fault keeps it off, with the aux yellow or white for three seconds after a press. `src/testing/examples.rs` has every case, and they run as tests.

## Building

You need [rustup](https://rustup.rs).

```sh
cargo xtask hex
cargo test --workspace
```

See [docs/toolchain.md](docs/toolchain.md) for how it builds and links.

## Flashing

The KR1K's driver has three UPDI pads, labelled `+ - U` on this torch. Pad order differs between lights, so check yours with a meter first. Take the cell out, put a plain 4.5 V serial UPDI programmer on the pads, and `cargo xtask` does the rest. Never a high-voltage programmer, and never write fuses, since either can cost the torch. Back up before the first flash.

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
cargo xtask backup /dev/cu.usbserial-XXXX backup/torch
cargo xtask flash /dev/cu.usbserial-XXXX
cargo xtask restore /dev/cu.usbserial-XXXX backup/torch
```

If the link fails, lift the probe and re-seat it. `flash` only checks the signature and verifies the write, which doesn't prove the firmware runs.

## Acknowledgements

- [Anduril](https://github.com/ToyKeeper/anduril), by ToyKeeper, the stock firmware, for the click notation and most of what's known about this hardware.
- [loneoceans](https://loneoceans.com/labs/temp/anduril/), who designed the Lume X1-C, for the hardware reference.
- [pymcuprog](https://github.com/microchip-pic-avr-tools/pymcuprog), which the UPDI programmer's sequences follow.
- [AVRDUDE](https://github.com/avrdudes/avrdude), whose flashing logic was reviewed when writing the UPDI programmer.
- Microchip, for the ATDF in `vendor/` and the datasheets.
- This firmware started as this project's own firmware for the Wurkkos TS26, and its history is that light's.
