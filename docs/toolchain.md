# Toolchain

How Rust gets onto an AVR32DD20 here.

- rustc's built-in `avr-none` target with `-C target-cpu=avr32dd20`, on the nightly pinned in `rust-toolchain.toml`. Nightly because `core` is rebuilt with `-Zbuild-std=core`, and `asm!` and `extern "avr-interrupt"` on AVR aren't stable.
- Linked by the `rust-lld` rustup ships, against this repo's linker script (`link.x`) and startup code (`src/avr/rt.rs`). No avr-gcc, avr-libc or libgcc.
- Register constants generated from Microchip's ATDF at build time (`build.rs`, `xtask/src/atdf.rs`), with zero crate dependencies.
- The UPDI programmer and the simulator live in `xtask`, so rustup is the only thing to install.

The first `cargo xtask hex` downloads the pinned nightly with `rust-src`, and the few crates `core`'s build needs from crates.io. After that it's offline.

## The memory map

AVR ELF files put data space at 0x800000, so in `link.x` SRAM is 0x807000 and the mapped flash 0x808000. Read-only data is linked at its flash address plus 0x808000 and read with ordinary loads through that window, so there's no `lpm`. All 32 KB of flash fits the window, so nothing needs `FLMAP`.

In SRAM, `link.x` puts `.data` first, at 0x7000, then `.bss`, with the stack growing down from the top.

## No 32-bit multiplication

The chip's multiplier is 8×8, and LLVM lowers a runtime `u32` or `u64` multiply to a call to `__mulsi3`, which only libgcc has. The link then fails with `undefined symbol: __mulsi3`.

Dividing a `u16` by a constant that isn't a power of two does the same, since LLVM turns `x / 10` and `x % 10` into a 32-bit multiply by a magic number. Host tests don't catch it. `cargo xtask hex` does.

What works is 8- and 16-bit multiplication, dividing a `u8` by a constant, division by a runtime value at any width up to 32 bits, and anything in a `const`.

If something needs a 32-bit product, split one operand into bytes so each partial product fits 16 bits, or write a 16 × 16 → 32 shift-and-add helper in the library and test it on the host.

## Looking inside the ELF

rustup's `llvm-tools` component has `llvm-objdump`, `llvm-nm` and `llvm-size` built from rustc's own LLVM. Nothing in the build needs it, and `rust-toolchain.toml` pins it so the tools are there to read an ELF with. If they're missing, `rustup component add llvm-tools` from the repo adds them to the pinned nightly.

```sh
LLVM="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
ELF=target/avr-none/release/kr1k.elf
```

Then, after `cargo xtask hex`, which leaves the ELF behind even when its checks fail.

```sh
"$LLVM/llvm-objdump" -d --no-show-raw-insn "$ELF"                                 # everything, vector table first
"$LLVM/llvm-objdump" -d -S --no-show-raw-insn --disassemble-symbols=main "$ELF"   # one function, with source
"$LLVM/llvm-nm" -S -C --size-sort "$ELF"                                          # what's using the flash
"$LLVM/llvm-objdump" -h "$ELF"                                                    # section addresses
```

`cargo xtask hex` stays the command for flash and RAM budgets. These are for finding out which function grew, or whether a handler saves more registers than it should.
