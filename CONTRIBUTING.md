# Contributing

## Checks

CI runs these on every pull request, so run them before you push.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -- --include-ignored
cargo clippy --package kr1k --lib --bins --examples --release --target avr-none -Zbuild-std=core -- -D warnings
cargo xtask hex
```

The second clippy is the only place `src/avr/hw.rs` and `src/avr/rt.rs` get linted. `cargo xtask hex` builds the firmware, checks the linked image (`xtask/src/layout.rs`), runs it on a simulated chip (`xtask/src/sim.rs`) and prints flash and RAM use. The files in `examples/` are never flashed. `link.rs` and `guards.rs` exist so xtask can check `link.x` and run `hw.rs`'s guards. `panics.rs` indexes out of bounds and `multiplies.rs` divides a `u16` by ten, and xtask's tests build both for the chip, failing unless the panic check refuses the first and the linker refuses the second.

One test is ignored, since it takes about twenty seconds, and `--include-ignored` runs it. It breaks copies of the firmware in one way at a time (`xtask/src/mistakes.rs`) and wants the checks `hex` runs to refuse each for its own reason, at the stage it names. A mistake in the library is tried with the copy's own `cargo xtask hex`, since the simulator is built from the library. It takes the tree as it stands to pass `cargo xtask hex`, and only checks that an unbroken copy does when a mistake isn't refused as expected. Run it after touching `xtask/src/sim.rs`, `xtask/src/layout.rs`, `src/avr/hw.rs`, `src/avr/rt.rs` or `src/main.rs`, and only add a mistake once you've seen it refused for the reason you meant.

The simulator finds mistakes and proves nothing. It's one reading of the datasheet, run against code written from the same reading. It doesn't model chip resets, anything analogue or real time, so a firmware change still wants reading against the datasheet before it's flashed.

## Rules

- **No dependencies**, in the firmware or in xtask.
- **Logic goes in the library** (`src/lib.rs` and its modules), which is `no_std`, has no `unsafe` and is tested on the host. `src/main.rs` stays a thin loop.
- **What the torch does is `testing::EXAMPLES`** (`src/testing/examples.rs`). Change the row with the code, and add one for new behaviour.
- **Registers come from `src/regs.rs`**, which `build.rs` generates from `vendor/AVR32DD20.atdf`. A number the ATDF doesn't have goes in by hand, with a comment saying why.
- **`unsafe` lives in `src/avr/`**, and every block gets a `// SAFETY:` comment.
- **Never write fuses**, and only a plain 4.5 V UPDI adapter goes on the torch. README.md has the flashing commands.

## Comments

The code is the source of truth, so a comment has to say something the code can't. Keep these, at a line or two each.

- `// SAFETY:` on an `unsafe` block.
- Where a number or an ordering comes from, such as `// DS40002413B §34.5.2`.
- A trap, where the obvious edit would be wrong and no test says so.
- "Unconfirmed" or "unmeasured" on a guess.

Leave out anything that restates a name or a signature, explains how the code works, narrates a test, records history, or points at a document. A test's name is its description.

## Commits

Single-line [Conventional Commits](https://www.conventionalcommits.org), such as `feat(ui): add a second level on 2C`. PRs merge by rebase only, so every commit on a branch lands on main and has to pass.

## Bumping the nightly

AVR is a tier 3 target and nightlies do break it. Change `rust-toolchain.toml`, run every check above, and compare `cargo xtask hex` before and after. `cargo test` fails if the panic check no longer knows the new nightly's panic symbols, or if a wide multiply has started to link.
