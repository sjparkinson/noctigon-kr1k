# The UI

What every click and hold does. This is the spec. `src/ui.rs` implements it and its tests check it, so change this file in the same commit as the code.

The notation is Anduril's. **1C** is one click, **2C** two quick clicks, **1H** press and hold, **2H** click then press and hold, and so on. A press becomes a hold after about 375 ms, and clicks count once the switch has been left alone for about 220 ms. When more clicks wouldn't mean anything, the last click counts as it's let go, with no wait. That's 1C while it's on, and the third click of 3C while it's off. Lockout always waits, so you unlock with a pause after 3C.

Every row has an example, which is a script in that notation, starting from a torch that's just had its battery put in, and how the light should be after it (`off`, `on` or `level N`). A hold lasts a second unless it says otherwise (`1H:3s`), and `wait:1s` leaves the switch alone. A script that ends in a hold never lets go, so it shows the light while it's still held. `cargo test` runs the examples against the library, so a wrong one fails. Only Planned rows can leave it as `-`. The background table at the end is the exception, since it isn't clicks.

## Off

| Action | Does | Example | Status |
|---|---|---|---|
| 1C | On, at level 52 | `1C` → level 52 | Done |
| 1C, after using it | On at level 52 again, since nothing's remembered | `1C 1H 1C 1C` → level 52 | Done |
| 3C | Lockout, as the third click's let go | `3C 1C` → off | Done |
| 4C or more | Lockout, since 3C counts without waiting, and the clicks after it count in lockout, where they do nothing | `5C 1C` → off | Done |
| Anything else | Nothing | `2C 1H:2s 2H:2s wait:1s` → off | Done |

## On

| Action | Does | Example | Status |
|---|---|---|---|
| 1C | Off, as it's let go | `1C 1C` → off | Done |
| 1H | Ramp up, stopping at the top | `1C 1H:3s` → level 120 | Done |
| 1H again | Ramp the other way from the last 1H, stopping at the end | `1C 1H:3s 1H:3s` → level 13 | Done |
| 1H, after one that stopped partway | Still the other way, so letting go and holding again turns the ramp round | `1C 1H:1s 1H:500ms` → level 84 | Done |
| 1H, after turning it off and on | Up, since nothing's remembered | `1C 1H:3s 1C 1C 1H:3s` → level 120 | Done |
| More clicks, or a hold after clicks | The first click turns it off without waiting, and the rest count from off. So 2C is off and back on at level 52, 4C is off and locked, and 2H is off and then a hold that does nothing | `1C 1H:1s 2C` → level 52 | Done |
| 4C | Off, then locked | `1C 4C 1C` → off | Done |
| 2H | Off, then nothing | `1C 2H:2s` → off | Done |

The ramp moves one level per 64 Hz tick, so bottom to top takes about 1.7 seconds. Each 1H goes the other way from the one before it, whether or not that one reached the end, and the first after a click on goes up, since the light comes on near the bottom.

The top is `ramp::CEILING`, level 120 of 150, and nothing goes above it until thermal regulation has been checked on a real light. On this firmware's curve that's about half the current of level 150, which is the stock firmware's top on this torch's 32 W build, so about half the stock power. The lowest level is 13, the same number as the TS26's, which here is the low power path at a DAC code of 92. The levels under it are only for the low-voltage steps to fade through. There's no PWM on the main LED, so nothing here flickers by design, but nobody's seen the bottom levels on a real light, and none of the levels has been measured in lumens, amps or watts. The path gains behind the curve are estimates too, so expect a small step or dip where it changes path (`src/ramp.rs`).

The aux LEDs on this torch hang off an AW2016 LED driver, which the firmware drives over I²C. Every aux colour on this page has lit on the simulator's fake AW2016. On 2026-11-16, with the firmware at `c4f9cd4`, dim blue when off, bright red in lockout and dark while lit all showed as expected on this torch. Yellow and white haven't been seen there.

## Lockout

For pockets and bags. The light never comes on.

| Action | Does | Example | Status |
|---|---|---|---|
| 3C, from off | Lock, with the aux bright red for five seconds | `3C` → off | Done |
| 3C | Unlock, back to off | `3C 3C 1C` → level 52 | Done |
| Any other click, or a hold | Nothing, and the aux bright red for five seconds from the click or the start of the hold | `3C wait:10s 2C` → off | Done |
| 1H | Nothing | `3C 1H:2s` → off | Done |
| Anything else | Nothing | `3C 1C 2C 4C 2H:2s wait:1s` → off | Done |

## When something's wrong

The light looks after the cell and itself whatever you've asked for. It never flashes to say so. Heat and a low cell take it down gently, and the level you asked for comes back once it can. A flat cell or a fault puts it out and keeps it out, and the aux LEDs say which.

These scripts have six more words. `cell:2.95V` is the cell's voltage from there on and `heat:80C` the temperature. `cell:0V`, like any voltage outside 1.00 to 4.40 V, is a cell the firmware can't read. `adc:stuck` sticks the ADC on the cell's reading as it stands, for everything it reads. `fault` starts a torch whose power-on checks failed (on the simulated chip, a fuse that isn't what the firmware expects), and `flash:corrupt` one whose flash doesn't match its checksum. `vddio2:lost` is the chip saying VDDIO2, the supply its PC1 pin is on, is missing from there on, and `vddio2:back` that it's there again. `cargo test` runs these against the library, and `cargo xtask hex` runs them against the firmware on a simulated chip. Every threshold behind them is unmeasured on this light.

| When | Does | Example | Status |
|---|---|---|---|
| Low cell | Steps down | `cell:2.95V 1C wait:6s` → level 37 | Done |
| Low cell, for long enough | Steps down, then off, as a flat cell | `cell:2.95V 1C wait:60s` → off | Done |
| Flat cell | Fades down the ramp, then off, with the aux yellow for three seconds | `cell:2.80V 1C wait:10s` → off | Done |
| Flat cell, 1C again | Nothing, and the aux yellow for three seconds | `cell:2.80V 1C wait:10s 1C` → off | Done |
| Flat cell, rested but not charged, 1C | Nothing | `cell:2.80V 1C wait:10s cell:3.20V 1C` → off | Done |
| Flat cell, charged, 1C | On as usual | `cell:2.80V 1C wait:10s cell:3.80V 1C` → level 52 | Done |
| Flat cell, charged, left alone | No longer flat at the next check of the cell, within five minutes, so off's blue comes back with no press | `cell:2.80V 1C wait:10s cell:3.80V wait:301s` → off | Done |
| A cell that collapses | Off at once, from any level, with no fade, at the second reading in a row under 2.80 V with the light on, which is within a fifth of a second. Then it's a flat cell, with the aux yellow for three seconds | `1C 1H:3s cell:2.70V wait:250ms` → off | Done |
| A collapsed cell, 1C again | Nothing, and the aux yellow for three seconds, as for any flat cell | `1C 1H:3s cell:2.70V wait:250ms 1C` → off | Done |
| A collapsed cell that rests at 3.60 V or more, 1C | On as usual, at level 52 | `1C 1H:3s cell:2.70V wait:250ms cell:3.80V 1C` → level 52 | Done |
| A dip under 2.80 V that's gone by the next reading | Nothing | `1C cell:2.70V wait:16ms cell:3.80V wait:10s` → level 52 | Done |
| Low cell, at rest | Off's dim blue goes dark at the first reading under 3.30 V with the light off, which is a few ticks after power-on or within five minutes of rest. The light still comes on | `cell:3.20V wait:1s 1C` → level 52 | Done |
| Low cell, read by a press | Blue goes dark with the press, with no wait for a check | `wait:1s cell:3.20V 2C` → off | Done |
| Low cell, charged at rest | Blue again at the first reading of 3.40 V or more with the light off, within five minutes. One between the two changes nothing | `cell:3.20V wait:1s cell:3.80V wait:301s` → off | Done |
| A cell it can't read | Fades down the ramp, then off, as a fault | `cell:0V 1C wait:5s` → off | Done |
| A cell it can read again | Still a fault | `cell:0V 1C wait:5s cell:3.80V 1C` → off | Done |
| A cell it can't read, at rest | Nothing. The aux stays as it was, and it's no fault until a press reads it | `cell:0V wait:900s cell:3.80V 1C` → level 52 | Done |
| An ADC stuck on one reading | Fades down the ramp, then off, as a fault, at the second reading of ground that isn't near zero, which is within about half a second | `1C adc:stuck wait:5s` → off | Done |
| An ADC stuck on one reading, at rest | Nothing until a press, which reads ground once before its click counts, so a 1C lights at level 52 and fades out from the next reading | `adc:stuck wait:900s 1C wait:5s` → off | Done |
| Hot | Fades down, no further than level 50 | `heat:80C 1C 1H:3s wait:120s` → level 50 | Done |
| Hot, at a level under 50 | Nothing | `heat:80C 1C 1H:500ms 1H:3s wait:120s` → level 13 | Done |
| Hot, then cool | Back up to the level you asked for | `heat:80C 1C 1H:3s wait:120s heat:25C wait:120s` → level 120 | Done |
| A failed check at power-on | A fault | `fault 1C` → off | Done |
| Flash that doesn't match its checksum | The same as any failed check | `flash:corrupt 1C` → off | Done |
| VDDIO2 missing at power-on | The same as any failed check | `vddio2:lost 1C` → off | Done |
| VDDIO2 lost with the light on | Off on that tick, with no fade, as a fault | `1C 1H:3s vddio2:lost wait:250ms` → off | Done |
| VDDIO2 lost, then back | Still a fault | `1C vddio2:lost wait:1s vddio2:back 1C` → off | Done |
| VDDIO2 lost with the light off | A fault at the next press, before its click counts, so nothing lights | `wait:1s vddio2:lost 1C` → off | Done |
| VDDIO2 lost and back while it rests | Nothing, since nothing looks at rest | `wait:1s vddio2:lost wait:1s vddio2:back 1C` → level 52 | Done |

A flat cell is one that's gone under 2.90 V at level 1, or under 3.00 V for long enough that the steps down reach level 1, or one that's collapsed (below). After that the light stays off, and clicks and holds do nothing but ask for the aux yellow for three seconds. A press reads the cell before its click counts, with the light off, and if that reading is 3.60 V or more the click turns the light on as if the torch were new. That's a cell that's been charged or swapped, since one that cut out at level 1 rests only a little above where it cut, and a rested cell at 3.3 V is close to empty. It's one reading because a press only has time for one. The check every five minutes at rest (below) takes the same reading, so a flat cell that's been charged is good again within five minutes with no press, and off's blue comes back to say so. A charger lifts the cell's voltage while it runs, so a cell on charge can pass 3.60 V before it's taken much. So USB charging brings the light back without taking the cell out, and a flat cell can't be run at level 1 again with every click.

A cell that collapses is one that reads under 2.80 V twice running with the light on, and it's one of two things that put the light out with no fade, VDDIO2 going missing (below) being the other. Everything above is slow on purpose, a filter and then a hold of a second or more, which suits a cell running down and is far too slow for one that falls off a cliff under load. VDD is the cell, and with the BOD off nothing resets the chip as it sags, so a cell that falls far enough takes the MCU down with the driver still on. So while the light's on the firmware reads the cell on seven ticks running in every sixteen, not one, and the second reading in a row under 2.80 V turns the light off there and then, from whatever level it was at. Off, not dimmed, since off is the only state the driver's known to be safe in if the chip does go on to brown out. After that it's a flat cell like any other. VDDIO2 going missing also puts the light out at once, as a fault, and if the two come together the fault wins, so charging the cell won't bring it back. A cell that rests at 3.60 V or more, which is one with charge in it that couldn't hold up the level you asked for, lights again at the next click, at level 52. One that rests lower stays dark until it's charged.

That narrows the window. It doesn't close it, and it's no brown-out detector. The seven readings come a tick apart and then the cell goes unread for ten ticks while the ADC is on the temperature, so the light's out within twelve ticks of the cell going under, 188 ms, and a cell that falls and takes the chip with it inside that gets through. One low reading alone does nothing, so a dip that's gone by the next reading isn't seen either. The two conversions that run as the light comes on don't count, since each catches a load landing on the cell, and that isn't the cell collapsing. The reading is against a reference that's ±4 %, so 2.80 V read could be anything from about 2.69 to 2.91 V, and like every threshold here it's unmeasured. Nobody knows how fast this cell and this boost converter can fall.

Every five minutes at rest, and on its first tick after the battery goes in, the torch reads the cell with the light off, the way a press does. That's five passes of the loop, with an idle rather than power-down between each and the next, so it stays out of power-down for four ticks, about 60 ms. The cell's reading is in on the third pass, and on the fifth there's a reading of VDDIO2, the supply the driver's enable hangs off, which the firmware keeps for the programmer to read (`cargo xtask peek`) and does nothing else with. A press or the light being on starts the five minutes again, since the cell's read then anyway, and a fault skips them. That reading decides off's dim blue. Under 3.30 V it goes dark, and it stays dark until a reading with the light off is 3.40 V or more, so a cell resting on the line doesn't flick it on and off. A press reads it too, before its click counts. Readings with the light on don't count, since the cell sags under load, and nor does one that comes in within two ticks of it going out. It's only the aux. The light still comes on with a low cell, and the cell's own thresholds above decide how far. A reading at rest that can't be real changes nothing. Each tick of rest forgets the ones before it, so they never add up to a fault at rest, and a dead ADC at rest goes unnoticed until a press reads it four times a second.

A fault is a failed check, a cell it can't read, or VDDIO2 going missing, and it lasts until the cell comes out. The light won't come on, and clicks and holds ask for the aux white for three seconds. A cell it can't read with the light on fades it down the ramp, a level a tick, so it doesn't snap out in your hand. VDDIO2 going missing doesn't fade.

A failed check is flash that doesn't match its checksum, a fuse that isn't what the firmware expects, VDDIO2 missing at power-on (PC1, the driver's enable, hangs off it and can't be driven without it), or the watchdog having reset the chip. So one watchdog reset leaves the torch dark until the cell comes out.

VDDIO2 is looked at again on every tick the torch is awake. That's every tick the light's on, every tick of a press or of a click being counted, and the five passes of each check of the cell. If the chip says it's missing on any of those, the light is out on that tick, from whatever level, and it's a fault from then on, even if VDDIO2 comes back. There's no fade because the chip lets go of PC1 without VDDIO2, so the pin that would hold the driver on through a fade is the one that's gone, and the current setpoint and the power paths are all the firmware has left to turn down. From VDDIO2 going to those being off is a tick and a pass of the loop at most, about 16 ms, a couple more if the aux's bus is stuck, and about 26 ms if the tick runs as slow as the firmware allows for. A dip shorter than a tick can go unseen. At rest nothing looks, since there's nothing lit to put out. So a supply that goes and comes back while the torch is off and left alone changes nothing, and one that stays gone is a fault at the next press, before its click counts, or at the next check of the cell. One reading is enough, since it isn't a measurement that wanders but the chip saying PC1 has already let go. That look is a yes or no from the chip. The VDDIO2 reading each check takes for the programmer is separate, a voltage that nothing in the firmware acts on. The datasheet doesn't say how low VDDIO2 gets before the chip says so, only that 1.62 V and up is in range. On a chip whose fuses say single supply, the chip always says VDDIO2 is there, so none of this does anything (this torch's say dual). It's only run on the simulator, and nobody has seen it on the torch, either working or tripping when it shouldn't.

At rest the torch slows down. After a second with the light off, no click counting, no colour on a timer and the next check of the cell more than a second away, the tick goes from 64 a second to one, and the watchdog to an 8 s timeout, fed every four seconds by a trip back to 64 Hz for two ticks. A slow tick is a tick of rest like any other, so it doesn't look at VDDIO2 either. A press wakes it at once, and that pass reads the switch on the press rather than up to a second later, so a press is no easier to miss than at 64 Hz, likely harder. The tick goes back to 64 Hz with the press, every tick from then on looks at VDDIO2 as above, and the light's watchdog window is back two ticks later, long before a click can count or a hold start (`watchdog::Pacer`). The first tick after the switch from 1 Hz can come anything up to a tick early or late, so the first tick of a press can be short or long by that much. The check of the cell every five minutes goes back to 64 Hz a second before it's due, and its ticks look at VDDIO2 as ever. None of this has run on the torch, only on the simulator, and what it saves there is an estimate (docs/safety.md, H4).

A cell it can't read is a voltage under 1.00 V or over 4.40 V, which is also what an ADC that never answers looks like. Three readings in a row like that go, and the fourth, about a second in, is a fault. With no reading there's no low-voltage cut-off, and this cell has no protection of its own, so a torch that can't read its cell doesn't light.

An ADC that's stuck on one reading can't read the cell either, however good that reading looks, and a stuck 3.8 V would otherwise keep a flat cell lit until it took the chip down with it. So while the torch is awake, with the light on or a press being counted, it also reads the ADC's ground input once a round, four times a second, straight after the temperature. A working ADC reads that as near zero, 0.4 % of full scale at most, and a cell never reads anywhere near that low. Two in a row that aren't near zero, a quarter of a second apart, are a fault, as for a cell it can't read. That catches an ADC that's stuck, or one reading the wrong input. It can't catch one that's wrong and still moving, or a reference that's drifted, since ground reads near zero against any reference. The checks at rest don't read ground, so garbage at rest still changes nothing, and a stuck ADC at rest goes unnoticed until a press or the light reads it, like a dead one. A press gets one reading of ground in before its click counts, and it takes two, so a 1C on a stuck ADC lights the torch for about a quarter of a second before the fault fades it.

## Always on, in the background

| What | Does | Status |
|---|---|---|
| Aux LEDs | Steady, dim blue when off, but dark while the cell last read under 3.30 V with the light off, until it reads 3.40 V or more. Bright red in lockout, for five seconds after a press. Out while the main LED's lit. Dim yellow for a flat cell and dim white for a fault, for three seconds after a press or after the light goes out. Dark otherwise. Never flashing. The button LED stays off. A colour shows a tick after it's asked for, or two from a state the firmware doesn't know, since the AW2016 wakes a tick before it takes one. Simulator only, never on the torch | Done |
| Sleep when off | Powers down and looks at the switch 64 times a second, then once a second after a second with nothing to do, and wakes on a press at either. A press at 1 Hz is seen as it happens, and clicks and holds count just as at 64 Hz. Simulator only, never on the torch | Done |
| Cell check when off | Stays out of power-down for four ticks after every five minutes of rest, give or take a second or so for the 1 Hz tick, and on the first tick after the battery goes in, to read the cell, then VDDIO2 for the programmer, with the light off. Not with a fault | Done |
| Cell watch when on | Reads the cell on seven ticks running in every sixteen, 28 times a second, where off and pressed it's four. Only the four feed the steps down and the fade. The rest are for a cell that collapses | Done |
| ADC check when awake | Reads the ADC's ground input four times a second while the light's on or a press is being counted, and calls two in a row that aren't near zero a fault. Not at rest | Done |
| A tick that stops | With the light on, off within about a tenth of a second, and it stays off until the watchdog resets the chip or the cell comes out | Done |

The aux colours say which of off and lockout you're in, so a click that does nothing doesn't look like a dead torch. TG Reviews measured a pre-production KR1K on the stock firmware at 30 µA off with the aux dark, 220 µA with aux low in blue, 400 µA low in white and 17 mA high in white, and the stock firmware's notes put AW2016 aux low at about 0.3 mA. This firmware's dim blue is a current setting of 1 in 15 on the AW2016's lowest global current, 5 mA, through a PWM code of 64, so at most 84 µA of LED current if the code is at most a quarter of the time (the datasheet calls the curve exponential and gives no formula), plus the AW2016's 100 µA typical awake (150 µA at most). That's about 0.18 mA, 4 to 6 mAh a day at most, or a third of a 5,000 mAh 21700 a year. With the aux off the AW2016 is in standby, 2 µA typical by its datasheet. That's not cheap, but blue isn't timed, since the check at rest bounds the drain instead. It puts blue out once the cell reads under 3.30 V, so a cell left in a drawer stops paying for it there. That cut-off matters more on this torch than most, since VDD is the cell, with no regulator in front of the MCU, and the BOD is off, so a cell run down under what the chip needs takes it down with no reset to catch it on the way. A rested cell at 3.30 V is likely well above that, though nobody's measured where this chip stops behaving, and from there only the torch's own draw off is left on it (30 µA on the stock firmware, under 1 mAh a day). Bright red is 5 mA, the most the 5 mA setting gives, which would empty a 5,000 mAh cell in about six weeks in a pocket, so it only shows for five seconds. Dim red was hard to see on the TS26, which is why lockout's is bright. The AW2016 dims with 250 Hz PWM, which the stock firmware's notes say looks more like strobing than dimming, so how dim the dim colours look, and whether a code of 64 shows at all, is an open question. Nobody's measured any of it on this firmware. The thresholds for blue are unmeasured, and the five minutes between checks are timed by the 32 kHz oscillator, which the datasheet gives no accuracy for. Off and lockout, the two you'll see most, get a channel each. A flat cell is yellow, red and green together, and a fault white, all three. A flat cell and a fault only ask for the aux for three seconds at a time, since a cell that's already flat, or one the torch can't read, shouldn't be drained by the aux. The watchdog reset after a stopped tick is a failed check, so that torch would show white when pressed.

None of this has met a real light yet, so this isn't firmware to carry. See the [roadmap](roadmap.md).
