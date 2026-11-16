# Roadmap

Roughly in order. Each step should leave a torch that's no worse than the step before. Tick things off here in the commit that does them. [breakout.md](breakout.md) gathers every check on a real light into one run.

This repo started as TS26 firmware and is being ported to the KR1K, so the TS26's ticks are gone. A box is ticked only for work done for the KR1K and its AVR32DD20, or work that doesn't depend on the board at all (the toolchain, the library's logic). Nothing from this repo has run on a KR1K yet.

## 1. It lights up

- [x] Toolchain with nightly `avr-none`, `rust-lld`, own linker script and vector table, no crates. Board-independent, carried over from the TS26
- [x] Register constants generated from the AVR32DD20 ATDF at build time
- [x] `link.x` and the start-up code for the AVR32DD20's memory map (32 KB flash, 4 KB SRAM at 0x7000, flash mapped at 0x8000) and its 36 vectors
- [x] Pin map for the Lume X1-C, from the stock firmware (PC1 enable, PD6 DAC, PA5 to PA7 paths, PD4 switch)
- [x] Switch sampling at 64 Hz on PD4, with its external pull-up and no internal one, and the click/hold decoder
- [x] The UPDI programmer in `xtask` ported to the AVR Dx NVM controller (no page buffer, 24-bit addresses, signature `1E 95 3A`), tested against a fake chip. Its link, key and reads matched avrdude on the real chip on 2026-11-16. Erase and write haven't met a real chip
- [ ] A first-light image for the AVR32DD20, click on and click off at one low level through PC1, the DAC and one path
- [x] Back up the stock firmware and fuses. avrdude 8.3 through the JLHawaii808 kit on 2026-11-16, cell out, nothing written, two flash reads matching, in `backup/stock/`. The pads are `+ - U`, the build is 0284 and the fuses are factory defaults with the BOD off (docs/hardware.md)
- [ ] Read the PCB code, measure the pads' pitch, and check the pads' order with a multimeter
- [ ] Ping, back up and flash the KR1K with `xtask`, with only the plain 4.5 V UPDI kit, and the first-light image first. Ping and back up done on 2026-11-16, matching avrdude's backup. Flash waits for the first-light image
- [ ] Disassemble `backup/stock`, and check what docs/hardware.md takes from the stock firmware's source against what the stock build really writes to PC1, the DAC, VREF and the ADC
- [ ] Find out what VDDIO2 is connected to, since PC1 is on it. It reads present on programmer power (`peek`, 2026-11-16); the rail and its behaviour on the cell are still unknown
- [ ] Check the enable, DAC and path sequencing on the real light. Does it come on cleanly, with no flash at turn-on?
- [ ] Measure what DAC value, path and reference do to output, so the ramp has something to stand on

## 2. It's safe to leave a cell in

A tick here means the firmware does it for the AVR32DD20 and it runs on the simulator. That isn't the torch being safe. [breakout.md](breakout.md) is what would show that.

- [ ] Sleep when off, in power-down, with PC1 low, the DAC at zero and every path low first. The PIT wakes it every tick and the loop samples PD4
- [ ] Test power-down straight after a watchdog feed on a real light (T4 in [breakout.md](breakout.md#test-builds)), then drop the idle on the tick after a feed if a resting torch doesn't reset
- [ ] Battery voltage from VDD through the ADC's VDDDIV10 input against 1.024 V, with the stock firmware's slope of 1014. Then check it against a multimeter
- [ ] Low-voltage protection on that reading. The cell is unprotected, so the firmware's the only thing between it and over-discharge
- [ ] Thermal regulation from the internal temperature sensor, with the AVR32DD20's `SIGROW` calibration
- [ ] A watchdog, so a hang can't leave the boost converter running, with a tick deadline counted off a clock this board has
- [ ] Check flash against its CRC, and read the AVR32DD20's fuses back, at power-on

## Safety evidence

What covers each hazard, and how well, is in [safety.md](safety.md). These are the gaps it names that code can close. Like section 2, a tick means the firmware or the tests do it for the AVR32DD20, and none of it is bench evidence.

- [x] No-panic link check on the AVR32DD20 build
- [ ] Overflow checks in release builds, re-proven on the AVR32DD20 build
- [x] Restriction lints on the library. Overflow, indexing, casts and unwraps are made explicit, and clippy denies the rest
- [x] Hold every ADC reading through low-voltage protection and thermal regulation and check where each ends up, plus a randomised property test of `limits::Limits`
- [x] Fault injection on the AVR32DD20 simulator, a dead ADC and a stuck switch
- [x] Unhandled interrupts go to `hw::halt` instead of `reti`, across the AVR32DD20's 36 vectors
- [ ] A commit stamp in flash, just before the CRC, at the end of 32 KB
- [x] The errata's lost write (DS80001026D §2.2.3) checked by the simulator, for every VPORT, GPR and `SLPCTRL.CTRLA` write the runs reach

## 3. It's nice to use

- [ ] A ramp on the DAC, the paths and the two references, on this firmware's own curve
- [ ] A TWI host driver for the AW2016, so the aux can show off, lockout and a flat cell
- ~~Remember the level in RAM~~ Struck. Every click on starts at the same level (docs/ui.md)
- [x] A simpler UI. 1C on and off, 1H ramps each time the other way from the last, 3C lockout that never lights. Library only
- [x] A quicker switch. Clicks count after about 220 ms rather than 280, and 1C off and the third click of 3C lock count as they're let go. Library only
- [x] Aux that can't flatten a cell nobody's reading. Lockout's red is bright, for five seconds after a press, and off's dim blue goes dark a day after the last press. Library only, since nothing drives the aux on this board until the AW2016 driver lands
- [x] The simulator ported to a fake AVR32DD20, wired like the Lume X1-C
- ~~Battery check~~ Struck. No magic flashes, so no 3C voltage blink (docs/ui.md)

## Maybe

- Drive PD6 low as a port pin while off rather than keeping the DAC enabled at zero, which the stock firmware notes might save power. Only once someone knows what the amplifier does with each
