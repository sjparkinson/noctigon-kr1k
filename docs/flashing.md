# Flashing

`cargo xtask` has a UPDI programmer built in. It runs on macOS and Linux, and sets up the serial port with `stty`. Nobody has used it on a KR1K yet. The torch's pads have been found and its stock firmware read with avrdude 8.3 (`-c serialupdi -p avr32dd20`) through the JLHawaii808 kit, with the cell out and nothing written (2026-11-16, [hardware.md](hardware.md#the-flashing-pads)). Everything below about `xtask` on the torch is still a plan.

## Only the plain adapter goes on this torch

Only a non-high-voltage UPDI adapter ever goes on the KR1K's pads, and only at the 4.5 V the JLHawaii808 kit supplies. Adafruit's High Voltage UPDI Friend never does, nor any other 12 V programmer, and that includes trying to recover a torch that's stopped talking. There's no exception for a torch that's already dead. If UPDI's gone, the torch is gone.

On this chip there's no reason to reach for one anyway. The UPDI pad (`U` on this board) is PF7, which is rated to VDD + 0.3 V like any ordinary pin (DS40002413B table 38-1). The high-voltage override goes on the RESET pin, PF6, which the three-pad layout doesn't bring out, and wants at least VDD + 2 V and at most 8.5 V there, against a 9 V absolute maximum (§36.3.2.1.2, table 38-29). The Friend's 12 V on PF7 is far outside its rating, and wouldn't do the job if it weren't.

An earlier torch, a Wurkkos TS26, was lost on 2026-11-06 after the Friend was hand-held on its pads, and it then measured about 2.5 Ω between all three. The likely cause is 12 V or a slipped probe, and neither is proven. It's the reason this rule exists, and it's not evidence about this board.

Both adapters are CH340s and show up as USB ID `1a86:7523`, with the same kind of port name, so nothing on the computer tells you which one you've plugged in. Look at the one in your hand, and keep the Friend unplugged and off the bench whenever the torch is open.

## Never write fuses

Nothing in this repo writes fuses, and nothing should. `SYSCFG0.UPDIPINCFG` set to GPIO turns PF7 into an ordinary pin at start-up (DS40002413B §8.8.2.4), and the only way back is the high-voltage pulse on PF6 above, which this torch never gets. The programmer writes flash and EEPROM, and never fuses, the user row or a locked chip.

## What you need

[JLHawaii808's kit](https://jlhawaii808.com/products/updi-programmer-driver-reflashling-kit), the 4.5 V version, with its pogo probe. It's a CH340 serial UPDI adapter with USB-C, and it's what the owner has. thefreeman says 3.3 V programmers aren't very reliable on AVR32DD20 boost drivers, and his 4.5 V programmer has extra capacitance for inrush ([BLF thread](https://budgetlightforum.com/t/wts-usb-updi-programmers-for-attiny1616-avr32dd20-drivers-sofirn-wurkkos-fireflylite-emisar/71459)). Don't use the kit's 3.3 V version here.

Watch the pad spacing before you rely on the fixed probe. On this torch the kit's pogo probe fits the pads, which are labelled `+ - U` (confirmed on this torch, not measured in millimetres). Another KR1K might not match, some boards use another pitch or another order, and the TS26 that was lost had pads about 1.75 mm apart against a written-down 1.27 mm. So check the fit on any other board before trusting the probe. One that nearly fits sits on the edges of the pads, which is how it slips. Loose pogo leads work whatever the layout. Use a short USB cable too; JLHawaii808 notes failures with 1 m ones.

## The pads

Where the KR1K's pads are, and what's likely about them, is in [hardware.md](hardware.md#the-flashing-pads). On this torch they're labelled `+ - U`, the kit's probe fits them, and the chip answered through them. One link attempt failed with "UPDI link initialization failed" and worked after re-seating the probe, so a failure to link is a reason to lift and re-seat, not to push harder.

Take the cell out first, every time. VDD on this driver is the cell, so the programmer's supply on `+` powers the MCU directly, and very likely the boost converter's input and the AW2016 too, since they all hang off the cell rail (reasoning, unmeasured). With a cell in, the programmer and the cell would fight over the same rail. Don't touch the switch while the probe's on.

Then name all three pads with a multimeter before anything touches them.

1. Continuity from each pad to the battery negative contact. Exactly one should be ground. If none is, stop.
2. Ohms from each of the other two to ground, watching how the reading moves. The supply pad has the board's capacitors on it, so it should start low and climb as the meter charges them. The UPDI pad has only PF7 on it, whose pull-up is inside the chip and only works powered (DS40002413B §8.8.2.4), so it should read very high from the start. That's a prediction from the datasheet. If the two don't differ clearly, stop.
3. Ohms between each pair of pads, both ways round, written down as the baseline. A few ohms between all three is a short, and that means stop.

Pad order isn't the same on every light, and a probe the wrong way round puts the adapter's 4.5 V on PF7 with the chip unpowered, which is outside its rating. If any naming is in doubt, stop.

Make the first contact in a dark room, watching the emitter, with a finger on the adapter. The stock firmware starts when the programmer powers the chip, and it might light the emitter as it does. Light that stays on, or warmth in the adapter, is a stop. Hold the probe with something that can't slip rather than your hand, if you can.

## Find the port

Run `ls /dev/cu.*` before and after plugging the adapter in. On macOS it's something like `/dev/cu.usbserial-1410` or `/dev/cu.wchusbserial1410`. On Linux it's usually `/dev/ttyUSB0`, and you may need to be in the `dialout` group.

## Check it's talking

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
```

This reads the chip's System Information Block and writes no memory, but don't count on it leaving a running chip alone. `ping` finishes by writing UPDIDIS, "upon which the UPDI will issue a System Reset" (DS40002413B §36.3.2.2.2). So expect the firmware to restart as `ping` ends. Nobody has seen what this torch says to `xtask`'s `ping`. avrdude read signature `1E 95 3A`, silicon revision 1.2 and "NVM type 2" from it, and pymcuprog calls that NVM version P:2, so expect something with `P:2` in it. If you don't get an answer, see [If it doesn't talk](#if-it-doesnt-talk).

## Back up the factory firmware first

Do this before you flash anything. It's the only copy of Hank's build and its fuses that you'll have.

This torch already has one. avrdude read it on 2026-11-16 into `backup/stock/` (gitignored), and a second read of flash matched the first byte for byte. It holds the stock firmware's 0284 build, and the fuses are factory defaults with the BOD off ([hardware.md](hardware.md#fuses)). Keep it somewhere safe as well. An `xtask` backup into `backup/torch` is still worth taking before the first flash, since it adds the `serial.hex` that `restore` checks.

```sh
cargo xtask backup /dev/cu.usbserial-XXXX backup/torch
```

That checks the chip's System Information Block says NVM version P:2, puts the chip in programming mode, checks its signature, and saves `flash.hex` (all 32 KB), `eeprom.hex`, `fuses.hex` (16 bytes from 0x1050), `lock.hex` (LOCK.KEY, 4 bytes from 0x1040), `userrow.hex` and `serial.hex` (SERNUM0 to SERNUM15) into the directory you name. The fuses, lock bits and user row are there for reference only, and nothing writes them back. The first five are the same memories avrdude's `backup/stock/` files of those names hold, so compare the decoded bytes, not the text. avrdude writes 32-byte records and leaves off flash's trailing 0xFF bytes, where `backup` writes 16-byte records and every byte. The serial number is the chip's own, from its signature row, and it's how `restore` knows whose backup it's been given. It refuses to run if that directory already has a `flash.hex`, so a second run can't overwrite your factory copy.

If it warns that the serial number can't tell one chip from another (all FF or all 00), the backup is saved all the same, but `restore` will refuse it. Stop there, before anything goes over the factory firmware.

The directory holds one chip's backup. The path is used as given, so run the command from the repo root for it to land in the repo's `backup/`, which is git-ignored. Copy it somewhere safe too.

## The first flash is a first-light image

The first thing that goes on the KR1K is a first-light image built for the AVR32DD20, and nothing else. It doesn't exist yet. It'll be the smallest firmware that turns the light on and off at one low level through PC1 and the DAC, with as little as possible in it to hang, so a light that works shows the pads, the programmer, the enable, the DAC and a path all do what this repo thinks they do. The full firmware only goes on once that's proven on the light, which is stage 3 of [breakout.md](breakout.md#stage-3-first-light-on-the-torch).

The repo's history has a first-light image for the TS26 at commit `9d3b7dc`. It's built for the ATtiny1616, and it must never go on this torch.

## Flash this firmware

```sh
cargo xtask flash /dev/cu.usbserial-XXXX
```

That builds the firmware, refuses it if the simulator finds a mistake, and then

1. refuses a chip whose System Information Block doesn't say NVM version P:2, before any key goes out, then puts the chip in programming mode and checks its signature (`1E 95 3A`), refusing anything else
2. chip-erases it, which wipes flash and, unless the EESAVE fuse is set, EEPROM
3. writes the image a 512-byte page at a time, skipping blank pages (the erase already left them blank), each page erased again and then written by the word. The image ends in a stamp of the commit and a CRC the firmware checks at power-on
4. reads it all back and compares
5. resets the chip and turns UPDI off, so the new firmware starts

If a write or the compare fails, step 5 still runs, so the chip isn't left in programming mode. Run the command again.

Step 1 is the only check on the chip before the erase, and the signature only says it's an AVR32DD20. `flash` never looks at the fuses, so read `backup/torch/fuses.hex` yourself first. `flash` with a file refuses a name without `kr1k` or `0283` in it, but that's the name and nothing more. Step 4 only says the bytes arrived, and nothing about whether they run.

A flash doesn't power-cycle the chip, so take the probe off and leave it a few seconds before putting the cell in, so VDD really collapses.

If the light goes dark after a flash that verified, run `cargo xtask peek /dev/cu.usbserial-XXXX` before anything else. It reads the running chip's registers and SRAM, which shows how far the firmware got, and finishes with the same reset as `ping`, so only the first run shows anything. If it talks, flash the first-light image again. A light that comes back points at the firmware, and one that doesn't points at the board.

## Going back

`restore` puts a backup's flash and EEPROM back, and only onto the chip they were read from. Before it erases anything it reads the chip's serial number and compares it with the backup's `serial.hex`, and it refuses on a mismatch, a missing `serial.hex` or a serial number that's blank. It writes every EEPROM byte, blank ones included, one erase-write at a time, since with EESAVE set the old contents survive the erase.

```sh
cargo xtask restore /dev/cu.usbserial-XXXX backup/torch
```

The check says which chip a backup came from and nothing about which build it holds, so read the directory in the command before you run it.

To go back to the stock firmware without a backup, download its latest release (the [README](../README.md#acknowledgements) links the project) and take the Lume X1-C build for model 0284, which is what this torch shipped with (its flash holds `0284.0.653607d`, [hardware.md](hardware.md#which-kr1k-this-is)). Save it under a name with `kr1k` or `0283` in it, since `flash` refuses any other name. The KR1K has no build of its own, and the backup in `backup/stock/` is the better way back. This project hasn't flashed one onto a KR1K.

```sh
cargo xtask flash /dev/cu.usbserial-XXXX path/to/stock-kr1k-0284.hex
```

## If it doesn't talk

- "no reply from the chip" means the adapter heard itself and the chip said nothing, which usually means poor contact or the wrong pad on UPDI. It's also what you get with the probe in the air. Reseat the probe or check the pads again, and don't lean on it.
- "nothing echoed back" with the probe in the air means the adapter isn't joining TX and RX, so it isn't a serial UPDI adapter.
- "nothing echoed back" with the probe on the pads, from an adapter that says "no reply" in the air, is the bad one. Something's pulling the UPDI line down so hard the adapter can't hear its own bytes, which means the probe's on the ground pad or there's a short on the board. Take the probe off, check the pad order, and measure the ohms against the numbers you wrote down. The Friend still stays off.
- The anduril.click [UPDI guide](https://anduril.click/drivers/updi.html) and the [reflash reference](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942) cover more adapters and their quirks. Their AVR32DD20 commands use `avrdude -p avr32dd20 -c serialupdi`, which is a second opinion if `xtask` and the chip disagree. Don't use any of their fuse commands.

## How it works

The programmer writes flash and EEPROM, and only those. That's all this torch needs, and it keeps fuses out of reach.

`xtask/src/updi.rs` is the protocol, covering link set-up, the NVM programming key, reset, flash writes through the NVM controller and reads in blocks. The AVR DD's controller writes flash a word at a time with no page buffer and erases it in 512-byte pages, and UPDI reaches flash at 0x800000 with 24-bit addresses (DS40002413B chapters 11 and 36). No separate UPDI programming specification for the AVR DD turned up from Microchip, so the sequences follow pymcuprog, and the registers come from the ATDF. `xtask/src/serial.rs` opens the port, configures it with `stty` (115200 baud, 8 data bits, even parity, 2 stop bits) and swallows the echo that a one-wire adapter sends back.

The tests run all of it against a pretend chip in `updi.rs`. That catches slips in framing and sequencing, but it was written from the same reading of the datasheet as the programmer, so the first run against a real chip is the real test. Start with `ping`, then `backup`.
