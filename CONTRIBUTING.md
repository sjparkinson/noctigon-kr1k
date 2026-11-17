# Contributing

## Checks

CI runs these on every pull request, so run them before you push.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo clippy --package kr1k --lib --bins --examples --release --target avr-none -Zbuild-std=core -- -D warnings
cargo xtask hex
```

The first three run on your machine's own target. The second clippy lints the firmware as the AVR sees it, which is the only place `src/avr/hw.rs` and `src/avr/rt.rs` get linted. `cargo xtask hex` builds the real firmware and checks the linked image against what those two files assume. That's the vector table, where startup copies and zeroes memory, and that each protected-register unlock is followed straight away by its write, and that no panic or bounds-check symbol is linked, so nothing can reach the panic handler (the checks are in `xtask/src/layout.rs`). It does the same layout checks on `examples/link.rs`, a throwaway program with a const table, a `.data` static and a `.bss` static, since the firmware has none of those yet. Then it runs both on the simulator, along with `examples/guards.rs`, which drives `src/avr/hw.rs` in ways the firmware never does so its guards get run too. It also builds `examples/first_light.rs`, the first-light image, the first thing to go on the torch, runs the same layout checks on it, and runs it on the simulator through clicks, bounces and a switch held down, holding it to its promises of no interrupt, no sleep, no watchdog and no timer as well as every pin rule the firmware has. It writes that to `target/noctigon-kr1k-first-light.hex`, the code alone with no stamp or CRC. If that passes, it pads the image to 32 KB, puts a 16-byte stamp of the commit (`xtask/src/stamp.rs`) just before the CRC in the last two bytes, and writes the file you'd flash. It prints how much of the 32 KB of flash and 4 KB of RAM the firmware uses, its stack, how many instructions a tick takes, and how many the longest interrupt handler takes against a TCB0 period. CI keeps that hex and the ELF as a build artefact.

## The simulator

`xtask/src/sim.rs` puts a fake AVR32DD20 around a CPU in `xtask/src/avr.rs`, wired like the KR1K's Lume X1-C driver, and runs the real firmware on it, from reset, through every example in docs/ui.md, through thousands of ticks of random clicks, and again with a switch bounce and a TCB0 period landing on every instruction the loop reaches. It also starts once with PC1 already high, checks start-up happens in order (reset flags cleared before the watchdog, flash, fuses and VDDIO2 checked before anything else is set up, the PIT on before the watchdog), runs with every tick as slow as the datasheet allows, stops the 32 kHz oscillator with the light on, runs with VDDIO2 missing from power-on and with it going at the top of the ramp, and puts three faults on the AW2016's bus (no answer, SDA held low, SCL held low). It compares PC1, DAC0, its reference and the three paths with the library's output on every tick, and the AW2016's colour and standby with the library's aux.

It turns the rules a real chip enforces quietly into errors, such as a `wdr` in the closed window, a 16-bit register written high byte first, or the errata's lost write (a store under 64 straight after one at 64 or over, DS80001026D §2.2.3). It holds the firmware to the board's rules too, which have no datasheet section. PC1 only rises with DAC0 at 0, its reference at 1.024 V and the paths low, DAC0 only goes above 0 8 ms after that, a reference or path only changes with DAC0 at 0, and only goes up once it's settled, and TCB0 runs exactly while PC1 is high. A pass that begins with VDDIO2 gone has to end with PC1 clear in PORTC, DAC0 at 0 and the paths low, and VDDIO2 coming back puts PC1 up again if PORTC still has it set. It refuses any register it doesn't model, so the first `cargo xtask hex` after you touch a new one fails until you teach it.

It's a mistake-finder, not proof. It's one reading of the datasheet, run against code written from that same reading, and it's never been compared with a real chip. A pass means the firmware does what the simulator thinks it should. Here's what it leaves out.

- **Chip resets.** A watchdog bite is a failed run, so a real bite, and the boot after one, never runs anywhere. Neither does anything that happens to the pins during a reset.
- **Anything analogue.** The boost converter, the amplifier, how fast PD6 really falls when DAC0 goes to 0, the cell, VDD and VDDIO2. The ADC reads scripted numbers, and the DAC's settling time is the datasheet's figure for a load nobody's measured.
- **The AW2016's light.** A fake AW2016 sits on TWI0's bus (`xtask/src/twi.rs`) and takes the firmware's writes, so the colour, dim or bright, standby and the bus faults are checked. Nothing about how bright an aux LED looks, what current it draws, or this board's pull-ups and rise times is modelled, and the TWI host is the same one reading of chapter 29 as the rest.
- **Clocks and time.** It counts instructions, not cycles. A minimum wait is checked as if every instruction took one clock, which a real one never beats, and a budget as if each took four. The oscillators don't drift, apart from the one run that slows every tick to the limit and the one that stops the 32 kHz oscillator outright, and a `wdr` that's lost in power-down can't happen.
- **The flash itself.** Fuses are scripted, and the CRC runs over the image, so it shows `xtask` and the firmware agree with each other and not that the chip does.
- **The programmer on the pads.** Nothing here can see a slipped probe, or 12 V.

The UPDI programmer has its own, smaller fake chip in `xtask/src/updi.rs`, with the same caveat.

## Breakages

```sh
cargo xtask breakages          # every breakage
cargo xtask breakages <name>   # only the ones named
```

Firmware that's right trips none of the simulator's rules, nor `xtask/src/layout.rs`'s, so a passing `cargo xtask hex` doesn't show they still work. The tests in `xtask/src/sim.rs` trip the ones a test can reach by driving the fake chip. The rest only go off with the real firmware running, and `cargo xtask breakages` is for those.

`xtask/src/breakages.rs` has a table of mistakes, each a line or two of `src/avr/hw.rs`, `src/main.rs`, `src/aw2016.rs` or `examples/first_light.rs` changed. DAC0 left live as PC1 drops, a path added with DAC0 still up, a pull-up on PC1, PD6's input buffer left on, `sts` where the errata wants `out`, TCB0 left running, the tick deadline removed, a 16-bit register written high byte first, a watchdog that's never fed, a handler that changes a register, an array indexed by a reading, a `u16` divided by ten, a TWI wait with no end, the aux left lit as the light comes on, aux off that isn't standby, a failed AW2016 update retried, and so on, one for each kind of alarm. The command copies the source to a temporary directory and runs `cargo xtask hex` there. First on the copy as it is, which has to pass. Then with each mistake in turn, which `hex` has to refuse, at the stage and with the message the table names. A mistake that passes fails the command, and so does one that's refused for some other reason, such as not compiling. The checkout is never edited.

Run it after touching `xtask/src/sim.rs`, `xtask/src/layout.rs`, `src/avr/hw.rs` or `src/avr/rt.rs`, and when bumping the nightly. It takes about a minute on a four-core machine, half of that building the copy's xtask and `core`, which is too long for every pull request, so CI doesn't run it.

Each mistake is found by its exact text, which has to be in the file exactly once. `cargo test` checks that, so a refactor that rewords one of those lines fails there until the table's brought up to date, and can't leave a breakage that changes nothing.

Like the simulator, it finds mistakes and proves nothing. A refusal shows one alarm rings for one mistake. It doesn't show the alarm rings for every mistake of that kind, and a mistake that isn't in the table is as unchecked as it was. Only add a breakage once you've run it and seen it refused for the reason you meant. If `hex` passes a mistake it ought to refuse, that's a gap in the simulator, so write it down and leave it out of the table.

## Layout

```
src/lib.rs        the no_std library: everything testable, nothing hardware
src/button.rs     switch samples in, clicks and holds out
src/ui.rs         the state machine docs/ui.md describes
src/torch.rs      one pass of the firmware loop: switch and ADC in, driver output and aux out
src/ramp.rs       the 150 levels as DAC0 codes, references and paths, and the ceiling on them
src/limits.rs     what the cell and the temperature allow, from the UI's level to the level to run at
src/battery.rs    VDD / 10 to centivolts, and low-voltage protection
src/thermal.rs    thermal regulation
src/temp.rs       the temperature sensor's calibration
src/sensors.rs    which ADC conversion is due on which tick
src/gradual.rs    fading a ceiling a level at a time
src/watchdog.rs   which passes of the loop feed the watchdog, the window that's sized for, and
                  the deadline for a tick that stops while the light's on
src/selftest.rs   what the checks at power-on found
src/testing.rs    a fake ADC, readings and the docs/ui.md script reader, shared by the tests and the simulator
src/regs.rs       register addresses and bit values, generated from the ATDF by build.rs
src/board.rs      the KR1K's pin map, which hw.rs and the simulator both wire up from
src/aw2016.rs     the AW2016's registers, and the writes that show each aux colour, a step a tick
src/main.rs       the firmware loop (a stub on the host)
src/avr/hw.rs     pins, register access and the interrupt handlers (AVR only)
src/avr/rt.rs     vector table and startup (AVR only)
examples/link.rs  a program with one static of each kind and a stub `mod hw`, so xtask can check link.x (never flashed)
examples/guards.rs  the real hw.rs asked for power-down with the light on, so the simulator runs its guard (never flashed)
examples/first_light.rs  the first-light image, the first flash on the torch, with hw.rs's output code, a switch and nothing else
link.x            memory layout for the linker, checked against the ATDF in xtask/src/atdf.rs
xtask/            hex, the simulator, breakages (xtask/src/breakages.rs, firmware broken on purpose to show
                  hex refuses it), simulate, and the UPDI programmer (ping, peek, backup, flash, restore)
docs/             hardware, flashing, bench plan, toolchain, UI spec, roadmap, hazard log
vendor/           Microchip's ATDF for the part (Apache-2.0)
```

Keep logic in the library, where `cargo test` can reach it. `src/main.rs` should stay a loop that moves values between `hw` and the library.

## Registers

There's no PAC, and nothing to add when you need a register. `build.rs` generates a constant for every register, bitfield, named value, memory segment and vector in `vendor/AVR32DD20.atdf`, and `src/regs.rs` includes them, so use the one you want, like `regs::addr::RTC_PITCTRLA`, `regs::RTC_PITEN`, `regs::RTC_PERIOD_CYC512`, `regs::mem::EEPROM_START`, `regs::VECTOR_RTC_PIT`. The names are the ATDF's, and the comment at the top of `src/regs.rs` has the rules. `src/avr/hw.rs` uses the constant, not the number. xtask gets the same constants through `kr1k::regs`. The firmware pays nothing for the ones it doesn't use.

The generator is `generate` in `xtask/src/atdf.rs`, which `build.rs` loads by path, and its tests are in that file. Those also check what generating can't, namely what `src/avr/hw.rs` assumes about the ATDF's layout, and `link.x`. If you need a number that isn't in the ATDF, like `ADC_SAMPLEN_32US`, it goes in `src/regs.rs` by hand, with a comment saying why and a check against whatever the ATDF does say about it. Reset values stay out on purpose, because the ATDF's disagree with the datasheet on `ADC0.CALIB` and the factory `OSCCFG` and `SYSCFG0`.

The ATDF only says so much. It has addresses, widths, masks and named values. Picking the wrong constant still compiles, and write order, timing and which flags set without their interrupt enabled are in the datasheet, so have someone (or the `register-checker` agent in `.claude/agents/`) read those against it. A wrong register does something to the hardware you didn't ask for.

Never write fuses. Setting `SYSCFG0.UPDIPINCFG` to GPIO disables UPDI, and getting it back takes a high-voltage pulse on PF6, which the pads don't reach and no high-voltage programmer ever goes on this torch to deliver ([docs/flashing.md](docs/flashing.md#only-the-plain-adapter-goes-on-this-torch)). So that's the end of the light.

## Unsafe

`unsafe_code` is denied across the workspace. Only `src/avr/hw.rs`, `src/avr/rt.rs`, the `#[unsafe(no_mangle)]` on `main`, and the `no_mangle` items in `examples/link.rs`, `examples/guards.rs` and `examples/first_light.rs` opt out, and every `unsafe` block gets a `// SAFETY:` comment.

## Bumping the nightly

AVR is a tier 3 target. Pick a nightly, change `rust-toolchain.toml`, run every check above, and compare `cargo xtask hex` before and after. Symbol names can change between nightlies, so also run `cargo xtask breakages`, which re-proves the no-panic check still fires. Its `bounds-check` indexes a small array in `src/main.rs`'s loop with a runtime value from `hw::adc_result()`, in a copy, and wants `cargo xtask hex` to fail naming `panic_bounds_check` (`llvm-nm -C` on the ELF shows what's there, if it doesn't). Its `wide-multiply` does the same for a 32-bit multiply, which has to fail to link. Then see whether the workarounds in [docs/toolchain.md](docs/toolchain.md) are still needed.

## Commits

Single-line [Conventional Commits](https://www.conventionalcommits.org), such as `feat(ui): add a second level on 2C`. PRs merge by rebase only, so every commit on a branch lands on main.
