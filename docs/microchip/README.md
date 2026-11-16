# Microchip references

Microchip's datasheet, errata and app notes that matter for this firmware on the AVR32DD20. The PDFs live in this directory on your machine but aren't in git: each one says "All Rights Reserved" and "No licenses are conveyed", so this public repo can't redistribute them. (The ATDF in `vendor/` is different; it's Apache-2.0.) `.gitignore` covers `*.pdf` here and a `txt/` cache of extracted text.

Each file is named `<document number><revision>-<short title>.pdf`, using the number printed on the document: the AN or TB number for app notes, the DS number for the rest. Microchip's download names are all over the place, so save each one under the name in the table, like this.

```sh
cd docs/microchip
curl -Lo DS40002413B-avr32-16dd20-14-datasheet.pdf https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ProductDocuments/DataSheets/AVR32-16DD20-14-Complete-DataSheet-DS40002413.pdf
```

Then check you've got the same revisions.

```sh
shasum -a 256 -c SHA256SUMS
```

A newer revision will fail the check. That's fine if you want it, but rename the file to its new revision letter, update the table and `SHA256SUMS` in the same commit, and note what changed if it affects anything in `docs/hardware.md`.

Plenty of what's below was written for another family. The AVR DD shares most of its peripherals with the AVR DA and DB, and several (CPUINT, RTC, WDT, CRCSCAN, SLPCTRL) with the tinyAVR 1-series, but not all of them. Where an app note names another part, take the ideas and check every register name and number against DS40002413B and `vendor/AVR32DD20.atdf`.

## The part

| File | Document | What it's for here |
|---|---|---|
| `DS40002413B-avr32-16dd20-14-datasheet.pdf` | [AVR16/32DD14/20 complete data sheet](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ProductDocuments/DataSheets/AVR32-16DD20-14-Complete-DataSheet-DS40002413.pdf) | The reference for every peripheral, timing and electrical limit. The DAC (chapter 34), VREF (21), MVIO (19), TWI (29), ADC (33), NVMCTRL (11) and UPDI (36) chapters matter most for this driver. The older `-Prel-` URL serves the same file |
| `DS80001026D-avr32-16dd20-14-errata.pdf` | [AVR16/32DD14/20 silicon errata and data sheet clarification](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ProductDocuments/Errata/AVR32-16DD20-14-SilConErrataClarif-DS80001026.pdf) | Overrides the datasheet. §2.2.3 (a write to an address under 64 straight after a store to one at 64 or over is lost, and the same for `SLPCTRL.CTRLA`) bites anything that writes the VPORTs or sleeps. §2.2.1 and §2.2.2 are about high-voltage pulses and PF7 |
| `DS40002198C-avr-instruction-set.pdf` | [AVR instruction set manual](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ProductDocuments/ReferenceManuals/AVR-InstructionSet-Manual-DS40002198.pdf) | Instruction encodings and cycle counts (AVRxt column) for reading `cargo xtask hex` output. The older `/en/DeviceDoc/` URL serves revision B |

## App notes

| File | Document | What it's for here |
|---|---|---|
| `AN4875A-getting-started-avr-dd.pdf` | [Getting started with the AVR DD family](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ApplicationNotes/ApplicationNotes/AN4875-Getting-Started-AVR-DD-Family-DS00004875.pdf) | Where Microchip lists the family's datasheets, errata and the technical briefs that apply to it |
| `AN1982B-interrupt-system.pdf` | [The tinyAVR 0/1 interrupt system](https://ww1.microchip.com/downloads/en/AppNotes/40001982B.pdf) | Written for the tinyAVR, and the AVR DD's CPUINT is laid out the same way. For the PIT tick and waking on the switch's pin change on PD4. Check vector numbers against the ATDF, since the AVR DD's 36 vectors aren't the tinyAVR's |
| `AN3341B-avr-da-bootloader.pdf` | [Basic bootloader for the AVR DA family](https://ww1.microchip.com/downloads/en/Appnotes/Basic-Bootloader-for-AVR-MCU-DA-Family-DS00003341B.pdf) | The AVR Dx NVM controller, which writes flash a word at a time with no page buffer, unlike the tinyAVR's. Background for the UPDI programmer and for EEPROM. Microchip publishes no UPDI programming specification for the AVR DD, so datasheet chapters 11 and 36 are the reference |
| `AN3731A-megaavr-to-avr-dx-migration.pdf` | [Migration from megaAVR to AVR Dx](https://ww1.microchip.com/downloads/en/Appnotes/Migration-from-megaAVR-to-AVR-DxMCU-Fam-DS00003731A.pdf) | The TWI in host mode on AVR Dx, for talking to the AW2016, along with the NVM, VREF and clock changes. No AVR Dx technical brief on the TWI turned up |
| `AN2447A-measure-vcc-without-io-pin.pdf` | [Measuring VCC without an I/O pin](https://ww1.microchip.com/downloads/en/AppNotes/00002447A.pdf) | The KR1K's VDD is the cell, so the battery reading is a supply reading. The AVR DD reads it directly through the ADC's VDDDIV10 input (DS40002413B §33.5.7), so this is background on the idea more than the method |
| `AN2515D-low-power-techniques.pdf` | [AVR low-power techniques](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ApplicationNotes/ApplicationNotes/AN2515-AVR-Low-Power-Techniques-00002515.pdf) | Sleeping when off, and what to do with PC1, the DAC and the path pins while asleep. Revision D adds the AVR DA and DB |
| `AN3664A-avr-da-low-power-sleep-modes.pdf` | [AVR DA low-power features and sleep modes](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ApplicationNotes/ApplicationNotes/AVRDA-Low-Power-Features-SleepModes-DS00003664A.pdf) | The AVR Dx sleep controller and what runs in power-down. Written for the DA, so check current figures against DS40002413B |
| `AN2519B-hardware-design-considerations.pdf` | [AVR hardware design considerations](https://ww1.microchip.com/downloads/en/Appnotes/AN2519-AVR-Microcontroller-Hardware-Design-Considerations-00002519B.pdf) | Reading the driver board: decoupling, reset and the UPDI pin. AN4875 points at it for the AVR DD |
| `AN2551C-adc-noise-countermeasures.pdf` | [Noise countermeasures for ADC applications](https://ww1.microchip.com/downloads/en/Appnotes/AN2551-Noise-Countermeasures-for-ADC-Applications-00002551C.pdf) | Why a VDD reading jumps while the boost converter is pulling current from the cell that VDD sits on |
| `AN2572C-adc-oversampling.pdf` | [ADC oversampling with tinyAVR and megaAVR](https://ww1.microchip.com/downloads/en/Appnotes/AN2572-ADC-Oversampling-with-tinyAVR-and-megaAVR-00002672C.pdf) | Accumulating conversions for a steadier battery and temperature reading. Written for a 10-bit ADC; the AVR DD's is 12-bit with its own accumulator. Microchip's file is numbered 00002672C, but the document says AN2572 |
| `AN2632D-iec-60730-class-b.pdf` | [IEC 60730 Class B with the tinyAVR 1-series](https://ww1.microchip.com/downloads/en/Appnotes/IEC60730-ClassB-Compliance-with-tinyAVR1-DS00002632D.pdf) | Self-tests (watchdog in window mode, clock, flash CRC, RAM) for making a hang or a fault fail safe. No AVR Dx version turned up, and the AVR DD's CRCSCAN and WDT are close enough to use the ideas |
| `AN2633B-ulp-timing-32khz-oscillators.pdf` | [Low-power timing with the 32 kHz oscillators](https://ww1.microchip.com/downloads/en/Appnotes/ULPTim-32768kHz-EXT-XTAL-OSC-ULPIntOsc-DS00002633B.pdf) | The RTC and PIT tick for when it sleeps, and how far the internal 32 kHz oscillator wanders. Written for the tinyAVR and megaAVR |
| `AN2747C-robustness.pdf` | [Robustness of tinyAVR 0/1 and megaAVR 0](https://ww1.microchip.com/downloads/en/Appnotes/Robustness-tinyAVR-0-1-megaAVR-0-series-DS00002747C.pdf) | BOD, watchdog and how a chip behaves at its edges, such as a cell sagging under boost load. Measured on other families, so it describes the kind of behaviour and not the AVR DD's numbers |
| `AVR104A-buffered-eeprom-writes.pdf` | [Buffered interrupt-controlled EEPROM writes](https://ww1.microchip.com/downloads/en/Appnotes/doc2540.pdf) (Atmel doc2540) | Old (pre-AVR Dx NVM), so take the ideas, not the register names |
| `TB3209B-getting-started-with-adc.pdf` | [Getting started with the ADC](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ApplicationNotes/ApplicationNotes/TB3209-Getting-Started-with-ADC-DS90003209.pdf) | ADC set-up for VDDDIV10 and the internal temperature sensor. Revision B covers the AVR DA's 12-bit ADC as well as the tinyAVR's |
| `TB3210B-getting-started-with-dac.pdf` | [Getting started with the DAC](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ApplicationNotes/ApplicationNotes/TB3210-Getting-Started-with-DAC-DS-90003210.pdf) | The 10-bit DAC on PD6 that sets the LED current. Revision B covers the AVR DA; revision A was tinyAVR only |
| `TB3287A-getting-started-with-mvio.pdf` | [Getting started with multi-voltage I/O](https://ww1.microchip.com/downloads/en/Appnotes/GettingStarted-MVIO-AVRDB-DS90003287A.pdf) | MVIO, which powers PC1 to PC3 (PC1 is the boost enable) from VDDIO2. Written for the AVR DB, and AN4875 points at it for the AVR DD |

Download links checked against these hashes on 2026-11-16. Microchip's www site blocks scripted downloads, but these `ww1.microchip.com` links work with `curl`.

## Not Microchip's

The AW2016, the I²C RGB driver for the aux LEDs, is Awinic's part. Its datasheet (`DS_AW2016_EN_V1.4`, dated 2026-04-14 on [Awinic's product page](https://www.awinic.com/en/productDetail/AW2016QNR)) has no stable public download link that a script can fetch, and Awinic grants no licence to redistribute it, so it isn't listed here or kept in this directory. Fetch it by hand from that page if you need it, and keep it local.
