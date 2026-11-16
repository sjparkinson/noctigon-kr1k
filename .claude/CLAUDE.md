# CLAUDE.md

Firmware for the Noctigon KR1K torch with the Lume X1-C boost driver (AVR32DD20, DAC into a current amplifier, three power paths, AW2016 aux), in Rust, with zero crate dependencies, and deliberately small. It started as firmware for the Wurkkos TS26 (ATtiny1616) and is being ported, so some code and history still describe that light. Read README.md for what it is, CONTRIBUTING.md for the checks and layout, and docs/ for the hardware, toolchain and UI spec. Nobody has opened this torch yet.

## Commands

```sh
cargo fmt --check                                          # formatting, as CI runs it
cargo test --workspace --locked -q                         # host tests, run these constantly
cargo clippy --workspace --all-targets --locked -- -D warnings   # host lints
cargo clippy --package kr1k --lib --bins --examples --release --target avr-none -Zbuild-std=core -- -D warnings   # firmware lints: the only place hw.rs and rt.rs get linted
cargo xtask hex                                            # firmware ELF and hex, plus flash/RAM use, after any firmware change
cargo xtask breakages                                      # about a minute: hex refuses each deliberately broken copy, after touching sim.rs, layout.rs or a breakage's lines
```

CONTRIBUTING.md lists every check CI runs. Plain `cargo test` only tests the root package, so always pass `--workspace`.

The firmware clippy and the xtask commands rebuild `core` with `-Zbuild-std`. The first run on a machine fetches crates from crates.io and writes to `~/.cargo` and `~/.rustup`, which the sandbox may block. If it does, rerun outside the sandbox; nothing else in this repo needs that.

## Agents

Agents in `.claude/agents/` report back and never edit. Use them rather than doing the same work inline.

- `register-checker` is for after touching `src/regs.rs`, `src/avr/hw.rs` or `src/avr/rt.rs`.
- `avr-codegen` is for when `cargo xtask hex` fails to link, or flash or RAM grows and you want to know why.
- `hardware-researcher` is for before writing a new fact into docs/hardware.md.
- `microchip-docs` is for when you need what the datasheet, errata or an app note says. It searches the local PDFs in `docs/microchip/`, which are gitignored and only exist in the main checkout.

## Working in this repo

- **Edit `.rs` files with Edit or Write**, not `sed -i`, `cp` or a heredoc. The lint hook only runs on Edit and Write.
- **Parallel agents get their own worktree and their own files.** Never `git stash` to compare before and after; use a WIP commit or a second worktree.
- **The sandbox can't write `.git/config`**, so `git push -u` pushes but can't record the upstream. Push with `git push origin <branch>`, pass `--head <branch>` to `gh pr create`, and don't try to fix the tracking.
- **PRs merge by rebase only**, so every commit on a branch lands on main as written. Keep each one single-line and passing.
- **Docs and comments are in documentation style.** No "I", "I've", "my", "we" or "our". Describe the thing ("the firmware reads", "nobody has measured"), and use "you" only for instructions to the reader. Keep uncertainty as words like "likely" or "unconfirmed" rather than "I think". Quotes stay as they were written.
- **docs/safety.md is the hazard log.** Change the section a commit affects in that commit.

## Rules that bite

- **No dependencies.** Not in the firmware, not in xtask. If something seems to need a crate, write the small part you need, or ask first.
- **Only the plain 4.5 V UPDI kit ever goes on this torch.** No high-voltage programmer (the 12 V HV Friend or any other) ever does, recovery included, so never suggest one. On the AVR32DD20 the UPDI pad is PF7, rated to VDD + 0.3 V, and the high-voltage override goes on PF6, which the three-pad layout doesn't bring out (DS40002413B table 38-1, §36.3.2.1.2). The rule comes from the TS26. An earlier torch was lost on 2026-11-06 after the Friend was hand-held on its pads, and it then measured about 2.5 Ω between all three. 12 V or a slipped probe is the likely cause and isn't proven, so don't write it up as if it were. docs/flashing.md has the rest.
- **The first flash on the KR1K is a first-light image built for the AVR32DD20**, the smallest click on and off through PC1, the DAC and one path. It doesn't exist yet. The full firmware only goes on once that's proven on the light. The TS26's first-light image in the history is for the ATtiny1616 and must never be flashed on this torch. docs/breakout.md has the order.
- **Read programmer output for what it really says.** A pulled-down UPDI line reads "nothing echoed back", not "no reply", so that message on the pads can mean a short rather than a bad adapter. Every CH340 adapter shows as `1a86:7523`, so ask which one's plugged in. `flash` only checks the signature, and a verify doesn't prove the firmware runs. `ping` and `peek` finish with UPDIDIS, which DS40002413B §36.3.2.2.2 says issues a System Reset, so don't call them harmless to a running chip. Nobody has found the KR1K's pads, so measure their order and pitch before trusting a probe.
- **Never write fuses, and never suggest a command that does.** `SYSCFG0.UPDIPINCFG` set to GPIO turns UPDI off, and only a high-voltage pulse on PF6 gets it back, which this torch never gets, so it would be lost. `cargo xtask flash` writes flash only.
- **The UPDI programmer never writes fuses, the user row or a locked chip.** Don't add any of that without the user asking for it explicitly.
- **Every register address and bit mask comes from `src/regs.rs`, which `build.rs` generates from the ATDF** (`vendor/AVR32DD20.atdf`) with `generate` in `xtask/src/atdf.rs`. No bare register numbers in `src/avr/hw.rs` or `src/avr/rt.rs`, and nothing typed by hand that the ATDF names, so use the generated constant. A number the ATDF doesn't have goes in `src/regs.rs` by hand with a comment saying why. Use the `register-checker` agent after touching `src/regs.rs`, `src/avr/hw.rs` or `src/avr/rt.rs`, for what the ATDF can't express.
- **16-bit peripheral registers go low byte first.** Use `write16` in `src/avr/hw.rs`. The DAC's 10 bits sit at the top of `DAC0.DATA`, and its output only changes when the high byte's written.
- **The errata lose a write.** A store to an address of 64 or more followed straight away by a store under 64 (the VPORTs) or to `SLPCTRL.CTRLA` loses the second one (DS80001026D §2.2.3). Put a `nop` between them, or use `out`, and check the generated code.
- **The boost and amplifier enable (PC1) is the dangerous pin.** It's low whenever the light's off, including at reset and in `hw::halt`, which is also the panic handler. Clear its `OUT` before setting its `DIR`. The DAC is at zero and every path pin (PA5, PA6, PA7) is low whenever PC1 rises, and turning off drops PC1, zeroes the DAC and drops the paths. Never give PC1 a pull-up. PC1 is on the VDDIO2 supply, and what VDDIO2 is connected to is an open question (docs/hardware.md), so don't assume it holds.
- **PD6 is the DAC's output and PF7 is UPDI.** `hw::init` turns PD6's digital input buffer off (DS40002413B §34.3.1) and writes nothing else to PF7. Neither gets a pull-up.
- **PD4, the switch, has an external pull-up.** Don't turn the internal one on.
- **VDD is the cell.** There's no regulator in front of the MCU, so it sees every sag the boost converter pulls out of the cell and browns out with it, and the programmer's supply pad feeds the cell rail. The battery reading is VDD itself, through the ADC's VDDDIV10 input.
- **Logic goes in the library** (`src/lib.rs` and its modules), which is `no_std`, has no `unsafe`, and is tested on the host. `src/main.rs` stays a thin loop.
- **docs/ui.md is the UI spec.** Change it in the same commit as the code, and add a test in `src/ui.rs` or `src/torch.rs` for every row you add.
- **Tick off docs/roadmap.md** in the commit that does the work.
- **The stock firmware is GPL-3.0.** Read its source for facts about the hardware (pins, calibration, timings), and don't copy its code or its ramp tables into this MIT repo. Docs and comments call it "the stock firmware" and name none of its files, identifiers or commits. README.md's acknowledgements are the only place that names or links it.
- **No runtime 32- or 64-bit multiplication.** The chip has an 8×8 hardware multiplier, so 8- and 16-bit multiplies compile to `mul` instructions, but there's no libgcc, so anything wider fails to link with `undefined symbol: __mulsi3`. That includes dividing a `u16` by a constant that isn't a power of two (`x / 10`, `x % 10`), which LLVM turns into a 32-bit multiply. Host tests won't catch it; `cargo xtask hex` will. 8-bit maths, 16-bit multiplies, division by a runtime value, and `const fn` maths are fine. docs/toolchain.md has the options.
- **Flash is 32 KB and RAM is 4 KB.** Check `cargo xtask hex` after changes. No `core::fmt` in the firmware (it pulls in kilobytes), and no floats.
- **Mark uncertainty.** docs/hardware.md labels facts as confirmed or likely. Keep doing that; don't promote a guess to a fact, and don't write anything up as seen on this torch until someone has seen it.
- **The simulator finds mistakes and proves nothing.** `cargo xtask hex` runs the firmware on a fake chip (`xtask/src/sim.rs`), but that chip is one reading of the datasheet, run against code written from the same reading, and it's never been compared with a real one. It doesn't model chip resets, so a watchdog bite is a failed run and what follows one never runs. It doesn't model anything analogue either, such as the driver, the cell, or a probe on the pads. A firmware change still needs reading against the datasheet (the `register-checker` agent) before anyone flashes it, and a pass never promotes anything in docs/hardware.md. A register it doesn't model is an error, so teach it what the datasheet says rather than what makes the build pass.

## Things that look wrong but aren't

- `.rodata` is linked at 0x808000 and up, because AVR ELF files put data space at 0x800000, plus the mapped-flash window at 0x8000. It's how const tables are read without `lpm`, and all 32 KB of flash fits the window.
- `src/main.rs` has a host `fn main()` that exits with an error. It's there so the workspace builds and tests on the host.
- `src/main.rs` idles rather than powers down on the tick after a watchdog feed. The datasheet doesn't say a `wdr` finishes syncing with the peripheral clock stopped. It stays until docs/breakout.md's T4 says it can go.
