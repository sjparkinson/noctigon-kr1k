# Hazard log

One section for every way identified so far that this torch could hurt someone or itself, what covers it, and what evidence there is that the cover works. It's a log of the current state. A section changes in the same commit as the code it describes, and a hazard nobody has thought of isn't in here, so a short log doesn't mean a short list.

This repo started as firmware for a Wurkkos TS26 (ATtiny1616, MP3432 boost driver, PWM into the current control) and is being ported to the KR1K (AVR32DD20, Lume X1-C, a DAC into a current amplifier, three power paths). Nothing from this repo has run on a KR1K. The only contact with this torch so far is one avrdude read of its stock firmware and fuses through the pads, cell out, nothing written (2026-11-16, [hardware.md](hardware.md)). The firmware, programmer and simulator have been ported to the AVR32DD20, and none of that code has run on a real chip. Where a section names code, it's the code as it stands. Read each Status strictly.

- **not yet** means nothing in the firmware or the tools covers this on the AVR32DD20 yet, or only procedure does.
- **code only** means the firmware for the AVR32DD20 does it and builds, and nothing has run it, not even the simulator.
- **host tests** means the library logic is tested on the host, which is board-independent, but nothing built for the AVR32DD20 has run it, and its constants may still be the TS26's.
- **simulator** means the firmware built for the AVR32DD20 has run it on the fake chip (`cargo xtask hex`). The simulator is one reading of the datasheet, run against code written from that same reading, so a pass finds mistakes and proves nothing. [CONTRIBUTING.md](../CONTRIBUTING.md#the-simulator) lists what it leaves out.
- **bench** means it's been seen working on a real KR1K.
- **measured** means a number from a real KR1K backs the constant.

Nothing is past the simulator today. Results from the TS26 and its breakout are in the repo's history, and none of them count here.

## Hazards

### H1. The boost converter on with nothing controlling it

**Cause.** PC1, the boost and amplifier enable, is a tri-stated input from every reset until firmware drives it (DS40002413B §18.3.1), for at least the `SUT` delay (0 ms on this torch's factory fuses, 64 ms in the stock source's). It's on the VDDIO2 supply, and the MVIO pins tri-state whenever that supply is missing (§5.1, §19.3.2.1), so if VDDIO2 sags or isn't what the board assumes, PC1 lets go mid-run. It could also be left high by a stray jump to the reset vector, or raised with the DAC still holding a current.

**Mitigation.** The first thing `main` does is `hw::hold_enable_low`, which clears PC1's `OUT` and then sets its `DIR`. `hw::set_output` only ever raises PC1 with DAC0 at 0 against 1.024 V and every path low, whatever it's asked for, and `torch::Torch` sends `ramp::Output::STARTING` (PC1 high, DAC 0, no path) for the first tick of every light, so the level comes 15.6 ms later. That's the stock firmware's order, PC1 then 8 ms then the current, done as a tick instead of a wait. Turning off, and `hw::halt` (also the panic handler and where an unhandled interrupt goes), follow the stock firmware's off order, DAC0 to 0, its reference to 1.024 V, PC1 low, then the paths low. That order is chosen because it's the only one known to have run on this board, not because anyone has shown it's the safest. PC1 never gets a pull-up, and `hw::init` writes its `PINCTRL` to 0. Every write to a VPORT goes through `out`, so the errata's lost write (DS80001026D §2.2.3) can't drop the write that lowers PC1. `main` reads `MVIO.STATUS.VDDIO2S` at power-on (§19.5.3), and VDDIO2 missing is a fault (`selftest::Faults::supply`), so a torch whose PC1 can't be driven never asks for light. It's only checked at power-on.

**Evidence.** Host tests `the_driver_starts_with_the_dac_at_zero_for_a_tick_every_time` and `random_switching_and_readings_never_break_a_promise` (which checks every lit stretch starts with `STARTING`) in src/torch.rs, and `off_and_starting_ask_for_nothing` in src/ramp.rs. On the simulator (xtask/src/sim.rs), `found_lit` starts with PC1 already high, `vddio2_missing` runs with VDDIO2 gone, and every rise of PC1 is held to DAC0 at 0, its reference at 1.024 V and the paths low. The `dac-live-before-pc1`, `dir-before-out`, `pc1-pull-up` and `vddio2-ignored` breakages are refused. Nothing has run `hw.rs` on a real chip.

**Status.** Simulator. It starts once with PC1 already high, and once with VDDIO2 missing, and holds every rise of PC1 to DAC0 at 0, its reference at 1.024 V and the paths low.

**What's left.** Nobody knows what holds the enable low on this board through reset and `SUT`, or whether anything does. The stock firmware's comment says VDDIO2 "isn't connected", while the datasheet puts PC1 on VDDIO2, and both can't be the whole story ([hardware.md](hardware.md#the-boost-enable-pc1)). Until that's measured, how firmly PC1 holds anything is unknown. The firmware follows the stock firmware's order, PC1 with the DAC at zero, then the current a tick later, which is the opposite of the TS26 rule, and nobody has said which is safer here. VDDIO2 sagging mid-run, after the power-on check, isn't watched. The DAC can sink only 0.001 mA (DS40002413B table 38-23), so "DAC at zero" is only as low as the board pulls PD6.

### H2. A current setpoint that's wrong or left behind

**Cause.** The LED current comes from DAC0 on PD6 into an amplifier, not from TCA0's PWM and delta-sigma as on the TS26. A DAC value left non-zero when the light goes off, a reference switched to 2.5 V when 1.024 V was meant (about 2.4 times the current for the same data), a value written with only the low byte (the output only changes when the high byte lands, §34.5.2), or PD6's input buffer left on.

**Mitigation.** Turning off zeroes the DAC data and sets the reference back to 1.024 V, before PC1 drops (H1). `hw::write16` writes the DAC's low byte then its high byte, so a code lands whole. A change of reference or path goes through a DAC code of 0 (H3). The ramp is this firmware's own cubic, from the moon path at code 100 against 1.024 V up to the stock firmware's top on this torch's 32 W build (high path, code 716 against 2.5 V, 70 % of full scale), so nothing in the table can ask for more than stock does. `ramp::CEILING` holds the light at level 120 of 150, about half of that top. PD6's input buffer is off, as §34.3.1 asks.

**Evidence.** Host tests in src/ramp.rs, among them `every_level_asks_for_more_current_than_the_last`, `the_ceiling_is_about_half_the_stock_firmwares_top` and `the_top_is_the_stock_firmwares_and_under_full_scale`. They test a model of the current, DAC code times reference times an estimated path gain. The simulator checks DAC0, its reference and the paths against the library's output on every tick, and refuses DAC0 above 0 within 8 ms of PC1 rising and PC1 dropping with DAC0 live (the `dac-before-the-boost-starts` and `pc1-down-with-dac-live` breakages).

**Status.** Simulator, for the order of every change and that the driver shows the library's output on every tick. Nothing about what current comes out.

**What's left.** Nobody knows what current any DAC value gives on this board, on any path, at either reference. The path gains (`ramp::LOW_GAIN`, `ramp::HIGH_GAIN`) are estimates from where the stock firmware changes path, and the levels just above a path change sit at DAC codes under 0x030, where the DAC's linearity isn't specified and its ±5-code offset is a big share (table 38-23). The internal references are ±4 % and only specified with VDD at 2.5 V or 2.7 V and up (table 38-18), and VDD is the cell. What the amplifier does with PD6 floating in power-down, where the DAC is off (§34.3.3), is unknown.

### H3. The power paths

**Cause.** Three path pins, PA7, PA6 and PA5, pick sense resistors, and the stock firmware raises more of them for more current. A path pin left high, a high path raised with a DAC value meant for a low one, or any path high while PC1 is off, with effects nobody knows.

**Mitigation.** Every path is low whenever the light's off, in `hw::halt`, and from `hw::init` on (at reset they're tri-stated, so before that it depends on the board). When a level change moves the path or the reference, `hw::set_output` writes DAC0 to 0 first, then the reference, waits 3 µs for it to settle (2 µs in table 38-18), then the paths, then the new code. It waits about 20 µs after the DAC goes to 0 before touching the reference or the paths, against 7 µs typical settling (table 38-23). A new path, or a higher reference, with the old code still in the DAC could ask for a hundred times the current for a moment, and this order is meant never to ask for more than the old or the new level. That rests on PD6 really reaching 0 in the wait, and the DAC can only sink 0.001 mA, so how fast it falls depends on the board, which nobody's measured. A change within one path and reference only writes the code. PA0, the FET gate that isn't fitted, is an input with a pull-up and is never driven.

**Evidence.** Reading of `hw::set_output`, and the simulator, which refuses any change of reference or path with PC1 high and DAC0 live, and a raised reference or added path within 10 µs of DAC0 reaching 0 (the `path-change-with-dac-live`, `paths-off-before-dac` and `dac-fall-not-waited` breakages). Nothing has run it on a real chip.

**Status.** Simulator, for the order. With PC1 high, a path or the reference only changes with DAC0 at 0, and only goes up once DAC0 has had 10 µs to get there, table 38-23's settling time, which is a floor and not this board's.

**What's left.** How the paths combine, whether they're FETs at all, and what a path does with the boost off are all unknown ([hardware.md](hardware.md#the-power-paths)). The first-light image uses the low paths (PA7 and PA6 together, as every level in the low range does) at one low DAC value, code 92 against 1.024 V, and only changes them with DAC0 at 0, from none to those two as it turns on and back as it turns off, through the same `hw.rs` output code as the firmware. It has only run on the simulator. The light goes dark for the few microseconds of each change of path, which nobody's seen, and what the amplifier and boost do across that gap is unknown.

### H4. A hang with the light on

**Cause.** The loop stops, so PC1 stays high and the DAC keeps its value.

**Mitigation.** `hw::start_watchdog` sets a 32-clock window and `watchdog::Feeder` says which passes feed it. A tick that stops while the light's on has its own deadline. TCB0 runs off the 4 MHz main clock in periodic interrupt mode while PC1 is high, flagging every 1 ms (`watchdog::TIMER_PERIOD_CLOCKS`), and `hw::wait_for_tick` counts those flags, not wake-ups, so a bouncing switch can't use it up. After `watchdog::TICK_DEADLINE_PERIODS` (104) with no tick, 103 to 104 ms, `main` calls `hw::halt`, which turns the light off and powers down for good. The next boot after a bite is a fault and won't light (`selftest::hung`).

**Evidence.** Host tests in src/watchdog.rs for the feeding pattern, the window and the deadline's sums. On the simulator, `oscillator_stops` and `slow_ticks` in xtask/src/sim.rs, and the `no-tick-deadline`, `watchdog-never-fed` and `watchdog-fed-every-tick` breakages.

**Status.** Simulator. It stops the 32 kHz oscillator with the light on, and PC1 has to drop within the deadline and the chip stop in power-down. It also runs every tick as slow as the oscillators' limits allow, which mustn't trip it.

**What's left.** A bench test. The sums that let the watchdog bite before the deadline use the ATtiny1616's 30 % for the 32 kHz oscillator, since DS40002413B gives it no accuracy at all, and the main oscillator's 5 % (figure 38-2). Since a bite is a fault, a spurious one leaves the torch dark until a cell swap.

### H5. Over-discharge of an unprotected 21700 at high current

**Cause.** The cell is unprotected, and the listing asks for one rated at 15 A or more. A 6 V, 6 A boost output from a single cell pulls well over 10 A from it near turbo (arithmetic before losses, so likely), more as the cell falls.

**Mitigation.** `battery::Protection` steps the ceiling down below `battery::STEP_DOWN_BELOW` (3.00 V) and fades it down and off below `battery::OFF_BELOW` (2.90 V). Turning off for a flat cell latches until a reading with the light off is `battery::CHARGED_AT` (3.60 V) or more. At rest the torch reads the cell after every `torch::CHECK_TICKS` (five minutes), and on its first tick after power-on, through `Limits::tick` with the light off as a press does, so a flat cell that's been charged reads as good again without a press. A cell it can't read is a fault (H15). The reading is VDD, which is the cell, through the ADC's VDDDIV10 input against 1.024 V (DS40002413B §33.5.7), four 12-bit conversions accumulated, with the stock firmware's slope of 1014 (`battery::SLOPE`), so a sixteenth of the raw sum less about 1 %. `battery::centivolts` does that with two 16-bit products, no 32-bit multiply.

**Evidence.** Host tests in src/battery.rs, among them `matches_the_formula_for_every_reading_the_adc_can_give`, and in src/limits.rs and src/torch.rs. For the check at rest, `the_cell_is_read_on_the_first_tick_and_after_every_five_minutes_of_rest`, `a_press_or_the_light_starts_the_five_minutes_again`, `a_flat_cell_charged_at_rest_is_good_again_at_the_next_check`, `a_fault_skips_the_checks` and `random_switching_with_long_rests_never_breaks_a_promise` in src/torch.rs, and `an_unloaded_reading_is_one_with_the_light_out_for_two_ticks_before_it` and `a_check_at_rest_seeds_the_filter_afresh_and_leaves_the_ceiling` in src/limits.rs. The docs/ui.md rows with a cell voltage, which run on the simulator too, with the ADC's VDD / 10 reading scripted.

**Status.** Host tests for the logic, simulator for the reading, which is scripted.

**What's left.** Every threshold is unmeasured on this torch. The 1.024 V reference is ±4 % (table 38-18), worth 0.12 V at 3 V, which is more than the 1014 corrects for. Read 4 % low, the light cuts out early. Read 4 % high, it steps down at about 2.9 V real and cuts out at about 2.8. The ramp's ceiling stays low until someone knows what the top of it draws.

### H6. The MCU browning out with the cell

**Cause.** VDD is the cell, through a reverse-polarity FET and no regulator ([hardware.md](hardware.md#power-and-the-battery-reading)). A cell sagging under load pulls the MCU's supply down with it. With the BOD on, the chip resets below its level (1.9 V to 2.85 V typical, table 38-16). On this torch the BOD is off. Its fuses read factory defaults, `BODCFG` 0 and `SUT` 0 ms (confirmed on this torch, 2026-11-16), and the stock source's BOD-on and 64 ms settings were never programmed. So a sagging cell browns the MCU out with no reset at all, and the CPU can keep running below the voltage its clock needs until POR at about 1.6 V (table 38-4), with PC1 high and the DAC holding a current. Code running out of its voltage range might do anything, including drive PC1. When POR does trip, PC1 tri-states (H1), and a cell that recovers and sags again could pulse the light.

**Mitigation.** None in firmware beyond H5. With the BOD off and VDD being the cell, a sagging cell browns the MCU out with no reset to catch it. The firmware can't turn the BOD on, since that's a fuse and nothing here writes fuses, and `selftest::Fuses::ok` doesn't ask for it, since this torch has it off. The BOD's level and mode can't be changed from software either, so low-voltage protection (H5) is the only cover, and it's meant to stop the cell getting near that low with the light on. The main clock is 4 MHz, a third of the stock firmware's 12, which costs less current. The datasheet specifies the main clock to 24 MHz across the whole 1.8 to 5.5 V range (table 38-15), so the slower clock buys no specified voltage margin, and below 1.8 V nothing is specified at all.

**Evidence.** The fuse read is the evidence that the hazard is live on this torch. Nothing covers it.

**Status.** Not yet.

**What's left.** How far the cell sags at what level, what the boost converter does as VDD and its input fall together, and whether a low-voltage threshold well above the brown-out region is enough with a cell that's sagging fast under turbo. The ramp's ceiling should stay low until someone has measured the sag.

### H7. Overheating

**Cause.** A lit light in still air, or a head that's covered.

**Mitigation.** `thermal::Thermal` steps the ceiling down towards `thermal::FLOOR` as the temperature passes `thermal::LIMIT` (50 °C at the sensor). `ramp::CEILING` holds the light well below full output until that's been checked on a real head. The sensor reads against the 2.048 V reference with its factory calibration from `SIGROW.TEMPSENSE0` and `TEMPSENSE1` (§33.3.3.8). `temp::sixteenths` does the datasheet's formula exactly on four accumulated conversions, building the 32-bit product from 8-by-8 multiplies. This torch's calibration, from the backup, is a slope of 0x03EE and an offset of 0x0D24, and the host tests use those.

**Evidence.** Host tests in src/thermal.rs and src/limits.rs, and docs/ui.md's rows with a temperature, which run on the simulator too, with the sensor's reading scripted.

**Status.** Host tests for the logic, simulator for the reading, which is scripted.

**What's left.** Every constant is unmeasured on this head, and the levels the floor and ceiling name don't mean the same current on a DAC curve as they did on the TS26. Thermal's floor, level 50, is about 3.5 % of the ramp's top.

### H8. Lit in a pocket or a bag

**Cause.** A press on the tail switch under pressure.

**Mitigation.** The 3C lockout (docs/ui.md). In lockout `ui::Ui` never lights, and answers only to 3C. A hold from off does nothing either. Clicks count as they're let go when no more would mean anything (`ui::Ui::most_clicks`), but never in lockout, so four presses there are a 4C, which does nothing, rather than 3C to unlock and 1C to light.

**Evidence.** Host tests in src/ui.rs and src/torch.rs, and the Lockout rows of docs/ui.md, on the host and on the simulator.

**Status.** Simulator.

**What's left.** Nothing limits how long a lit light stays lit. Lockout lives in RAM, so a cell swap drops back to off, unlocked. Nobody has tried this tail switch's feel. The aux that shows lockout on the TS26 is the AW2016 here, which this firmware drives, bright red for five seconds after a press, but only on the simulator so far (H13).

### H9. Corrupt or half-written flash

**Cause.** An interrupted or bad flash, or a bit that's flipped.

**Mitigation.** `hw::flash_ok` runs CRCSCAN over all of flash at power-on, and a failure is a fault. The AVR32DD20 has CRCSCAN (DS40002413B chapter 30) over 32 KB, and `cargo xtask hex` signs the full 32 KB image. `flash` refuses a full image whose CRC doesn't check.

**Evidence.** Host tests in xtask/src/crc.rs, and the simulator, which runs CRCSCAN over the signed image and docs/ui.md's `flash:corrupt` rows over one with a byte changed.

**Status.** Simulator, which runs CRCSCAN over the signed image and over one with a byte changed.

**What's left.** The scan has never met a real AVR32DD20.

### H10. Wrong fuses

**Cause.** A torch whose fuses the firmware's timing or its own protection don't expect.

**Mitigation.** `selftest::Fuses::ok` checks the AVR32DD20's fields (DS40002413B §8.8): `WDTCFG` 0, `OSCCFG.CLKSEL` the internal oscillator, `SYSCFG0.UPDIPINCFG` UPDI and `CRCSEL` CRC-16, `SYSCFG1.MVSYSCFG` dual or single, and `CODESIZE` and `BOOTSIZE` 0. `BODCFG` and `SUT` aren't checked. `hw::fuses` only ever reads them, and nothing in this repo writes a fuse. This torch's read `00 00 00 FF FF D0 08 00 00` (factory defaults, BOD off, `SUT` 0 ms, dual supply), and they pass.

**Evidence.** `this_torchs_fuses_pass`, `checks_fields_not_whole_bytes` and `each_wrong_field_fails` in src/selftest.rs, against the bytes from the 2026-11-16 backup, and docs/ui.md's `fault` rows on the simulator, whose fuses are those bytes.

**Status.** Simulator, with this torch's fuses and with one that fails the read-back.

**What's left.** Whether other KR1Ks match this one's factory defaults. A bad fuse is the one permanent thing in this log, which is why nothing here writes one.

### H11. Losing UPDI or the chip while programming

**Cause.** A `SYSCFG0.UPDIPINCFG` write to GPIO, a high voltage on the UPDI pad, a slipped probe, or a wrong pad order.

**Mitigation.** The programmer never writes fuses, the user row or a locked chip, and refuses a chip with the wrong signature, which for this chip is `1E 95 3A`. No high-voltage programmer ever goes on this torch, recovery included. On the AVR32DD20 that's doubly so. The UPDI pin, PF7, is rated to VDD + 0.3 V (table 38-1), and the high-voltage override goes on PF6, which the three-pad layout doesn't bring out, at VDD + 2 V or more and 8.5 V at most, against a 9 V maximum (§36.3.2.1.2, table 38-29). The first flash is a first-light image built for the AVR32DD20, which doesn't exist yet, and the full firmware goes on only after that's proven on the light. The TS26's first-light image in the history is for the ATtiny1616 and never goes on this torch. `restore` refuses a backup from another chip.

**Evidence.** Host tests in xtask/src/updi.rs and xtask/src/main.rs, against a fake AVR32DD20 (`updi::tests::fake::Dd20`), which is one reading of the datasheet and pymcuprog and has never been compared with a real chip. On 2026-11-16 avrdude read this torch through the JLHawaii808 kit on its `+ - U` pads with the cell out, and the chip took the key, answered with `1E 95 3A` and read back the same flash twice. One link attempt failed until the probe was re-seated. That's avrdude, not `xtask`, and nothing was written.

**Status.** Not yet.

**What's left.** On 2026-11-16 `xtask`'s `ping`, `backup` and `peek` ran on the torch, cell out, and its backup matched avrdude's byte for byte, so the link, the key, programming mode and the reads work on this chip. Its erase and write first met the real chip the same night, flashing the 736-byte first-light image, which verified, and the chip answered `ping` straight after (rows 2.2 and 2.4 of [breakout.md](breakout.md#results)). That's one write of one small image. A full 32 KB write and `restore` haven't been tried on the torch. Measuring the pads' pitch in millimetres and naming each with a meter before `xtask` first touches them ([flashing.md](flashing.md#the-pads)). No test shows that no command path writes a fuse. It's true by reading the code.

### H12. The programmer feeding the cell rail

**Cause.** The supply pad is VDD, and VDD is the cell. With the cell in, the programmer's 4.5 V and the cell fight over one rail. With the cell out, the programmer powers the MCU and very likely the boost converter's input and the AW2016 (reasoning, unmeasured). The stock firmware starts as soon as the probe lands and might light the emitter. In programming mode the CPU isn't running, so PC1 floats for the whole command (H1).

**Mitigation.** Procedure only. Cell out before anything touches the pads, the switch left alone, the first contact made in the dark while watching the emitter, and a stop on any light that stays on or an adapter that warms ([flashing.md](flashing.md#the-pads), [breakout.md](breakout.md)).

**Evidence.** The 2026-11-16 avrdude read ran with the cell out, as the procedure says, with the chip asleep in the stock firmware. Nothing in that report says what the emitter or the adapter did, so it says nothing either way about the cell rail.

**Status.** Not yet.

**What's left.** Whether `+` reaches the boost converter's input, how much a USB adapter can push into it, and what holds PC1 in programming mode, are all unknown.

### H13. The AW2016 drawing current in storage

**Cause.** The AW2016 sits on the cell rail (likely), so it draws its standby current whatever the MCU does. A cell left in a drawer drains through it, and through anything else on the rail, with nothing in firmware to stop it.

**Mitigation.** Whenever the aux are off, the firmware puts the AW2016 in standby, which is its lowest state short of shutdown. `aw2016::Aw2016` sends every output off then GCR1 with CHIPEN clear (`aw2016::STANDBY`), or a software reset from a state it doesn't know, at power-on and after a failed update (`aw2016::FROM_UNKNOWN`), which also leaves it in standby. The datasheet gives standby as 2 µA typical and 10 µA at most, at 3.8 V with SCL and SDA at 1.8 V, against 100 µA typical (150 µA at most) awake with every output off. Shutdown is 2 µA at most, but it only comes from holding SCL low for over 130 ms, which would put the board's pull-up across the cell for as long as it lasted, so off isn't shutdown. The aux are off whenever the light's on, and the AW2016 goes to standby in the same pass, before PC1 rises (src/main.rs). Lit, the aux run at the lowest global current, 5 mA, with PWM for the dim colours (`aw2016::DIM_PWM`, a code of 64). Off's dim blue is then 5 mA × 1/15 through the PWM, at most 84 µA if a code of 64 is at most a quarter of the time (the datasheet calls its curve exponential and gives no formula), plus the chip's 100 µA awake, so about 0.18 mA typical and 0.23 mA at most. That's at most 4 to 6 mAh a day, a third of a 5,000 mAh cell a year. Lockout's bright red is 5 mA, for five seconds after a press.

**Evidence.** A reviewer measured 30 µA off on a pre-production KR1K with stock firmware, and 220 µA with aux low in blue ([TG Reviews](https://tgreviews.com/2026/06/05/noctigon-kr1k/)). That's the whole torch, and one sample. The figures above are the AW2016 datasheet's (V1.3), at a supply nobody has checked is this board's. `off_is_standby_and_sends_nothing_once_its_there` and `through_every_ui_md_example_the_aux_are_dark_while_lit_and_catch_up` in src/aw2016.rs, and the simulator, which holds the AW2016 to standby at the end of every pass where the torch wants the aux off (`AuxWatch` in xtask/src/sim.rs), and refuses the aux lit with PC1 high.

**Status.** Simulator.

**What's left.** Measuring off current on this torch, stock first (from a backup) and then with this firmware, both with the aux dark and with dim blue. The stock firmware also clears CHIPEN when its aux go off, so the two should match with the aux dark. Whether this AW2016 is on the cell rail, and so whether 3.8 V figures apply, is likely and unchecked. The library keeps off's dim blue lit with no timer, and puts it out while the last reading with the light off was under `torch::AUX_DARK_BELOW` (3.30 V), until one is `torch::AUX_LIT_FROM` (3.40 V) or more. The torch reads the cell at rest every `torch::CHECK_TICKS` (five minutes) for that (H5). Lockout's bright red shows for only five seconds after a press (`torch::LOCKED_TICKS`). The cut-off matters more here than on a torch with a regulator, since VDD is the cell and the BOD is off (H6), so a cell the aux run down far enough would take the MCU with it and no reset would catch it. With blue dark at 3.30 V, only the torch's own standby is left on the cell. Those limits are in the library and its tests (`blue_stays_lit_through_three_days_of_rest_with_a_good_cell`, `a_low_cell_at_power_on_darkens_blue_from_the_first_check`, `blue_goes_dark_under_3_30_volts_and_comes_back_at_3_40`, `readings_with_the_light_on_never_move_the_aux` and `lockout_shows_bright_red_for_five_seconds_after_each_press` in src/torch.rs). The 3.30 V and 3.40 V are unmeasured, and a dead ADC at rest goes unnoticed until a press, so until then blue says nothing about the cell. A failed update leaves the AW2016 however far it got, which could be a colour left lit, awake, until the colour next changes (H19). A reset that isn't a power-on, like the watchdog's, finds the AW2016 as the last firmware left it, and the first update is a software reset for that reason.

### H14. A stray interrupt

**Cause.** An interrupt that something enables by mistake, or whose flag was already up, with a vector that does nothing.

**Mitigation.** `__unhandled_interrupt` in src/avr/rt.rs jumps into `hw::halt`, and `cargo xtask hex` checks the vector table (`vectors` in xtask/src/layout.rs). The AVR32DD20's table has 36 vectors, three of them handled, the PIT (6), TCB0 (14) and PORTD (24). Only those three are ever enabled. TCB0's only while PC1 is high, with its flag cleared before its interrupt goes on. `hw::halt` turns all three off.

**Evidence.** The simulator runs all three handlers, with a switch bounce and a TCB0 period landing on every new instruction, and `unhandled_interrupt` in xtask/src/sim.rs forces one with no handler. The `halt-without-cli` and `halt-leaves-the-light-on` breakages are refused.

**Status.** Simulator. A forced interrupt with no handler, with the light on, has to end in `hw::halt` with the light off and the chip asleep in power-down.

**What's left.** A pending interrupt that can't be taken, and whether it wakes `halt`'s sleep, is as unknown here as it was on the TS26.

### H15. A dead or garbage ADC

**Cause.** A broken ADC, a reading from a rest, or, here, a VDD reading taken while the boost converter pulls the cell down.

**Mitigation.** `battery::plausible` (1.00 to 4.40 V) and `thermal::plausible` reject readings that can't be real, and a run of garbage voltages is a fault. The logic is board-independent, and the readings are ported (H5, H7). A temperature sum that would mean a die below absolute zero, or one past `u16::MAX` sixteenths, saturates rather than wrapping. The check of the cell at rest every five minutes (H5) takes one reading between rests, and `Protection::rest` forgives the glitch count on every tick of rest, so garbage at rest is never a fault and leaves off's blue as it was.

**Evidence.** Host tests in src/battery.rs, src/thermal.rs, src/limits.rs and src/torch.rs, among them `garbage_at_rest_leaves_the_aux_alone_and_is_no_fault` and `a_check_whose_result_never_comes_still_ends`, and `adc_never_answers` on the simulator.

**Status.** Simulator, with an ADC that never finishes a conversion.

**What's left.** A reading that's plausible and frozen isn't detected. A dead ADC at rest goes unnoticed until a press or the light reads it. VDDDIV10 against a ±4 % reference (table 38-18) puts 4.40 V well inside the ADC's range, unlike the TS26's divider, but nobody has compared a reading with a multimeter.

### H16. A panic path in the firmware

**Cause.** An overflow, an index or an unwrap in code that reaches a panic.

**Mitigation.** `hw::halt` is the panic handler, and `cargo xtask hex` refuses an image that links any panic machinery (`no_panics` in xtask/src/layout.rs). That check is about symbol names, not the chip, so it carries over.

**Evidence.** Host tests in xtask/src/layout.rs, the clippy restriction lints on the library, and the `bounds-check` breakage, refused on the AVR32DD20 build.

**Status.** Simulator. `cargo xtask hex` refuses an AVR32DD20 image with panic machinery linked, and `cargo xtask breakages` shows it does.

**What's left.** Re-proving the check on every nightly bump (CONTRIBUTING.md).

### H17. A stuck switch

**Cause.** Dirt, a pressed-in tail cap, or a jammed button.

**Mitigation.** The UI's holds are bounded. From off or in lockout a hold does nothing, and so does a 2H anywhere. With the light on, 1H ramps each time the other way from the last and stops at the end it's heading for, `ramp::CEILING` going up and `ui::FLOOR` going down, and never above either of the other ceilings (H5, H7).

**Evidence.** Host tests in src/ui.rs and src/torch.rs, and `switch_stuck` on the simulator.

**Status.** Simulator, with the switch held from power-on.

**What's left.** A stuck switch keeps the light on at the level it reached. A tail switch on a 100 kΩ external pull-up (PD4) bounces in ways nobody has measured.

### H18. A wrong clock

**Cause.** A wrong `OSCCFG.CLKSEL`, or a clock set-up that isn't the one the firmware's sums assume.

**Mitigation.** `hw::init` writes `CLKCTRL.OSCHFCTRLA` to 4 MHz and `MCLKCTRLB` to no prescaler, both behind the protected-write sequence, which is also what reset leaves (§12.5.7, §12.5.2). `watchdog::CLOCK_HZ` is that clock, and the tick deadline and the ADC's timing start from it. `AUTOTUNE` stays off, since it wants an external 32 kHz crystal (§12.3.6), which this board likely doesn't have. `selftest::Fuses::ok` checks `OSCCFG.CLKSEL` is the internal high-frequency oscillator.

**Evidence.** `each_wrong_field_fails` in src/selftest.rs for `CLKSEL`, and `clocked_as_modelled` in xtask/src/sim.rs.

**Status.** Simulator, which refuses any clock but the internal oscillator at 4 MHz, undivided.

**What's left.** A real chip. The simulator only shows the firmware asks for the clock it models.

### H19. A stuck aux bus

**Cause.** TWI0's bus to the AW2016 failing. The AW2016 not answering, SDA or SCL held low by a fault, or no pull-ups, in which case SCL never rises and the host waits for it forever (section 29.3.2.2.1 has the high time start only once SCL reads high). A wait for the bus with no end would hang the loop with the light on, and the watchdog's reset would be the only way out. A retry on every tick would keep the CPU awake and the bus busy at 64 Hz.

**Mitigation.** `hw::set_aux` polls with no interrupts, and every wait in an update counts down one budget, `TWI_POLLS` reads of `TWI0.MSTATUS` (900), which is over one and a half times the longest update. A NACK, lost arbitration, a bus error or the budget running out ends the update there, with a STOP only if the host still owns the bus, and the host is turned off before `set_aux` returns, whatever happened, so the loop never sleeps with it on (it stops in every sleep mode, section 29.3.5) and PA2 and PA3 go back to inputs. `aw2016::Aw2016::failed` then gives that colour up until the torch wants another, and starts from a software reset when it does. So a dead bus costs one update per change of colour, never one a tick. The aux go before the output in each pass, so a failed update can't hold up the light. `hw::halt` turns the host off too, in case an interrupt with no handler lands mid-update.

**Evidence.** Three runs on the simulator, the AW2016 never answering, SDA held low and SCL held low, through the light on and off, lockout and a ramp, with the light following the library on every tick, every update failing, no retry with the colour unchanged, and every pass inside its budget (`bad_bus` in xtask/src/sim.rs). With SCL held low, a pass takes about 9,800 instructions, 62 % of the simulator's budget of 15,625, which takes every instruction at four clocks. `a_failed_step_gives_up_until_the_colour_changes_then_resets` in src/aw2016.rs.

**Status.** Simulator.

**What's left.** A real bus. The simulator's TWI0 is one reading of chapter 29, and nobody has seen this board's pull-ups, measured its rise time, or seen how the real host behaves with SDA or SCL held low. The longest pass, about 9,800 instructions, is about 2.5 ms on the chip if they average a bit over one clock each, so the watchdog's feed moves by a few WDT clocks at most against the 16 either side of it (src/watchdog.rs). That's reasoning, not a measurement. A failed update can leave a colour lit until the colour next changes, including while the light's on, which is cosmetic and costs current (H13).

## What's left

This is true of every hazard, so it's kept out of the sections above.

- **Nothing from this repo has met a KR1K.** Apart from one avrdude read of the stock torch, every result in the repo's history is from a TS26 or its breakout, on another chip and another driver.
- **Every threshold is unmeasured.** The cell's cut-offs, the temperature limit, the ramp's ceiling, the 3.60 V that clears a flat cell and the deadlines are from the stock firmware, the datasheet or the project's own judgement.
- **The simulator's limits** are in [CONTRIBUTING.md](../CONTRIBUTING.md#the-simulator). It has no chip resets and models nothing analogue, so the driver, the cell and the head are all scripted.
- **There's no run-time limit.** A lit light with a good cell and a cool head stays lit, whatever pressed the switch.
- **[breakout.md](breakout.md) is what would move hazards to bench.** Stage 3's first-light run is the gate for H1, H2, H3, H11 and H12. Until a hazard has a result there, it stays where it is.
