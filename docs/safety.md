# Hazard log

Each way found so far that this torch could hurt someone or itself, what covers it, and what's unproven. A hazard nobody has thought of isn't here. The code is the source of truth, so this only changes when a hazard is added or removed, or when its cover changes in kind.

- **simulator** means it has run on the fake chip, which finds mistakes and proves nothing.
- **seen** means seen working on this torch.
- **measured** means a number from this torch backs it.

## Hazards

### H1. The boost converter on with nothing controlling it

**Hazard.** PC1, the boost and amplifier enable, floats from reset until firmware drives it (DS40002413B §18.3.1) and whenever VDDIO2 is missing (§19.3.2.1).

**Cover.** `main` calls `hw::hold_enable_low` first. `hw::set_output` raises PC1 only with DAC0 at 0 and the paths low, through `out`, which the errata spare (DS80001026D §2.2.3). VDDIO2 missing (`hw::vddio2_ok`) is a fault that puts the light out that tick.

**Unproven.** Seen, as no flash when the cell goes in. The VDDIO2 fault is simulator only. Measured once, VDDIO2 within 0.07 V of VDD on programmer power, so very likely tied, unconfirmed ([hardware.md](hardware.md)). What holds the enable low through reset, and which on and off order is safest, are unknown.

### H2. A current setpoint that's wrong or left behind

**Hazard.** A DAC0 code left behind, the wrong reference or a half-written code.

**Cover.** `hw::set_output` zeroes DAC0 and resets its reference at every turn-off, and `write16` lands a code whole.

**Unproven.** Seen, as a ramp that looked right both ways, though not its top. No current is measured, and the path gains in `ramp` are estimates.

### H3. The power paths

**Hazard.** A path (PA5 to PA7) or the reference changed with the old code in DAC0 could ask for a hundred times the current.

**Cover.** `hw::set_output` takes DAC0 to 0 and waits about 20 µs first. The paths are low whenever the light is off.

**Unproven.** Seen only as the same ramp. The wait is a guess, since how fast PD6 falls is unmeasured (the DAC sinks 0.001 mA, table 38-23).

### H4. A hang with the light on

**Hazard.** The loop stops with PC1 high.

**Cover.** A window watchdog (`hw::start_watchdog`, `watchdog::Pacer`) resets the chip, and that boot is a fault (`selftest::hung`). If the 32 kHz oscillator stops, `hw::wait_for_tick` times out off the main clock and `main` calls `hw::halt`.

**Unproven.** Simulator. A night at rest with no bite was seen on a build older than the 1 Hz rest, and no long rest is recorded on the build that has it. The datasheet gives the 32 kHz oscillator no accuracy, so the margins rest on a guess.

### H5. Over-discharge of an unprotected 21700 at high current

**Hazard.** The cell has no protection of its own, and the boost converter likely pulls over 10 A from it.

**Cover.** `battery::Protection` steps the ceiling down as the cell falls, fades the light off, and keeps it off until the cell reads charged.

**Unproven.** Simulator, with the reading scripted. Every threshold is unmeasured, against a ±4 % reference (table 38-18).

### H6. The MCU browning out with the cell

**Hazard.** VDD is the cell and this torch's BOD is off, so a sagging cell browns the MCU out with no reset while PC1 is high.

**Cover.** A floor that narrows the window and doesn't close it. While lit, two cell readings running under `battery::COLLAPSE_BELOW` put the light out that tick.

**Unproven.** Simulator, with the cell scripted. Nothing covers a brown-out itself. A trip can take 188 ms, or 203 ms as a light starts, and how this cell sags is unmeasured.

### H7. Overheating

**Hazard.** A lit light in still air, or with the head covered.

**Cover.** `thermal::Thermal` steps the ceiling down past `thermal::LIMIT` at the MCU's sensor. Nothing else holds the light under the stock firmware's top.

**Unproven.** Simulator, with the reading scripted. What the MCU reads when the head is hot is unknown.

### H8. Lit in a pocket or a bag

**Hazard.** A press on the switch under pressure.

**Cover.** 3C locks and unlocks. In lockout `ui::Ui` never lights, and four presses are a 4C, which does nothing.

**Unproven.** Seen, for locking and unlocking. Unlocked, a click lights it, and so does a press held past `button::HOLD_TICKS`, which lights `ui::FLOOR`. A cell swap unlocks it, and nothing limits how long a lit light stays lit.

### H9. Corrupt or half-written flash

**Hazard.** An interrupted or bad flash, or a flipped bit.

**Cover.** `hw::flash_ok` runs CRCSCAN over all of flash at power-on, and a failure is a fault.

**Unproven.** Simulator. The torch lighting only shows the scan didn't fail, and `STATUS.OK` reads 1 before any scan runs.

### H10. Wrong fuses

**Hazard.** Fuses the firmware's timing or its own protection don't expect.

**Cover.** `selftest::Fuses::ok` checks the fields the firmware relies on, and a failure is a fault. Nothing in this repo writes a fuse.

**Unproven.** Simulator, with this torch's bytes, which are factory defaults and pass. Whether other KR1Ks match is unknown.

### H11. Losing UPDI or the chip while programming

**Hazard.** A fuse write that turns UPDI off, a high voltage on the pad, a slipped probe or a wrong pad order.

**Cover.** `xtask/src/updi.rs` never writes fuses, the user row or a locked chip, and refuses the wrong signature. Only the plain 4.5 V kit goes on the pads, never a high-voltage programmer, recovery included (PF7 is rated to VDD + 0.3 V, table 38-1).

**Unproven.** Seen, for `ping`, `backup` and `flash`. `restore` has never run on this torch, and no test shows that no command writes a fuse.

### H12. The programmer feeding the cell rail

**Hazard.** The programmer's supply pad is the cell rail. It fights a cell that's in and very likely feeds the boost converter, with PC1 floating in programming mode.

**Cover.** Procedure only. The cell comes out before the probe goes on.

**Unproven.** Seen only as sessions with the cell out that left a working torch. What the emitter and the adapter did isn't recorded.

### H13. The AW2016 drawing current in storage

**Hazard.** The AW2016 is likely on the cell rail, so it and the aux drain a stored cell.

**Cover.** `aw2016::Aw2016` puts the chip in standby whenever the aux are off, and `torch::Torch` puts out off's dim blue under `torch::AUX_DARK_BELOW`.

**Unproven.** Seen, as dim blue when off, bright red in lockout and dark while lit. Tail current is unmeasured, and the low-cell cut-off hasn't been seen to act.

### H14. A stray interrupt

**Hazard.** An interrupt enabled by mistake, or with its flag already up, and no handler.

**Cover.** Every unused vector goes to `__unhandled_interrupt` in `src/avr/rt.rs` and on into `hw::halt`, which `cargo xtask hex` checks.

**Unproven.** Simulator. Whether a pending interrupt that can't be taken wakes `halt`'s sleep is unknown.

### H15. A dead or garbage ADC

**Hazard.** A broken ADC, or one stuck on a believable reading, hides a flat cell or a hot head.

**Cover.** `battery::plausible` and `thermal::plausible` reject readings that can't be real, and `sensors::Schedule` reads the ADC's GND input to catch one that's stuck. A run of bad voltages or ground readings is a fault.

**Unproven.** Simulator. A reading that's wrong but plausible and still moving isn't caught, nor is a drifted reference. A stuck ADC at rest is only found at a press, which lights for about a quarter of a second.

### H16. A panic path in the firmware

**Hazard.** An overflow, an index or an unwrap reaches a panic.

**Cover.** `hw::halt` is the panic handler, and `cargo xtask hex` refuses an image with panic machinery linked in (`xtask/src/layout.rs`).

**Unproven.** Simulator at most. The check goes by symbol names, so `cargo test` builds `examples/panics.rs` for the chip and fails if the check stops seeing its panic.

### H17. A stuck switch

**Hazard.** Dirt, a pressed-in tail cap or a jammed button.

**Cover.** `ui::Ui` bounds every hold. Locked, a hold does nothing. Off, 1H lights `ui::FLOOR` and the rest of that hold moves nothing. Lit, 1H stops at `ramp::CEILING` or `ui::FLOOR`.

**Unproven.** Seen, for ordinary holds while lit. 1H from off and a stuck switch are simulator only. A stuck switch lights the floor from off or from power-on and stays there, does nothing if it sticks within `button::RELEASE_TICKS` of a click that acted, can leave a lit light at the level it reached, and keeps the tick at 64 Hz. What the floor draws is unmeasured.

### H18. A wrong clock

**Hazard.** A wrong `OSCCFG.CLKSEL`, or a clock the firmware's sums don't assume.

**Cover.** `hw::init_pins` sets the internal oscillator to 4 MHz with no prescaler (`watchdog::CLOCK_HZ`), and `selftest::Fuses::ok` checks `CLKSEL`.

**Unproven.** Simulator, which only shows the firmware asks for the clock it models.

### H19. A stuck aux bus

**Hazard.** TWI0's bus to the AW2016 fails, and a wait with no end would hang the loop with the light on.

**Cover.** `hw::set_aux` counts every wait against one budget and turns the host off before it returns. A failed update isn't retried (`aw2016::Aw2016::failed`).

**Unproven.** Seen with a working bus only. A dead bus is simulator only, and this board's pull-ups are unmeasured.

### H20. A press read as more than one

**Hazard.** A switch that chatters, or that actuates twice on one press. Lit, a 1C then reads as off and on again, at `ui::ON_LEVEL` whatever the level was.

**Cover.** `button::Debounce` counts a change of the switch once two samples in a row show it. After a click that acts as it's let go, `button::Clicks` ignores the switch until it has been up for `button::RELEASE_TICKS`.

**Unproven.** Simulator only. Seen without the cover, at `ui::FLOOR` on 2026-11-18, and not reproduced since, so which of the two it was is unknown. How long this switch bounces is unmeasured, and chatter longer than a sample can still end a hold or split a click from off into two. The first two samples after a switch wake can be less than a tick apart, so a short glitch at rest can still count as a press.

## What's left

- Nobody knows what holds PC1 low through reset and in programming mode (H1, H12).
- No current, temperature or tail current is measured, and the top of the ramp hasn't been seen (H2, H3, H7, H13).
- No low-voltage step, cut-off, collapse or brown-out has been seen (H5, H6).
- No fault, watchdog reset or tick deadline has been seen on the chip (H4, H9, H14, H15).
- `restore` has never run on this torch (H11).
- Nothing limits how long a lit light stays lit (H8, H17).
- The switch's bounce is unmeasured, and the misread press hasn't been reproduced (H20).
