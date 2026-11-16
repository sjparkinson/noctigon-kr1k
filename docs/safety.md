# Hazard log

One section for every way identified so far that this torch could hurt someone or itself, what covers it, and what evidence there is that the cover works. It's a log of the current state. A section changes in the same commit as the code it describes, and a hazard nobody has thought of isn't in here, so a short log doesn't mean a short list.

This repo started as firmware for a Wurkkos TS26 (ATtiny1616, MP3432 boost driver, PWM into the current control) and is being ported to the KR1K (AVR32DD20, Lume X1-C, a DAC into a current amplifier, three power paths). Nothing has run on a KR1K, nobody has opened this torch, and much of the cover below is TS26 code that hasn't been ported yet. Where a section names code, it's the code as it stands, and the section says what has to change. Read each Status strictly.

- **not yet** means the firmware doesn't do this on the AVR32DD20 yet, or does it only as TS26 code that hasn't been ported.
- **host tests** means the library logic is tested on the host, which is board-independent, but nothing built for the AVR32DD20 has run it, and its constants may still be the TS26's.
- **simulator** means the firmware built for the AVR32DD20 has run it on the fake chip (`cargo xtask hex`). The simulator is one reading of the datasheet, run against code written from that same reading, so a pass finds mistakes and proves nothing. [CONTRIBUTING.md](../CONTRIBUTING.md#the-simulator) lists what it leaves out.
- **bench** means it's been seen working on a real KR1K.
- **measured** means a number from a real KR1K backs the constant.

Nothing is past host tests today. Results from the TS26 and its breakout are in the repo's history, and none of them count here.

## Hazards

### H1. The boost converter on with nothing controlling it

**Cause.** PC1, the boost and amplifier enable, is a tri-stated input from every reset until firmware drives it (DS40002413B §18.3.1), for at least the 64 ms `SUT` the stock fuses ask for. It's on the VDDIO2 supply, and the MVIO pins tri-state whenever that supply is missing (§5.1, §19.3.2.1), so if VDDIO2 sags or isn't what the board assumes, PC1 lets go mid-run. It could also be left high by a stray jump to the reset vector, or raised with the DAC still holding a current.

**Mitigation.** Needed on the AVR32DD20, and not done yet. The first thing `main` does is clear PC1's `OUT` and then set its `DIR`, as `hw::hold_enable_low` does for the TS26's PB2 today. The DAC's data is zero and every path pin is low whenever PC1 rises. `hw::halt` (also the panic handler) drops PC1, zeroes the DAC and drops the paths. PC1 never gets a pull-up. The errata's lost-write problem (DS80001026D §2.2.3) means a VPORT write straight after a store to a higher address needs a `nop` or an `out`, or the write that drops PC1 can vanish.

**Evidence.** None on this chip. The TS26 code and its simulator tests cover PB2, a different pin on a different chip.

**Status.** Not yet.

**What's left.** Nobody knows what holds the enable low on this board through reset and `SUT`, or whether anything does. The stock firmware's comment says VDDIO2 "isn't connected", while the datasheet puts PC1 on VDDIO2, and both can't be the whole story ([hardware.md](hardware.md#the-boost-enable-pc1)). Until that's measured, how firmly PC1 holds anything is unknown. The stock firmware raises PC1 with the DAC at zero, waits 8 ms, then sets the current, which is the opposite order to the TS26 rule, and nobody has said which is safer here. The DAC can sink only 0.001 mA (DS40002413B table 38-23), so "DAC at zero" is only as low as the board pulls PD6.

### H2. A current setpoint that's wrong or left behind

**Cause.** The LED current comes from DAC0 on PD6 into an amplifier, not from TCA0's PWM and delta-sigma as on the TS26. A DAC value left non-zero when the light goes off, a reference switched to 2.5 V when 1.024 V was meant (about 2.4 times the current for the same data), a value written with only the low byte (the output only changes when the high byte lands, §34.5.2), or PD6's input buffer left on.

**Mitigation.** Needed and not done yet. Turning off zeroes the DAC data and sets the reference back to 1.024 V before anything else that matters. The ramp has a ceiling well below full output (`ramp::CEILING` holds the TS26 at level 120 of 150), and this firmware's curve is its own. PD6's input buffer is off, as §34.3.1 asks.

**Evidence.** None. `src/ramp.rs` still describes PWM and delta-sigma.

**Status.** Not yet.

**What's left.** Nobody knows what current any DAC value gives on this board, on any path, at either reference. The internal references are ±4 % and only specified with VDD at 2.5 V or 2.7 V and up (table 38-18), and VDD is the cell. What the amplifier does with PD6 floating in power-down, where the DAC is off (§34.3.3), is unknown.

### H3. The power paths

**Cause.** Three path pins, PA7, PA6 and PA5, pick sense resistors, and the stock firmware raises more of them for more current. A path pin left high, a high path raised with a DAC value meant for a low one, or any path high while PC1 is off, with effects nobody knows.

**Mitigation.** Needed and not done yet. Every path is low whenever the light's off, at reset (they're tri-stated, so that depends on the board) and in `hw::halt`. A level sets its path and its DAC value together, and the change between paths goes through zero current, or through an order someone has reasoned through against the datasheet.

**Evidence.** None.

**Status.** Not yet.

**What's left.** How the paths combine, whether they're FETs at all, and what a path does with the boost off are all unknown ([hardware.md](hardware.md#the-power-paths)). The first-light image should use one path and one low DAC value.

### H4. A hang with the light on

**Cause.** The loop stops, so PC1 stays high and the DAC keeps its value.

**Mitigation.** `hw::start_watchdog` sets a 32-clock window and `watchdog::Feeder` says which passes feed it. A tick that stops while the light's on has its own deadline, `watchdog::TICK_DEADLINE_CYCLES`, after which `main` calls `hw::halt`. The next boot after a bite is a fault and won't light (`selftest::hung`). All of it is TS26 code. The deadline counts TCA0's PWM cycles, which the KR1K doesn't have, so it needs another clock to count, and `watchdog::CLOCK_HZ` is the TS26's 10 MHz, not the 12 MHz the stock firmware runs the AVR32DD20 at.

**Evidence.** Host tests in src/watchdog.rs for the feeding pattern and the window.

**Status.** Host tests, for the logic only.

**What's left.** Porting the deadline to a clock this board has, then the simulator, then a bench test. Since a bite is a fault, a spurious one leaves the torch dark until a cell swap.

### H5. Over-discharge of an unprotected 21700 at high current

**Cause.** The cell is unprotected, and the listing asks for one rated at 15 A or more. A 6 V, 6 A boost output from a single cell pulls well over 10 A from it near turbo (arithmetic before losses, so likely), more as the cell falls.

**Mitigation.** `battery::Protection` steps the ceiling down below `battery::STEP_DOWN_BELOW` (3.00 V) and fades it down and off below `battery::OFF_BELOW` (2.90 V). Turning off for a flat cell latches until a reading with the light off is `battery::CHARGED_AT` (3.60 V) or more. A cell it can't read is a fault (H15). The thresholds are board-independent, but the reading isn't. `battery::SLOPE` is the TS26's divider calibration (1094). The KR1K reads VDD itself through the ADC's VDDDIV10 input against 1.024 V (DS40002413B §33.5.7), with the stock firmware's slope of 1014, and nothing in this repo does that yet.

**Evidence.** Host tests in src/battery.rs, src/limits.rs and src/torch.rs, and the docs/ui.md rows with a cell voltage.

**Status.** Host tests, for the logic only.

**What's left.** Porting the reading. Every threshold is unmeasured on this torch. The ramp's ceiling stays low until someone knows what the top of it draws.

### H6. The MCU browning out with the cell

**Cause.** VDD is the cell, through a reverse-polarity FET and no regulator ([hardware.md](hardware.md#power-and-the-battery-reading)). A cell sagging under load pulls the MCU's supply down with it. Below the BOD level (1.8 V to 2.85 V, table 38-16) the chip resets, and with the BOD off it can run below its safe range until POR at about 1.6 V (table 38-4). A reset tri-states PC1 (H1). A reset loop on a sagging cell could pulse the light, and code running out of its voltage range might do anything.

**Mitigation.** None in firmware yet. The stock ELF asks for the BOD on continuously in active mode, which this firmware would read and check, never write. Low-voltage protection (H5) is meant to stop the cell getting that low with the light on.

**Evidence.** None.

**Status.** Not yet.

**What's left.** Nobody knows the board's fuses (a backup's `fuses.hex` says), how far the cell sags at what level, or what the boost converter does as VDD and its input fall together.

### H7. Overheating

**Cause.** A lit light in still air, or a head that's covered.

**Mitigation.** `thermal::Thermal` steps the ceiling down towards `thermal::FLOOR` as the temperature passes `thermal::LIMIT` (50 °C at the sensor). `ramp::CEILING` holds the light well below full output until that's been checked on a real head. The sensor's calibration on this chip comes from `SIGROW.TEMPSENSE0` and `TEMPSENSE1`, read against the 2.048 V reference, and the port has to do that.

**Evidence.** Host tests in src/thermal.rs and src/limits.rs, and docs/ui.md's rows with a temperature.

**Status.** Host tests, for the logic only.

**What's left.** Porting the reading. Every constant is unmeasured on this head, and the levels the floor and ceiling name don't mean the same current on a DAC curve as they did on the TS26.

### H8. Lit in a pocket or a bag

**Cause.** A press on the tail switch under pressure.

**Mitigation.** The 3C lockout (docs/ui.md). In lockout `ui::Ui` never lights, and answers only to 3C. A hold from off does nothing either.

**Evidence.** Host tests in src/ui.rs and src/torch.rs, and the Lockout rows of docs/ui.md.

**Status.** Host tests.

**What's left.** Nothing limits how long a lit light stays lit. Lockout lives in RAM, so a cell swap drops back to off, unlocked. Nobody has tried this tail switch's feel. The aux that shows lockout on the TS26 is the AW2016 here, which this firmware doesn't drive yet (H13), so a locked torch shows nothing.

### H9. Corrupt or half-written flash

**Cause.** An interrupted or bad flash, or a bit that's flipped.

**Mitigation.** `hw::flash_ok` runs CRCSCAN over all of flash at power-on, and a failure is a fault. The AVR32DD20 has CRCSCAN (DS40002413B chapter 30) over 32 KB, where the TS26 had 16 KB, so the image size, the CRC's place and `xtask/src/crc.rs` all need porting. `flash` refuses a full image whose CRC doesn't check.

**Evidence.** Host tests in xtask/src/crc.rs, for the TS26's image size.

**Status.** Not yet.

**What's left.** Porting, then the simulator. The scan has never met a real AVR32DD20.

### H10. Wrong fuses

**Cause.** A torch whose fuses the firmware's timing or its own protection don't expect.

**Mitigation.** `selftest::Fuses::ok` checks fuse fields, with `hw::fuses` only ever reading them. Nothing in this repo writes a fuse. The fields are the tinyAVR's. The AVR32DD20's are `WDTCFG`, `BODCFG`, `OSCCFG.CLKSEL`, `SYSCFG0` (with `UPDIPINCFG`), `SYSCFG1` (`MVSYSCFG` and `SUT`), `CODESIZE` and `BOOTSIZE` (DS40002413B §8.8), so the check needs rewriting against them and against a real board's bytes.

**Evidence.** None for this chip.

**Status.** Not yet.

**What's left.** Nobody has read a KR1K's fuses. A bad fuse is the one permanent thing in this log, which is why nothing here writes one.

### H11. Losing UPDI or the chip while programming

**Cause.** A `SYSCFG0.UPDIPINCFG` write to GPIO, a high voltage on the UPDI pad, a slipped probe, or a wrong pad order.

**Mitigation.** The programmer never writes fuses, the user row or a locked chip, and refuses a chip with the wrong signature, which for this chip is `1E 95 3A`. No high-voltage programmer ever goes on this torch, recovery included. On the AVR32DD20 that's doubly so. The UPDI pin, PF7, is rated to VDD + 0.3 V (table 38-1), and the high-voltage override goes on PF6, which the three-pad layout doesn't bring out, at 7.5 to 8.5 V against a 9 V maximum (§36.3.2.1.2, table 38-29). The first flash is a first-light image built for the AVR32DD20, which doesn't exist yet, and the full firmware goes on only after that's proven on the light. The TS26's first-light image in the history is for the ATtiny1616 and never goes on this torch. `restore` refuses a backup from another chip.

**Evidence.** Host tests in xtask/src/updi.rs and xtask/src/main.rs, against a fake ATtiny1616.

**Status.** Not yet.

**What's left.** Porting the programmer to the AVR Dx NVM controller (no page buffer, 24-bit addresses). Finding the pads, their order and pitch, and naming each with a meter ([flashing.md](flashing.md#the-pads)). No test shows that no command path writes a fuse. It's true by reading the code.

### H12. The programmer feeding the cell rail

**Cause.** The supply pad is VDD, and VDD is the cell. With the cell in, the programmer's 4.5 V and the cell fight over one rail. With the cell out, the programmer powers the MCU and very likely the boost converter's input and the AW2016 (reasoning, unmeasured). The stock firmware starts as soon as the probe lands and might light the emitter. In programming mode the CPU isn't running, so PC1 floats for the whole command (H1).

**Mitigation.** Procedure only. Cell out before anything touches the pads, the switch left alone, the first contact made in the dark while watching the emitter, and a stop on any light that stays on or an adapter that warms ([flashing.md](flashing.md#the-pads), [breakout.md](breakout.md)).

**Evidence.** None.

**Status.** Not yet.

**What's left.** Whether `+` reaches the boost converter's input, how much a USB adapter can push into it, and what holds PC1 in programming mode, are all unknown.

### H13. The AW2016 drawing current in storage

**Cause.** The AW2016 sits on the cell rail (likely), so it draws its standby current whatever the MCU does. A cell left in a drawer drains through it, and through anything else on the rail, with nothing in firmware to stop it.

**Mitigation.** None. This firmware never addresses the AW2016, so it stays as it powers up, with its outputs off.

**Evidence.** A reviewer measured 30 µA off on a pre-production KR1K with stock firmware ([TG Reviews](https://tgreviews.com/2026/06/05/noctigon-kr1k/)). That's the whole torch, and one sample.

**Status.** Not yet.

**What's left.** Measuring off current on this torch, stock first (from a backup) and then with this firmware. Whether stock leaves the AW2016 in a lower-power state than it powers up in is unknown.

### H14. A stray interrupt

**Cause.** An interrupt that something enables by mistake, or whose flag was already up, with a vector that does nothing.

**Mitigation.** `__unhandled_interrupt` in src/avr/rt.rs jumps into `hw::halt`, and `cargo xtask hex` checks the vector table (`vectors` in xtask/src/layout.rs). The AVR32DD20 has 36 vectors, with the PIT at 6, so the table and its check need porting.

**Evidence.** Host tests in xtask/src/layout.rs, for the TS26's vector table.

**Status.** Not yet.

**What's left.** Porting. A pending interrupt that can't be taken, and whether it wakes `halt`'s sleep, is as unknown here as it was on the TS26.

### H15. A dead or garbage ADC

**Cause.** A broken ADC, a reading from a rest, or, here, a VDD reading taken while the boost converter pulls the cell down.

**Mitigation.** `battery::plausible` (1.00 to 4.40 V) and `thermal::plausible` reject readings that can't be real, and a run of garbage voltages is a fault. The logic is board-independent. The reading isn't ported (H5).

**Evidence.** Host tests in src/battery.rs, src/thermal.rs, src/limits.rs and src/torch.rs.

**Status.** Host tests, for the logic only.

**What's left.** A reading that's plausible and frozen isn't detected. VDDDIV10 against a ±4 % reference (table 38-18) puts 4.40 V well inside the ADC's range, unlike the TS26's divider, but nobody has compared a reading with a multimeter.

### H16. A panic path in the firmware

**Cause.** An overflow, an index or an unwrap in code that reaches a panic.

**Mitigation.** `hw::halt` is the panic handler, and `cargo xtask hex` refuses an image that links any panic machinery (`no_panics` in xtask/src/layout.rs). That check is about symbol names, not the chip, so it carries over.

**Evidence.** Host tests in xtask/src/layout.rs, and the clippy restriction lints on the library.

**Status.** Host tests.

**What's left.** Re-proving the check on the AVR32DD20 build and on every nightly bump (CONTRIBUTING.md).

### H17. A stuck switch

**Cause.** Dirt, a pressed-in tail cap, or a jammed button.

**Mitigation.** The UI's holds are bounded. From off a hold lights the floor and nothing more, and with the light on a hold ramps up and stops at `ramp::CEILING`, under both ceilings (H5, H7).

**Evidence.** Host tests in src/ui.rs and src/torch.rs.

**Status.** Host tests.

**What's left.** A stuck switch keeps the light on at the level it reached. A tail switch on a 100 kΩ external pull-up (PD4) bounces in ways nobody has measured.

### H18. A wrong clock

**Cause.** A wrong `OSCCFG.CLKSEL`, or a clock set-up that isn't the one the firmware's sums assume.

**Mitigation.** Not ported. The TS26 code sets a 10 MHz clock from a 20 MHz oscillator. The AVR32DD20's internal oscillator is set by `CLKCTRL.OSCHFCTRLA` behind the protected-write sequence, and the stock firmware runs it at 12 MHz. Its `AUTOTUNE` wants an external 32 kHz crystal (§12.3.6), which this board likely doesn't have, so this firmware has no reason to set it.

**Evidence.** None.

**Status.** Not yet.

**What's left.** Porting, and a fuse check for `CLKSEL`.

## What's left

This is true of every hazard, so it's kept out of the sections above.

- **Nothing has met a KR1K.** Every result in the repo's history is from a TS26 or its breakout, on another chip and another driver.
- **Every threshold is unmeasured.** The cell's cut-offs, the temperature limit, the ramp's ceiling, the 3.60 V that clears a flat cell and the deadlines are from the stock firmware, the datasheet or the project's own judgement.
- **The simulator's limits** are in [CONTRIBUTING.md](../CONTRIBUTING.md#the-simulator). It has no chip resets and models nothing analogue, so the driver, the cell and the head are all scripted, and until it's ported it models the wrong chip.
- **There's no run-time limit.** A lit light with a good cell and a cool head stays lit, whatever pressed the switch.
- **[breakout.md](breakout.md) is what would move hazards to bench.** Stage 3's first-light run is the gate for H1, H2, H3, H11 and H12. Until a hazard has a result there, it stays where it is.
