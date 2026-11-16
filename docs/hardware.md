# Hardware

What's inside a Noctigon KR1K with the Lume X1-C boost driver, and how sure the docs are about each bit. "Confirmed" means a primary source (Microchip's files, the stock firmware's source, a datasheet, or someone who opened one up) says so. "Likely" means it follows from a sibling light, a reviewer's guess or a convention and nobody's contradicted it. "Confirmed on this torch" means it was seen on this project's KR1K. On 2026-11-16 the owner reached its flashing pads with the cell out and read the chip with avrdude 8.3 (`-c serialupdi -p avr32dd20`) through the JLHawaii808 kit, writing nothing. That session is the only thing anyone here has seen of this board, and no published teardown or driver photo of a KR1K has been found.

Facts from the stock firmware come from its source, as it stood in November 2026. Anything it sets in a register could be checked against the backup of this torch's flash (`backup/stock/`, gitignored), and nobody's done that yet. What only its comments say, such as resistor values and what's wired where, can't be.

## Which KR1K this is

The KR1K is a 21700 single-emitter torch with a TIR optic, a tail e-switch, RGB aux LEDs and no charging ([Hank's product page](https://intl-outdoor.com/product/noctigon-kr1k-21700-tail-e-switch-led-flashlight/)). Every public source puts the switch in the tail. The light comes with more than one driver. The TIR version this repo is for has the Lume X1-C, a boost driver, and the reflector version uses a different buck driver. Nothing here applies to the reflector version.

This torch was bought from JLHawaii808 with an FFL5009R 5000 K emitter and the Lume X1-C. The listing, as the owner pasted it on 2026-11-16, says "Lume X1-C 6V 6A high efficient boost driver (single 6v led)" and "21700 flat top unprotected cell only min 15A CDR! DO NOT use button top or protected cells". Confirmed as what the listing says. That makes it the 6 V single-emitter build. The emitter is the seller's word, and nobody has looked.

The stock firmware has no KR1K build of its own. The KR1K runs its build for Hank's Lume X1-C, model 0283, on an AVR32DD20. A second build, model 0284, is the 32 W version. The driver designer's build page ([loneoceans](https://loneoceans.com/labs/temp/anduril/)) maps the PCB code "C 0126" to 0283 and "C 0126 / C 0626" to 0284, described as 32 W "for 18650", and says to choose by the code printed on the board. Confirmed as what that page says. The stock firmware says its 6 V, 9 V and 12 V boards share a build. Confirmed as what it says.

This torch shipped with the 0284 build. Its flash holds the version string `0284.0.653607d`, and the image runs to 0x3B7D (15,230 bytes). Confirmed on this torch, from the backup. That's the build the stock firmware calls 32 W and the designer's page says is "for 18650", on a light sold as a 6 V 21700 build, so either the listing, the build page or Hank's choice of build is loose. Nobody has read the PCB code yet, which is what the build page says to go by. Support for the X1-C landed in the stock firmware in September 2026, so a torch shipped before then likely had other firmware on it (an inference from the dates).

## The driver

The stock firmware's hardware definition calls the board a "40W Boost Driver with Ultra Dynamic Range, RGB & SW Aux, Powerbank", on an AVR32DD20 in the 20-pin 3 mm VQFN. Confirmed as what the stock build says, in the sense that it's what Hank's lights are flashed with. It says the MCU drives a DAC "reference voltage for a current-regulated amplifier which drives the LED", with three power paths that route current through different sense resistors. Nobody has named the boost converter chip, the amplifier, or any resistor value, and no schematic is public.

| Pin | Job | Notes |
|---|---|---|
| PC1 | Boost and amplifier enable | Output, high is on. **The dangerous pin.** On the VDDIO2 supply, see [The boost enable](#the-boost-enable-pc1) |
| PD6 | Current setpoint | DAC0 output into the amplifier. Digital input buffer off, as DS40002413B §34.3.1 asks |
| PA7 | Power path 1 | Lowest range |
| PA6 | Power path 2 | Low range |
| PA5 | Power path 3 | High range |
| PD4 | E-switch | Active low, with an external 100 kΩ pull-up per the stock firmware's comment, so the internal one stays off |
| PA4 | Switch LED | Plain GPIO in the stock build. What it drives on a KR1K is unknown, see [The switch](#the-switch) |
| PA2, PA3 | TWI0 SDA, SCL | To the AW2016 aux driver |
| PA0 | A FET path through TCA0 WO0 | Not connected on the Lume X1, per the stock firmware's comment. It gets the pull-up |
| PA1 | Unknown | An output held low, no pull-up, as the stock build on this torch has it, see below |
| PC2, PC3, PD5 | Unused | They get the pull-up |
| PD7 | Power-bank enable | Unused here. The stock firmware gives it the pull-up and writes its OUT low |
| PF6 | nRST | Input only on this chip (DS40002413B §3.1) |
| PF7 | UPDI | The programming pin |
| VDD | MCU supply | The cell, through a P-channel FET for reverse polarity |
| VDDIO2 | Supply for PC1 to PC3 | Present on programmer power (`MVIO.STATUS` reads 1). What feeds it is unknown, see [The boost enable](#the-boost-enable-pc1) |

That table, PA1's row aside, is confirmed as what the stock firmware's hardware definition says, and nothing more. The pins themselves are confirmed by the datasheet's multiplexing table (DS40002413B §3.1), which has DAC0's output on PD6, TWI0's host-capable SDA and SCL on PA2 and PA3, RESET on PF6 and UPDI on PF7.

The stock firmware's set-up makes PA4 to PA7, PC1 and PD6 outputs with `DIR`, clears PC1's `OUT`, gives PD4 a both-edges pin interrupt, turns PD6's input buffer off, sets the DAC's reference to 1.024 V, enables the DAC and its output buffer with the data at 0, turns TCA0, TCB0, TCB1 and TCD0 off, then sets up the AW2016. Confirmed from source.

PA1 is where the build on this torch parts from the source. The stock source's board definition has nothing on it and gives it the pull-up, but the 0284 build this torch shipped with does neither. A disassembly of its flash (`backup/stock/flash.hex`, gitignored) shows it writing 0xF2 to `VPORTA.DIR` in the reset path, which makes PA1 an output as well as PA4 to PA7, and never sets PA1's `OUT`, so it's held low. `cargo xtask peek` on 2026-11-16, with that build asleep, read `VPORTA.DIR` as 0xE2, PA1 still an output. Confirmed on this torch, from the backup and the peek. What PA1 connects to on the X1-C is unknown (on the plain Lume X1 it's the aux red LED). This firmware does what the stock build did, an output at 0 with `OUT` written before `DIR`, no pull-up, and never high. The other unused pins (PA0, PC2, PC3, PD5 and PD7) get the pull-up, as the stock firmware has them.

### The boost enable, PC1

PC1 raises the boost converter and the amplifier together. Before firmware drives it, after any reset, every output is tri-stated (DS40002413B §18.3.1), and that floating enable lasts at least as long as `SUT` on every power-up, with nothing firmware can do about it. This torch's `SUT` is 0 ms (see [Fuses](#fuses)), so the window is short, but the BOD is off too, so a slowly rising or bouncing cell can start the chip below its safe range. Nothing public says whether the board holds the enable low. The stock firmware never gives PC1 a pull-up (the line's commented out). Confirmed from source.

PC1 is one of the three pins on the VDDIO2 supply. The datasheet's power-domain figure puts PC1 to PC3 on VDDIO2 and every other port pin on VDD (DS40002413B §5.1, figure 5-1), and says that when either domain loses power, "the MVIO I/O pins are tri-stated" (§19.3.2.1). Confirmed for the chip. The stock firmware asks for the `MVSYSCFG` fuse at dual supply (which is also the factory default, and what this torch reads) with the comment "enable MVIO because VDDIO2 pin isn't connected". Confirmed as what the comment says. Those two don't fit together. A PC1 with no supply can't drive anything, and the stock firmware does turn the light on with it. So either VDDIO2 is connected to something after all (VDD, or another rail), or the comment means something else. Nobody has measured the pin. On 2026-11-16 `cargo xtask peek`, with the cell out and the kit's 4.5 V on the pads, read `MVIO.STATUS` as 0x01, so `VDDIO2S` is set and the chip sees VDDIO2 as present (DS40002413B §19.5.3). Confirmed on this torch, on programmer power. So VDDIO2 is connected to something, most likely VDD, and the stock comment means something other than what it says. Whether it's still present on the cell, under load and as the cell sags, is likely and unmeasured. The firmware checks it at power-on (`selftest::Faults::supply`), not after. Which rail feeds it is still an open question.

### Turning on and off

To turn on, the stock firmware raises PC1 if it isn't already high and then waits 8 ms. Then it writes the DAC data and the DAC's reference, and only then sets the power paths (path 3 also raises 2 and 1, path 2 also raises 1). To turn off it zeroes the DAC data, sets the reference back to 1.024 V, clears PC1, then clears all three paths. Confirmed as the stock firmware's order, though not as the only safe one.

That raises the enable with the DAC already at zero and then sets the current, the reverse of the order this repo used on the TS26 (duty first, then enable). Nobody has said which order is safer on this board, on the way on or the way off. Either way, keep the DAC at zero whenever PC1 rises, and end every turn-off with PC1 low, the DAC at zero and every path low, in an order nobody has confirmed yet. Nobody knows what the amplifier does with the DAC at zero, what the 8 ms is waiting for (most likely the boost output settling, but that's a guess), or whether the paths need to be set before or after the current.

### DAC and VREF

DAC0 is 10-bit, and its output is DATA / 1024 of the selected reference (DS40002413B §34.3.2.3). The 10 bits sit at the top of the 16-bit `DAC0.DATA`, so the value goes in shifted left by 6, and the output only changes once the high byte is written (§34.5.2). Confirmed for the chip. The stock firmware switches the DAC's reference between 1.024 V and 2.5 V from level to level, through `VREF.DAC0REF`. Confirmed from source.

From the datasheet (confirmed for the chip, characterised and not production tested where it says so)

- The internal references are ±4 % at 85 °C and below, and the 1.024 V and 2.048 V ones are only specified with VDD at 2.5 V or more, the 2.5 V one at 2.7 V or more (table 38-18). VDD here is the cell, so a nearly flat cell is near the edge of that.
- The DAC's output range is 0.1 V to VDD − 0.1 V, and it can sink 0.001 mA (table 38-23). The datasheet suggests a resistor to ground if the load needs it to sink. So "DAC at zero" is only as low as whatever the amplifier's input and any resistor on the board pull it to, and nobody knows what's there.
- Settling from 0x000 to 0x3FF takes about 7 µs at 3 V (table 38-23).
- In power-down the DAC and its buffer are off (§34.3.3). What PD6 then floats to, and what the amplifier does with it, is unknown. PC1 low is what's meant to keep the light off.

### The power paths

The stock firmware describes three "power paths", each through a different sense resistor, so the same DAC range covers a much wider span of current. Its comment claims a dynamic range "on the order of >10-50 million : 1" and recommends starting the ramp well above the bottom, since the lowest levels are "basically not visible". Confirmed as what the comment says. Its levels pick a path and a reference together for each step. Those tables are the stock firmware's and stay out of this repo, which needs a curve of its own.

How the three paths combine (in parallel, so raising more pins lowers the sense resistance, is the obvious reading) is likely and unmeasured, and so is what a path left on with the boost off does.

## Power and the battery reading

VDD is the cell. The stock firmware's comment reads "read voltage from VDD, PFET RPP, no voltage drop", so the cell reaches VDD through a P-channel FET for reverse polarity protection. Confirmed as what the comment says. So there's no regulator in front of the MCU, and the MCU sees every sag the boost converter pulls out of the cell.

That makes the battery reading a supply reading. The stock firmware reads the ADC's `VDDDIV10` input (VDD divided by 10, DS40002413B §33.5.7) against the 1.024 V reference, so the ADC's top is 10.24 V and a 4.2 V cell sits well inside it. It accumulates 16 samples with `PRESC` DIV4, `SAMPCTRL` 32 and `INITDLY` DLY32. It calibrates with a slope of 1014 ("default = 1024, higher = higher voltage") and an offset of 0. Confirmed from source. How good that calibration is on any given light, nobody has published.

Since VDD is the cell, the MCU browns out with it. VDD's floor is 1.8 V (table 38-4). POR releases at about 1.6 V (table 38-4). The BOD's levels run from 1.9 V to 2.85 V typical (table 38-16), but on this torch the BOD is off ([Fuses](#fuses)), so nothing resets the chip as the cell sags until POR. A cell sagging under turbo pulls the MCU's supply down with it, and the CPU can keep running below the voltage its clock needs. Confirmed for the chip and for this torch's fuses, and reasoning for what happens under load.

The cell is meant to be an unprotected flat-top 21700 rated for at least 15 A continuous, per the JLHawaii808 listing. Nothing between the cell and the boost converter protects it except firmware.

### Low voltage

Nothing in the stock firmware's X1-C build overrides its default low-voltage threshold, so that's its default of 2.90 V. Likely, from reading the build, since the default sits in a file nobody has checked against the build output.

## Temperature

The sensor inside the MCU, on the driver. The stock firmware reads it against the 2.048 V reference, with the factory calibration in `SIGROW.TEMPSENSE0` and `TEMPSENSE1`. Confirmed from source. How it tracks the head, nobody knows.

## The clock

The stock firmware runs the internal high-frequency oscillator at 12 MHz, with the frequency select and `AUTOTUNE` written through the protected-write sequence. Confirmed from source. `AUTOTUNE` tunes the oscillator against an external 32.768 kHz crystal (DS40002413B §12.3.6), and the crystal's pins are PA0 and PA1 (§3.1), which the stock firmware treats as an unused FET path and, in the build on this torch, an output held low. So the board likely has no crystal, and what `AUTOTUNE` does without one, the datasheet doesn't say. This firmware has no reason to set it.

The tick is the RTC's PIT at `CYC512` from the internal 32.768 kHz oscillator, 64 Hz, the same tick the TS26 used. Waking from sleep is PD4's pin change. Confirmed from source.

## The AW2016

The aux LEDs are on an Awinic AW2016, an I²C three-channel LED driver, on TWI0 (PA2 and PA3). Confirmed as what the stock firmware drives. Its comments call its PWM 250 Hz and "obnoxiously slow", so a dim aux visibly strobes, give the lowest current settings as 5, 10, 15 or 30 mA globally with 0 to 15 of 15 per channel, and put aux low at about 0.3 mA. It maps red to the chip's channel 1, green to 0 and blue to 2. Confirmed as what the source says. Awinic's product page lists the same four global currents.

After power-up the AW2016's outputs are off, and Awinic's product page says it runs a pattern of its own when its CHRG pin is pulled high with I²C halted. What CHRG is wired to here, nobody knows. The AW2016 sits on the cell rail (likely, since there's nothing else to power it), so its standby current is drawn whatever the MCU does, and nobody has measured it on this board.

This firmware leaves the AW2016 alone for now and never addresses it, so the aux stays dark. Driving it needs a TWI host driver, which is new work.

## The switch

The e-switch is in the tail, on PD4 through an external 100 kΩ pull-up (per the stock firmware's comment). How a tail switch reaches a pin on a driver in the head isn't public. A tube or a sleeve in the body carrying the switch line is the usual way, and that's a guess for this light.

PA4 drives a "switch LED" in the stock build. On a light with the switch in the tail, what that pin reaches is unknown, and it might be nothing.

## The flashing pads

On this torch the pads are labelled `+ - U` on the board, the JLHawaii808 kit's pins mapped onto them, and their pitch fits the kit's pogo probe. Confirmed on this torch. Nobody has measured the pitch in millimetres. The chip answered with signature `1E 95 3A`, silicon revision 1.2, and avrdude reported "NVM type 2: 24-bit, word oriented write". It was asleep running the stock firmware and took the NVM programming key. One link attempt in the session failed with "UPDI link initialization failed", likely the probe shifting, and worked after re-seating. A second read of flash matched the first byte for byte. Lock bits read `5C C5 C5 5C`, unlocked. All confirmed on this torch.

Before that session, no public photograph of a KR1K's pads had been found. The [reflash reference](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942) lists Hank's AVR32DD20 Lume X1 as "Gen3 Emisar/Noctigon, 3:0 pinout", the common three-pad UPDI layout, which thefreeman labels `+ - R` at 1.27 mm pitch. This board labels the UPDI pad `U` rather than `R`. The [Unofficial Guide](https://anduril.click/drivers/updi.html) warns that pad order differs between lights, so another KR1K's pads still want measuring before a probe goes on.

`U` is UPDI, on PF7 (confirmed on this torch, since the chip answered through it). On this chip that's not the RESET pin, which is PF6 and has no pad in the three-pad layout. PF7's absolute maximum is VDD + 0.3 V like any ordinary pin (DS40002413B table 38-1). Confirmed for the chip. So no high-voltage pulse belongs on that pad, and a high-voltage programmer couldn't reach RESET through it anyway. [flashing.md](flashing.md#only-the-plain-adapter-goes-on-this-torch) has the rest.

`+` is very likely VDD, which is the cell rail, since the programmer powered the chip through it with the cell out. So with the cell out, a programmer's supply on `+` feeds the MCU and very likely whatever else sits on the cell rail, the boost converter's input and the AW2016 included (reasoning, unmeasured). thefreeman says 3.3 V programmers "isn't very reliable" on AVR32DD20 boost drivers, and his 4.5 V programmer has extra capacitance for inrush ([thread](https://budgetlightforum.com/t/wts-usb-updi-programmers-for-attiny1616-avr32dd20-drivers-sofirn-wurkkos-fireflylite-emisar/71459)). No public report has been found of a Lume X1 or X1-C being bricked by a reflash. A D3AA, a sibling AVR32DD20 boost light, with bad firmware drove its LED hard and starved the adapter. Secondhand, and a different board.

## The MCU

An AVR32DD20. From `vendor/AVR32DD20.atdf` (confirmed)

- 32 KB of flash at 0 in 512-byte pages, all of it mapped into data space at 0x8000
- 4 KB of SRAM at 0x7000
- 256 bytes of EEPROM at 0x1400
- fuses at 0x1050, the user row at 0x1080, the signature `1E 95 3A`
- 36 interrupt vectors, with the RTC's PIT at vector 6

Its NVM controller writes flash a word at a time without a page buffer, and UPDI addresses it with 24-bit addresses, flash at 0x800000. pymcuprog calls this NVM version P:2, and avrdude called it "NVM type 2: 24-bit, word oriented write" on this torch. 16-bit registers go low byte first, and `CPU.CCP` guards the clock and other protected registers, as on the TS26's chip.

The errata (DS80001026D §2.2.3) say a store to an address of 64 or more, followed straight away by a store to an address under 64 or to `SLPCTRL.CTRLA`, loses the second write, on every silicon revision so far. The fix is a `nop` between them, or `out` for the low addresses. The VPORTs live under 64, so this bites the pin writes that matter most here. Confirmed for the chip. avrdude reported silicon revision 1.2 on this torch. The errata name their revisions A1 to A3, and how avrdude's 1.2 maps onto those letters isn't stated, so treat every erratum as live.

Register addresses and bit values in `src/regs.rs` are generated from Microchip's ATDF in [`vendor/`](../vendor/README.md) at build time.

### Fuses

The stock firmware's ELF carries a fuse section, which its comment says is "for enabling BOD in Active Mode from the factory". It sets `BODCFG` to the BOD on continuously in active mode, and `SYSCFG1` to `MVSYSCFG` dual and `SUT` 64 ms, and leaves `WDTCFG`, `OSCCFG`, `SYSCFG0`, `CODESIZE` and `BOOTSIZE` at their defaults. Confirmed as what the source asks for. None of it was programmed on this torch.

This torch's fuses read `00 00 00 FF FF D0 08 00 00`, in the order `WDTCFG`, `BODCFG`, `OSCCFG`, two reserved, `SYSCFG0`, `SYSCFG1`, `CODESIZE`, `BOOTSIZE`. Confirmed on this torch. Those are the factory defaults (DS40002413B §8.8). The BOD is off (`BODCFG` 0), `OSCCFG` selects the internal high-frequency oscillator, `SYSCFG0` 0xD0 has UPDI on PF7, PF6 as an input and no CRC check at boot, and `SYSCFG1` 0x08 is dual supply with `SUT` at 0 ms. So the stock source's BOD and 64 ms start-up never reached this board, and with the BOD off and VDD being the cell, a sagging cell takes the MCU down with no brown-out reset ([safety.md](safety.md)).

`SYSCFG0.UPDIPINCFG` decides whether PF7 is UPDI or GPIO at start-up, and its factory value is UPDI (§8.8.2.4). Writing it to GPIO turns UPDI off for good unless a high-voltage pulse (VDD + 2 V at least, 8.5 V at most) reaches the RESET pin on PF6 (§36.3.2.1.2, table 38-29), which the three-pad layout doesn't bring out. So a bad fuse write costs this torch. This repo never writes fuses.

## Reviews

[TG Reviews](https://tgreviews.com/2026/06/05/noctigon-kr1k/) tested a pre-production KR1K (an NTG50 build, 5 June 2026). 2,400 lm at turn-on on turbo, stepping down straight away. Low-voltage protection dims and then shuts off. Drain off was 30 µA, 220 µA with the aux on low blue, 400 µA on low white and 17 mA on high white. Six RGB aux emitters sit under the optic. Confirmed as what one reviewer measured on one pre-production light, which might not be the X1-C or this emitter.

## Open questions

- What feeds VDDIO2? It reads present on programmer power. Does it stay present on the cell, under load, and does anything hold the boost enable if it drops?
- Does anything on the board pull PC1 low through reset, and through a power-up with the BOD off?
- What does the amplifier do with the DAC at zero, and does PD6 need the board to pull it down? What does PD6 float to in power-down?
- Which order is safe on this board, enable then current or current then enable, and what's the 8 ms for?
- How do the three paths combine, and what does a path left on with the boost off do?
- Which PCB code does this board carry? It shipped with the 0284 (32 W) build, so is it the 32 W board, or the 6 V board the listing describes running the other build?
- What's the pads' pitch in millimetres? Does `+` feed the boost converter and the AW2016 as well as the MCU with the cell out?
- What's the AW2016's standby current, and what's its CHRG pin wired to?
- How does the tail switch reach PD4, and what does PA4 drive, if anything?
- Is there a 32 kHz crystal on PA0 and PA1? (Likely not, and the stock build drives PA1 low.) What is PA1 wired to?
- Which errata letter is avrdude's silicon revision 1.2?
- What does the 1014 calibration read against a multimeter, near 3 V and under load?
