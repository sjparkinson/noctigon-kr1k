# Hardware

What's known about a Noctigon KR1K with the Lume X1-C boost driver, and how sure each fact is.

- **Confirmed** means a primary source says so (Microchip's files, a datasheet, the stock firmware's source).
- **Likely** means it follows from a sibling light or a convention and nothing contradicts it.
- **On this torch** means it was read or seen on this project's KR1K.

No schematic, teardown or driver photo of a KR1K is public. What only the stock firmware's comments say (resistor values, what's wired where) can't be checked from here.

## This torch

- Bought from JLHawaii808 as "Lume X1-C 6V 6A" with an FFL5009R 5000 K emitter (the seller's word) and "21700 flat top unprotected cell only min 15A CDR".
- Shipped with the stock firmware's model 0284 build, version string `0284.0.653607d`, 15,230 bytes. On this torch, from the backup in `backup/stock/` (gitignored). 0284 is the stock firmware's 32 W build, and the PCB code that says which board this is hasn't been read.
- Signature `1E 95 3A`, silicon revision 1.2 as avrdude names it, lock bits `5C C5 C5 5C` (unlocked). On this torch.
- Fuses `00 00 00 FF FF D0 08 00 00` (`WDTCFG` to `BOOTSIZE`), the factory defaults. On this torch. See [Fuses](#fuses).

## Pins

| Pin | Job | Notes |
|---|---|---|
| PC1 | Boost and amplifier enable | High is on. On the VDDIO2 supply. Never gets a pull-up |
| PD6 | Current setpoint | DAC0's output. Digital input buffer off (DS40002413B §34.3.1) |
| PA7, PA6, PA5 | Power paths 1, 2, 3 | Lowest range to highest |
| PD4 | E-switch | Active low, external 100 kΩ pull-up per the stock firmware's comment |
| PA4 | Switch LED | What it reaches on a tail-switch light is unknown |
| PA2, PA3 | TWI0 SDA, SCL | To the AW2016. The board likely has its own pull-ups, unseen |
| PA1 | Unknown | An output held low, as the stock build on this torch has it |
| PA0, PC2, PC3, PD5, PD7 | Unused | They get the pull-up |
| PF6 | nRST | Input only. No pad |
| PF7 | UPDI | The programming pin |

Confirmed as what the stock firmware's hardware definition says, with the pin functions confirmed by DS40002413B §3.1. PA1 is the exception. The stock source gives it a pull-up, but the build on this torch writes 0xF2 to `VPORTA.DIR` at reset and never sets PA1's `OUT` (on this torch, from the backup and a `peek`). What PA1 connects to is unknown.

## The boost enable and VDDIO2

PC1 raises the boost converter and the amplifier together. After any reset every pin is tri-stated until firmware drives it (§18.3.1), and nothing public says whether the board holds the enable low meanwhile.

PC1 to PC3 are on the VDDIO2 supply, and those pins tri-state whenever it's missing (§5.1, §19.3.2.1). Confirmed for the chip. `MVIO.STATUS` reads VDDIO2 as present, and with the cell out on the programmer's supply VDD read 4.43 V and VDDIO2 4.50 V (2026-11-17). On this torch, with no load. So VDDIO2 is very likely tied to VDD, unconfirmed, and unmeasured on the cell or under load. The datasheet gives no voltage at which `VDDIO2S` clears.

To turn on, the stock firmware raises PC1, waits 8 ms, writes the DAC and its reference, then sets the paths. To turn off it zeroes the DAC, sets the reference to 1.024 V, clears PC1, then clears the paths. Confirmed as its order. Whether that order is the safest, on or off, is unconfirmed, and so is what the 8 ms waits for.

## DAC and power paths

- DAC0 is 10-bit, its output DATA / 1024 of the reference, with the 10 bits at the top of `DAC0.DATA`. The output changes when the high byte is written (§34.3.2.3, §34.5.2).
- The stock firmware switches the reference between 1.024 V and 2.5 V from level to level. Confirmed from source.
- The references are ±4 %, and are only specified with VDD at 2.5 V or more (2.7 V for the 2.5 V one), which a nearly flat cell approaches (table 38-18).
- The DAC's output range is 0.1 V to VDD − 0.1 V and it sinks 0.001 mA (table 38-23), so "DAC at zero" is only as low as the board pulls it. Settling is about 7 µs for a full-scale step.
- In power-down the DAC is off (§34.3.3). What PD6 floats to then is unknown.
- Each path routes current through a different sense resistor. How the three combine, and what a path left on with the boost off does, is unmeasured.

The stock firmware's level tables stay out of this repo.

## Power and the battery reading

VDD is the cell, through a P-channel FET for reverse polarity (the stock firmware's comment). There's no regulator, so the MCU sees every sag and browns out with the cell. VDD's floor is 1.8 V and POR releases at about 1.6 V (table 38-4). With the BOD off, nothing resets the chip on the way down.

The battery reading is the ADC's `VDDDIV10` input against 1.024 V (§33.5.7). The stock firmware calibrates it with a slope of 1014. Nobody has checked that against a meter on this torch.

The stock firmware's low-voltage threshold for this build is likely its default of 2.90 V.

## Temperature and clock

The temperature sensor is inside the MCU, read against 2.048 V with the factory calibration in `SIGROW`. How it tracks the head is unknown.

The board likely has no 32 kHz crystal, since its pins are PA0 and PA1. The stock firmware runs the internal oscillator at 12 MHz and ticks at 64 Hz from the RTC's PIT off the internal 32.768 kHz oscillator.

## The AW2016

The aux LEDs are on an Awinic AW2016 at I²C address 0x64 on TWI0. The stock firmware maps red to channel 1, green to 0 and blue to 2. Confirmed from source, and blue and red showed right on this torch.

From its datasheet (V1.3), confirmed for the chip.

- Up to 400 kHz, with external pull-ups of 1 to 10 kΩ.
- VBAT 2.5 to 5.5 V. V1.3 says VBAT has to rise from 0 in under 1 ms for its configuration to load.
- Holding SCL low for over 130 ms shuts it down and resets every register.
- Standby is 2 µA typical and 10 µA at most. Active with every output off is 100 µA typical.
- It gives no wait after CHIPEN. Two other drivers wait 1 to 2 ms, so a wait is likely needed.
- With CHRG high and CHGDIS clear the chip wakes and breathes by itself. What CHRG is wired to here is unknown.
- The PWM is 250 Hz, and its code-to-duty curve is called exponential with no formula.

It likely sits on the cell rail, so its standby current is drawn whatever the MCU does. Unmeasured.

## The switch and the pads

The e-switch is in the tail, on PD4. How it reaches the driver in the head isn't public.

The flashing pads are labelled `+ - U` and fit the JLHawaii808 kit's pogo probe. On this torch. Their pitch hasn't been measured. `U` is UPDI on PF7, whose absolute maximum is VDD + 0.3 V (table 38-1), so no high-voltage programmer belongs on it. `+` is very likely the cell rail, so a programmer's supply there feeds the MCU and likely the boost converter's input and the AW2016 too. Pad order differs between lights, so another KR1K's pads want measuring first.

## The MCU

An AVR32DD20, from `vendor/AVR32DD20.atdf`. 32 KB of flash mapped into data space at 0x8000, 4 KB of SRAM at 0x7000, 256 bytes of EEPROM, 36 interrupt vectors.

Its NVM controller writes flash a word at a time with no page buffer, and UPDI addresses it with 24-bit addresses, flash at 0x800000 (pymcuprog's NVM version P:2).

Errata DS80001026D §2.2.3 applies to every silicon revision so far. A store to an address of 64 or more, followed straight away by a store under 64 or to `SLPCTRL.CTRLA`, loses the second one. How avrdude's revision 1.2 maps onto the errata's A1 to A3 isn't stated, so every erratum is treated as live.

### Fuses

The factory defaults, which this torch has, leave the BOD off, UPDI on PF7, dual supply and `SUT` at 0 ms. The stock source asks for the BOD on and `SUT` 64 ms, and none of that was programmed here.

`SYSCFG0.UPDIPINCFG` set to GPIO turns UPDI off until a high-voltage pulse reaches PF6 (§36.3.2.1.2), which has no pad. So a bad fuse write loses this torch, and this repo never writes fuses.

## Seen on this torch

- 2026-11-16. Stock firmware backed up with avrdude and again with `cargo xtask backup`, byte for byte the same. One link attempt failed until the probe was re-seated.
- 2026-11-16. First flash, then the full firmware. Clicks and holds worked, and the aux showed dim blue when off, bright red in lockout and dark while lit. Yellow and white haven't been seen.
- 2026-11-17. Left off overnight with the cell in, then lit at once, so the watchdog didn't bite at rest.
- 2026-11-17. `bb9ee76` flashed. No flash of light as the cell went in, ramping both ways, lockout and unlock all worked.
- 2026-11-17. Main flashed, with the 1 Hz tick at rest, and reported in good working order. What was checked isn't recorded.
- 2026-11-18. Lit at the floor, a 1C pressed on the edge of the button turned the light off and it came back brighter a moment later, likely at `ui::ON_LEVEL`. Likely more than once that night, and not reproduced the next day.

Not measured on this torch yet are tail current, anything thermal, the low-voltage steps and the top of the ramp.

## Open questions

- What feeds VDDIO2, and does it hold on the cell under load?
- Does anything on the board pull PC1 low through reset?
- What does the amplifier do with the DAC at zero, and what does PD6 float to in power-down?
- Which order is safe, enable then current or the reverse, and what's the 8 ms for?
- How do the three paths combine?
- Which PCB code does this board carry?
- What's the AW2016's standby current here, what's CHRG wired to, and which output is green?
- What do PA1 and PA4 connect to?
- What does the 1014 calibration read against a multimeter, near 3 V and under load?
