# Bench plan

Every hardware check the roadmap is waiting on, in the order to do them, for a bench with the KR1K (Lume X1-C, sold as the 6 V build, AVR32DD20), a Fluke 107 and [JLHawaii808's 4.5 V UPDI kit](https://jlhawaii808.com/products/updi-programmer-driver-reflashling-kit). Every Expect below is a prediction until its row has a result. [flashing.md](flashing.md) has how to use the programmer.

Before this plan was written for the KR1K, the owner read the stock torch with avrdude 8.3 (`-c serialupdi -p avr32dd20`) through the kit on 2026-11-16, with the cell out and nothing written. That answered parts of stage 1 by another route, and the [Results](#results) table has what it found, against the rows it touches. It used avrdude, not `xtask`, so 1.7 and 1.8 still want running with `xtask` before stage 2.

The order is the point of it. **The first flash on the torch is a first-light image built for the AVR32DD20, and the full firmware only goes on after stage 3 has passed.** That image is `examples/first_light.rs`, and it has only run on the simulator. On the TS26 this plan came from, a small first-light image kept firmware faults and board faults apart, and an earlier torch had gone dark straight after a verified flash of a much bigger start-up with nobody able to say which it was.

Each check says what it needs.

- **First light** is the smallest image that turns the light on and off at one low level through PC1, the DAC and the low paths (PA7 and PA6 together, as every level in the low range has them), built for the AVR32DD20. It's `examples/first_light.rs`, and `cargo xtask hex` writes it to `target/noctigon-kr1k-first-light.hex`. It has no watchdog, no sleep, no timer and no interrupts, so there's very little in it to hang. It ignores the switch until it's read released for about 0.6 s, then each press toggles the light. It has only run on the simulator, and nobody has flashed it. It's not the TS26's first-light image in the history (commit `9d3b7dc`), which is for the ATtiny1616 and never goes on this torch.
- **Main** is the full firmware on main, once it's ported.
- **T1** to **T4** are each a [test build](#test-builds).

The Status column says where each row stands. It's blank until the row's been done, then Pass, Fail or Skipped. A Fail isn't always a stop, so read the row's own words for what follows, and put the detail in [Results](#results).

The firmware passing the simulator doesn't change any of this. It's a mistake-finder written from the same reading of the datasheet as the firmware, and it doesn't model a chip reset or anything analogue, so none of its passes count as a result here.

Checks this kit can't do at all are at [the end](#what-this-kit-cant-do), with what would unlock them.

## What the kit can and can't see

Here's the [Fluke 107](https://www.fluke.com/en-us/product/electrical-testing/digital-multimeters/fluke-107), from Fluke's spec table.

| It does | Which means |
|---|---|
| DC volts, 1 mV resolution on the 6 V range | Cell voltage, loaded or not, is the one thing it's really good at here |
| DC amps, 4 A and 10 A ranges only, 1 mA resolution, ±(1.5% + 3 counts) | Fine for low levels. No mA or µA range, so nothing near a sleeping chip's current |
| Frequency, 10 Hz to 100 kHz | Might catch the AW2016's 250 Hz PWM on an aux line, if one can be reached |
| Ohms, continuity, diode, hold | Pad and board checks with the cell out |
| 3 readings a second, no min/max | Can't catch anything shorter than about a second. Eyes are quicker than this meter |
| No temperature | Nothing here checks the sensor against a real temperature |
| 11 A fuse | A 6 V, 6 A boost output pulls well over 10 A from one cell near the top (arithmetic). See the rules below |

The programmer reads flash, EEPROM and fuses, and writes flash and EEPROM. It can't watch the firmware run. So apart from what the meter gets at the tail, what the firmware thinks is invisible unless a test build shows it with the light.

Your eyes are the third instrument. A dark room shows a millisecond flash from the boost converter far better than any of the above.

## A spare chip

The TS26 plan rehearsed every risky step first on a spare ATtiny1616 breakout, with a 12 V UPDI Friend, a breadboard, LEDs and resistors. That kit doesn't apply here. Its chip is the wrong one, and the Friend never goes near an AVR32DD20 that matters ([flashing.md](flashing.md#only-the-plain-adapter-goes-on-this-torch)). Until an AVR DD breakout is chosen, there's no stage 0, so the torch's own chip is the first AVR32DD20 the programmer meets. That's a reason to go slowly through stages 1 and 2, and to stop at anything unexplained.

If one is chosen, stage 0 would rehearse `ping`, `backup`, the first-light image, `flash`, `restore` and its serial check, the reset at the end of `ping` (§36.3.2.2.2), the watchdog biting on purpose (T3) and power-down straight after a feed (T4), all through the JLHawaii808 kit at 4.5 V, before any of it touches the torch.

## Rules

- **Only the plain 4.5 V adapter on the torch.** No 12 V programmer ever touches it, recovery included. Both kinds of adapter show up as CH340 `1a86:7523`, so look at the one in your hand before every command.
- **Nothing here writes fuses.** If a fuse looks wrong, stop and decide with the datasheet open. A bad `SYSCFG0` would need a high-voltage pulse on a pin the pads don't reach, and that means the torch is gone.
- **Cell out before anything touches the pads.** VDD is the cell, so the programmer powers the MCU, and very likely the boost converter's input and the AW2016, through the supply pad. Check the ground pad is continuous with the battery negative first, every time you've had it apart. Don't touch the switch with the probe on.
- **Name all three pads before anything touches them.** This torch labels them `+ - U`, but labels aren't a measurement. Continuity finds ground, and 1.5 tells the supply pad from the UPDI pad by how each reads against it. If any naming is in doubt, stop.
- **Don't hold the probe by hand if you can avoid it.** Clamp it, or use something else that can't slip. A slip across pads this close together shorts them.
- **Touch ground before the probe lands.** An ESD pulse on the UPDI pin can be read as a high-voltage override if the board's protection isn't enough (DS40002413B §36.3.2.1.2, note 1). Touch the adapter's USB shell and the torch's battery negative contact before each contact.
- **Measure before trusting a probe.** The JLHawaii808 probe is made for 1.27 mm pads, and the TS26 that was lost had pads about 1.75 mm apart. If this board's don't fit, use loose leads.
- **"Nothing echoed back" on the pads means stop.** Something's holding the line down. Take the probe off and measure ([flashing.md](flashing.md#if-it-doesnt-talk)).
- **Never put the meter in series above 3 A.** The level that gets to 3 A isn't known. Start low and ramp up slowly, and stop at 3 A whatever the level.
- **Stay with it in short bursts at high levels.** The thermal constants have never met this head, and the cell's unprotected. A hand on the head is the only thermometer here, so stop when it's uncomfortable.
- **Don't take the cell under 2.8 V.** The firmware should stop it sooner, but that's one of the things being tested. The first-light image has no protection at all, so don't leave a cell in it.
- **One backup directory per chip.** `backup/torch` for this torch, used as given from the repo root. `restore` refuses a backup read from another chip.

Any firmware problem below is recoverable while the chip still talks. Flash and EEPROM are all that ever change, so `cargo xtask restore <port> backup/torch` puts the stock light back. A torch that stops talking isn't recoverable by anything in this plan.

## Stage 1, the pads, and stock

The last chance to measure the stock firmware, and the first time `xtask` meets this board.

| # | Status | Do | Expect | Closes |
|---|---|---|---|---|
| 1.1 | | Cell voltage on the Fluke, out of the light | 3.6 to 4.2 V. Write it down | |
| 1.2 | | On stock, tail current off, at the lowest level and a few levels up, cap off, meter from cell negative to the tube. Stop at 3 A. Note how the bottom looks and whether it whines | Off should be tens of µA, under this meter's resolution (a reviewer measured 30 µA on a pre-production light). Write the rest down as the comparison for stage 5 | |
| 1.3 | | Cell out. Find the pads, and photograph them. Read the PCB code while you're there. Don't force anything that's glued | Three pads, likely labelled `+ - R`. The code tells 0283 from 0284 ([hardware.md](hardware.md#which-kr1k-this-is)) | "Find the flashing pads" |
| 1.4 | | Measure the pitch, with calipers or against something of known size | 1.27 mm if it's the common layout. Write it down. If it isn't what the kit's probe is made for, use loose leads | The open question on pitch |
| 1.5 | | Continuity from each pad to the battery negative contact. Then ohms from each of the other two to the ground pad, watching how the reading moves over a few seconds. Then diode mode from each pad to the battery positive contact, both ways round | Exactly one pad is ground. If none is, stop. The supply pad is VDD, which is the cell rail, so it should start low, climb as the meter charges the board's capacitors, and settle, and in diode mode it should look nearly like a short to battery positive through the reverse-polarity FET or its body diode. The UPDI pad has only PF7 on it, with its pull-up inside the chip, so it should read very high from the start (reasoning from DS40002413B §8.8.2.4, unmeasured). If the two don't differ clearly, stop | "Check their order with a multimeter" |
| 1.6 | | Ohms between each pair of pads, both ways round | Write them down as the baseline. A few ohms between all three is a short, and that means stop. If anything here disagrees with 1.5's names, stop | |
| 1.7 | | `cargo xtask ping <port>`, first with the probe touching nothing, then on the pads, touching ground first. First contact in a dark room, watching the emitter, with a finger on the adapter | In the air, "no reply from the chip". On the pads, an answer naming NVM P:2 (pymcuprog's name for this chip's NVM). The stock firmware starts when the probe lands, so a blink is possible. Light that stays on, or warmth in the adapter, is a stop | First half of "ping, back up and flash" |
| 1.8 | | The same watch as 1.7, since this is the first time the chip sits in programming mode with PC1 floating. `cargo xtask backup <port> backup/torch`, then copy that directory somewhere safe | `flash.hex`, `eeprom.hex`, `fuses.hex` and `serial.hex`, signature `1E 95 3A`, and dark and cool throughout. Dark is the first hint that something holds PC1 low, and no proof, since the DAC is off too. If `backup` warns that it can't tell one chip from another, stop before flashing anything | Second half |
| 1.9 | Pass | Read `backup/torch/fuses.hex`. The bytes run `WDTCFG`, `BODCFG`, `OSCCFG`, two reserved, `SYSCFG0`, `SYSCFG1`, `CODESIZE`, `BOOTSIZE` (DS40002413B §8.8.1) | `SYSCFG0.UPDIPINCFG` UPDI. If the stock factory programming wrote the stock ELF's fuses, `BODCFG` has the BOD on in active mode and `SYSCFG1` is dual supply with `SUT` 64 ms. Factory defaults would show `SUT` 0 ms. Write it all down. Don't write any fuse | What fuses a shipped board has |
| 1.10 | | Cell in. A version check from off (15 or more clicks), counting the blinks | The build date and then 0283 or 0284. That and 1.3's code say which build this board wants | Which build |

If 1.9 shows anything but UPDI on PF7, stop. That's a chip one bad write away from lost.

### What the adapter powers

With the cell out, the adapter feeds VDD through the supply pad, and VDD is the cell rail. So it very likely reaches the boost converter's input and the AW2016 as well as the MCU (reasoning, unmeasured).

That matters because the chip doesn't hold PC1 low for the whole time the probe's on. After a reset every output is tri-stated (DS40002413B §18.3.1), so PC1 floats until firmware drives it. In programming mode, which is all of `backup`, `flash` and `restore`, the CPU isn't running, so PC1 floats for as long as the command takes. PC1 is on the VDDIO2 supply, and nobody knows what that's connected to ([hardware.md](hardware.md#the-boost-enable-pc1)). `ping` and `peek` don't enter programming mode, so the firmware runs and drives PC1 itself, and stock starts again after the reset `ping` ends with.

So the emitter could light with the adapter as its only supply. A single blink as the probe lands is possible and not a stop on its own. Light that stays on through `backup` or `flash`, or warmth in the adapter, is a stop, so lift the probe.

## Stage 2, first flash

The first-light image only. The full firmware waits for stage 4.

| # | Status | Do | Expect | Closes |
|---|---|---|---|---|
| 2.1 | | Look at the adapter in your hand. Cell out. Ohms between the pads again | The plain kit, and the same numbers as 1.6 | |
| 2.2 | Pass | Flash the first-light image, `cargo xtask flash <port> target/noctigon-kr1k-first-light.hex` ([flashing.md](flashing.md#the-first-flash-is-a-first-light-image)) | `flash` warns about an image short of the full flash, which is expected for first light, then verify passes. A verify only shows the bytes arrived | "Ping, back up and flash", the AVR Dx NVM writes |
| 2.3 | | If verify fails | Run it once more. If it fails again, stop there. Nothing has proven the programmer's AVR Dx NVM sequence on a real chip before this, so suspect it as much as the contact | |
| 2.4 | Pass | `cargo xtask ping <port>`, then probe off | Same answer as 1.7. A chip that talked before the flash and doesn't after it is how the TS26 was lost, so stop and measure | |

## Stage 3, first light on the torch

The proof that this board does what the repo thinks. In a dark room.

| # | Status | Do | Expect | Closes |
|---|---|---|---|---|
| 3.1 | Pass | Put the cell in and do up the head, watching the emitter. Repeat ten times | No flash at all. PC1 floats through reset and `SUT` until the image drives it, so a flash here means nothing's holding it low, or VDDIO2 isn't what anyone thinks. The image ignores the switch until it's been let go for about 0.6 s, so pressing it while doing up the head shouldn't light it either | The open question on what holds PC1 low, by eye |
| 3.2 | Pass | Press on, press off, a few dozen times | Low, steady, and dark again every time. It changes as the press lands, about 40 ms in, not on the release, and holding the switch down does nothing more. No flash at turn-on, no blip at turn-off | "Does it come on cleanly, with no flash at turn-on?" |
| 3.3 | Skipped | Tail current, on | Write it down. It's the first number for what a DAC value does on the low paths, code 92 against 1.024 V | Part of "measure what the DAC does" |
| 3.4 | Skipped | Tail current, off | A few mA, since the first-light image doesn't sleep. Much more and something's on that shouldn't be. Write it down | |
| 3.5 | | Ten minutes on, in sight | Doesn't move, doesn't go out | |
| 3.6 | | Cell out. Ohms between the pads, then `cargo xtask ping <port>` | Same as 1.6, and the chip still talks | |

**Stage 3 is the gate.** All of it passes, or the full firmware doesn't go on. If the light won't come on, `cargo xtask restore <port> backup/torch` puts the stock firmware back, and a stock light that works means the fault's in the first-light image and how it reads this board.

Write the result in the [table](#results) before going on.

## Stage 4, the full firmware

| # | Status | Do | Expect | Closes |
|---|---|---|---|---|
| 4.1 | Pass | `cargo xtask flash <port>` with no file, then `ping`, then probe off and leave it a few seconds with no cell, so VDD really collapses | Verify passes, and it still talks | |
| 4.2 | | If the light's dark from here on | `cargo xtask peek <port>` first, since only the first run shows how far the firmware got. Then flash the first-light image again. If that still works, it's the firmware, and the board's fine | |
| 4.3 | Pass | Cell in, head on, ten times, watching the emitter | No flash, and off until clicked | |
| 4.4 | Pass | 1C on, 1C off, a few dozen times, then holds | Same feel as the docs/ui.md rows | |
| 4.5 | | Tail current, off, on the 4 A range | 0.000 to 0.003 once it's asleep. Anything steadily over about 5 mA isn't a sleeping chip, or it's a reset loop | The rough version of the off current |
| 4.6 | | An hour on at a low level, in sight | Stays on and doesn't move. Going out is the watchdog biting or the tick deadline | "It never trips in normal use", in part |
| 4.7 | | Leave it off overnight, then 1C | Comes on at once | |

## Stage 5, the ramp

Short bursts at high levels, and the meter comes out at 3 A.

| # | Status | Do | Expect | Closes |
|---|---|---|---|---|
| 5.1 | | 1C, then 1H to the top and 1H again to the bottom of the ramp, in the dark | The floor lights, steadily | That the floor holds on this light |
| 5.2 | | Ramp slowly end to end, and through each change of path and reference | Smooth, no steps that jump, no flash where the path or the reference changes, each direction | That the path and reference changes are clean |
| 5.3 | | T1 at a handful of DAC values on each path, tail current, stopping at 3 A | A table of current against path and DAC value. That's what this firmware's own curve stands on | "Measure what the DAC does" |
| 5.4 | | Ear to the head at low and middle levels | Any whine, compared with 1.2 | |
| 5.5 | | Top of the ramp, a few seconds. Cell volts across the cell | Write down the sag. Since VDD is the cell, that's also the MCU's supply | |
| 5.6 | | Same run, in the dark, off and on a few times | No blip on either edge. A visible lag is fine, given the 8 ms the stock firmware waits after PC1, and a flash isn't | |

## Stage 6, faults and the watchdog

| # | Status | Do | Needs | Expect | Closes |
|---|---|---|---|---|---|
| 6.1 | | 1C right after a flash | Main | Lights. A fault at power-on (flash CRC, fuses, a watchdog reset) keeps it dark. Suspect, in order, a bite (a cell swap clears it), then `xtask/src/crc.rs`, then the fuse check | The CRC, the fuse check |
| 6.2 | | Leave it off for an hour, then 1C | Main | Lights. Dark means the watchdog bit while it rested | That a resting torch doesn't bite |
| 6.3 | | T4, power-down straight after a feed, left overnight. Then 1C, and tail current off | T4 | Lit on 1C, and 0.000 to 0.003 A. Steady mA means it's resetting over and over | "Test power-down straight after a feed" |

## Stage 7, voltage and heat

| # | Status | Do | Needs | Expect | Closes |
|---|---|---|---|---|---|
| 7.1 | | Run a cell down, with the meter across it between bursts, then a middling level | Main | It steps down near 3.00 V and turns off near 2.90 V, loaded, by `src/battery.rs`. Unmeasured | The low-voltage item |
| 7.2 | | Compare a blinked-out voltage with the Fluke at three or four charge levels, off and on | T2 | Within 0.1 V. This tests the VDDDIV10 reading and its calibration on this light | "Check against a multimeter" |
| 7.3 | | Near the cut-off, off for a minute, then on | Main | It starts, and the MCU doesn't reset as the boost converter pulls the cell down. A reset here is H6 in [safety.md](safety.md) | That VDD survives turn-on near the cut-off |
| 7.4 | | Blink out the temperature at a stable room, three or four readings | T2 | Near the room. There's no reference to hold it to, so this only finds gross errors | Part of the thermal item |
| 7.5 | | Top of the ramp until it steps down, with a thermometer on the head | A thermometer | It steps down before the head's painful. Only after this does the ceiling get raised, a step at a time | "Thermal regulation" |

## Test builds

None exist yet, and they should stay on a throwaway branch, because they aren't the firmware.

**T1, another level.** A build that comes on at a set path and DAC value, for 5.3. Build it with `cargo xtask hex --no-examples`, which leaves the docs/ui.md examples out of the simulator run and writes the CRC as usual.

**T2, a number out.** A build that blinks the last cell reading (and, in a second build, the temperature) out on the main LED at the floor, volts on long blinks and tenths on short ones, on 6C. It keeps the real firmware's protection, except that a blink lights the floor whatever the limits say.

**T3, a hang on demand.** A build that stops feeding the watchdog after a long hold from on, so a real bite can be seen once. **Spare chip only**, and there's no spare AVR DD chip yet, so it waits.

**T4, power-down after a feed.** `watchdog::Feeder::power_down` returning `resting` alone, dropping the `&& !self.fed`. Only to find out if the idle on the tick after a feed can go.

## What this kit can't do

| Check | Why | What would unlock it |
|---|---|---|
| Off current to the µA, the AW2016's standby current | No mA or µA range | A 1 kΩ resistor in series with the cell, which gives 1 mV per µA on the 600 mV range |
| What VDDIO2 is connected to | The pin's on a 3 mm VQFN under the head | A fine probe on the pin with the cell out, against VDD and ground. Only with a tip that can't slip |
| PC1 and PD6 through a real reset | Too quick for the meter | A scope. 3.1 by eye is the stand-in |
| The turn-on order, and the 8 ms after PC1 | Needs a scope on PC1, PD6 and the boost output | A scope |
| Which chip the boost converter and the amplifier are | Under the head, possibly potted | Reading the markings through a loupe if the driver can be seen. Don't dig |
| Sensor against a real temperature, thermal tuning | No thermometer | Any thermometer on the head |
| Cell sweep from 4.2 V down | No bench supply | A bench supply. 7.1 covers the part that matters |

Until those happen, docs/hardware.md's labels stay as they are. Move a fact from likely to confirmed only with a result from here beside it.

## Results

Fill this in as you go, with the commit you flashed.

| # | Date | Commit | Result |
|---|---|---|---|
| 1.3 | 2026-11-16 | stock | Partly, during the avrdude read. Pads found, labelled `+ - U` on the board, and the kit's pins mapped onto them. The PCB code wasn't read, and no photograph is recorded |
| 1.4 | 2026-11-16 | stock | Partly. The kit's pogo probe fits the pads. Not measured in millimetres |
| 1.7, 1.8 | 2026-11-16 | stock, avrdude 8.3 | Not these rows' commands, so neither is passed. avrdude through the kit, cell out, read signature `1E 95 3A`, silicon revision 1.2, "NVM type 2: 24-bit, word oriented write". The chip was asleep in the stock firmware and took the NVM programming key. One attempt failed with "UPDI link initialization failed", likely a shifted probe, and worked after re-seating. Flash read twice, matching byte for byte, used to 0x3B7D (15,230 bytes). Saved in `backup/stock/` (gitignored). Nothing written. What the emitter and the adapter did isn't recorded |
| 1.9 | 2026-11-16 | stock, avrdude 8.3 | `00 00 00 FF FF D0 08 00 00` (`WDTCFG` to `BOOTSIZE`), the factory defaults. `SYSCFG0` 0xD0, UPDI on PF7. `BODCFG` 0, so the BOD is off, and `SYSCFG1` 0x08, dual supply with `SUT` 0 ms, so the stock source's fuse settings were never programmed (H6 in [safety.md](safety.md)). Lock bits `5C C5 C5 5C`, unlocked. Not written |
| 1.10 | 2026-11-16 | stock | Answered from the backup rather than a version check. The flash holds `0284.0.653607d`, the stock firmware's 0284 (32 W) build, not 0283 |
| 1.7 | 2026-11-16 | `8a601a1` | `cargo xtask ping` on the pads, cell out, answered `AVR     P:2D:1-3`. An earlier attempt with the probe off the pads gave "no reply from the chip", as expected. What the emitter and the adapter did isn't recorded, so the row isn't marked passed |
| 1.8 | 2026-11-16 | `8a601a1` | `cargo xtask backup` to `backup/torch/` read flash, EEPROM, fuses, lock bits, user row and serial number. Each matches the avrdude backup in `backup/stock/` byte for byte (flash padded to 32 KB, serial against prodsig bytes 0x10 to 0x1F). Nothing written. The emitter watch isn't recorded, so the row isn't marked passed |
| VDDIO2 | 2026-11-16 | `8a601a1` | `cargo xtask peek`, cell out, on the kit's power. `MVIO.STATUS` 0x01, so VDDIO2 is present. With the stock firmware asleep, PC1 an output held low, the DAC enabled at 0 on the 1.024 V reference, PA1 and PA5 to PA7 outputs, PD4 reading high, the clock at 12 MHz with autotune on, divided by 4. `RSTFR` 0x21 |
| 2.2 | 2026-11-16 | `a9ef250` | `cargo xtask flash` wrote the 736-byte first-light image (SHA-256 `8f70c80f…e304`), warned it was short as expected, verified and restarted the chip. The first erase and write this programmer has done on a real chip |
| 2.4 | 2026-11-16 | `a9ef250` | `cargo xtask ping` after the flash answered `AVR     P:2D:1-3`, the same as before it |
| 3.1, 3.2 | 2026-11-16 | `a9ef250` | First light on the torch. Cell in and head done up, then clicked on and off. Reported as all fine, no flash and a steady low light. The count of cell insertions and clicks wasn't recorded |
| 3.3, 3.4 | 2026-11-16 | `a9ef250` | Skipped, no tail current measured |
| 3.5, 3.6 | 2026-11-16 | `a9ef250` | Not done. The full firmware went on without them, by the owner's choice after 3.1 and 3.2 looked right |
| 4.1 | 2026-11-16 | `910c667` | `cargo xtask flash` wrote all 32 KB of the full firmware (stamp `kr1k 910c66741a`, SHA-256 `8168f352…64c3`), verified and restarted. `ping` answered `AVR     P:2D:1-3` after it. The first full-size write this programmer has done on a real chip |
| 4.3, 4.4 | 2026-11-16 | `910c667` | Cell in and head on, then clicks and holds. Reported as all good, the light off until clicked and the UI as docs/ui.md has it. The counts weren't recorded |
| 4.1 peek | 2026-11-16 | `910c667` | `cargo xtask peek` with the full firmware resting, cell out, on the kit's power. PC1 an output held low, DAC0 enabled at 0 on 1.024 V, `VPORTA.DIR` 0xF2 with OUT 0, PD4 high, 4 MHz with no prescaler or autotune, `WDT.CTRLA` 0x33, PIT at CYC512 and on, TCB0 stopped with CCMP low byte 0x9F, SREG.I set, SP 0x7F81 (about 126 bytes of stack), `MVIO.STATUS` 0x01, `RSTFR` cleared. All as the code intends at rest |
