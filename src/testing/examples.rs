//! What the torch does, as scripts and how the light ends up. `cargo test`
//! plays each on the library, and `cargo xtask hex` on the firmware too.

use super::{Action, parse};
use Expect::{Level, Off};

/// How the light should be on an example's last tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Off,
    Level(u8),
}

impl Expect {
    pub const fn holds(self, level: u8) -> bool {
        match self {
            Self::Off => level == 0,
            Self::Level(want) => level == want,
        }
    }
}

/// A script, played on a torch that's just had its cell put in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Example {
    pub name: &'static str,
    pub script: &'static str,
    pub expect: Expect,
}

impl Example {
    pub fn actions(&self) -> impl Iterator<Item = Action> + use<> {
        parse(self.script.split_whitespace())
            .map(|action| action.expect("every script in EXAMPLES parses"))
    }
}

const fn ex(name: &'static str, script: &'static str, expect: Expect) -> Example {
    Example {
        name,
        script,
        expect,
    }
}

pub const EXAMPLES: &[Example] = &[
    ex("Off: 1C", "1C", Level(52)),
    ex("Off: 1C, after using it", "1C 1H 1C 1C", Level(52)),
    ex("Off: 3C", "3C 1C", Off),
    ex("Off: 4C or more", "5C 1C", Off),
    ex("Off: 1H", "1H", Level(1)),
    ex("Off: 1H, held for a minute", "1H:60s", Level(1)),
    ex("Off: 1H, let go", "1H wait:10s", Level(1)),
    ex("Off: 1H, then 1H", "1H 1H:1s", Level(40)),
    ex("Off: 1H, then 1H twice", "1H 1H:1s 1H:500ms", Level(34)),
    ex("Off: 1H, then 1C", "1H 1C", Off),
    ex("Off: 1C, after 1H", "1H 1C 1C", Level(52)),
    ex("Off: Anything else", "2C 2H:2s wait:1s", Off),
    ex("On: 1C", "1C 1C", Off),
    ex("On: 1C, after ramping", "1C 1H:3s 1C", Off),
    ex("On: 3C", "1C 3C 1C", Level(52)),
    ex("On: 1H", "1C 1H:3s", Level(150)),
    ex("On: 1H again", "1C 1H:3s 1H:3s", Level(1)),
    ex("On: 1H, a third time", "1C 1H:3s 1H:3s 1H:3s", Level(150)),
    ex(
        "On: 1H, after one that stopped partway",
        "1C 1H:1s 1H:500ms",
        Level(85),
    ),
    ex(
        "On: 1H, after turning it off and on",
        "1C 1H:3s 1C 1C 1H:3s",
        Level(150),
    ),
    ex(
        "On: More clicks, or a hold after clicks",
        "1C 1H:1s 2C",
        Off,
    ),
    ex("On: 2C", "1C 2C", Off),
    ex("On: 2C, at the floor", "1H 2C", Off),
    ex("On: 2C, then 1C", "1C 2C 1C", Level(52)),
    ex("On: 4C", "1C 4C 1C", Level(52)),
    ex("On: 2H", "1C 2H:2s", Off),
    ex("Lockout: 3C, from off", "3C", Off),
    ex("Lockout: 3C", "3C 3C 1C", Level(52)),
    ex("Lockout: Any other click, or a hold", "3C wait:10s 2C", Off),
    ex("Lockout: 1H", "3C 1H:2s", Off),
    ex("Lockout: Anything else", "3C 1C 2C 4C 2H:2s wait:1s", Off),
    ex("Low cell", "cell:2.95V 1C wait:6s", Level(37)),
    ex("Low cell, for long enough", "cell:2.95V 1C wait:60s", Off),
    ex("Low cell, 1H", "cell:2.95V 1H wait:3s", Level(1)),
    ex(
        "Low cell, 1H, for long enough",
        "cell:2.95V 1H wait:6s",
        Off,
    ),
    ex("Flat cell", "cell:2.80V 1C wait:10s", Off),
    ex("Flat cell, 1C again", "cell:2.80V 1C wait:10s 1C", Off),
    ex("Flat cell, 1H", "cell:2.80V 1C wait:10s 1H", Off),
    ex(
        "Flat cell, rested but not charged, 1C",
        "cell:2.80V 1C wait:10s cell:3.20V 1C",
        Off,
    ),
    ex(
        "Flat cell, charged, 1C",
        "cell:2.80V 1C wait:10s cell:3.80V 1C",
        Level(52),
    ),
    ex(
        "Flat cell, charged, left alone",
        "cell:2.80V 1C wait:10s cell:3.80V wait:301s",
        Off,
    ),
    ex(
        "A cell that collapses",
        "1C 1H:3s cell:2.70V wait:250ms",
        Off,
    ),
    ex(
        "A collapsed cell, 1C again",
        "1C 1H:3s cell:2.70V wait:250ms 1C",
        Off,
    ),
    ex(
        "A collapsed cell that rests at 3.60 V or more, 1C",
        "1C 1H:3s cell:2.70V wait:250ms cell:3.80V 1C",
        Level(52),
    ),
    ex(
        "A dip under 2.80 V that's gone by the next reading",
        "1C cell:2.70V wait:16ms cell:3.80V wait:10s",
        Level(52),
    ),
    ex("Low cell, at rest", "cell:3.20V wait:1s 1C", Level(52)),
    ex("Low cell, read by a press", "wait:1s cell:3.20V 2C", Off),
    ex(
        "Low cell, charged at rest",
        "cell:3.20V wait:1s cell:3.80V wait:301s",
        Off,
    ),
    ex("A cell it can't read", "cell:0V 1C wait:5s", Off),
    ex(
        "A cell it can read again",
        "cell:0V 1C wait:5s cell:3.80V 1C",
        Off,
    ),
    ex(
        "A cell it can't read, at rest",
        "cell:0V wait:900s cell:3.80V 1C",
        Level(52),
    ),
    ex("An ADC stuck on one reading", "1C adc:stuck wait:5s", Off),
    ex(
        "An ADC stuck on one reading, at rest",
        "adc:stuck wait:900s 1C wait:5s",
        Off,
    ),
    ex("Hot", "heat:80C 1C 1H:3s wait:120s", Level(50)),
    ex(
        "Hot, at a level under 50",
        "heat:80C 1C 1H:500ms 1H:3s wait:120s",
        Level(1),
    ),
    ex(
        "Hot, then cool",
        "heat:80C 1C 1H:3s wait:120s heat:25C wait:120s",
        Level(150),
    ),
    ex(
        "A degree over the limit",
        "1C 1H:3s heat:51C wait:40s",
        Level(111),
    ),
    ex("A failed check at power-on", "fault 1C", Off),
    ex("A failed check at power-on, 1H", "fault 1H", Off),
    ex(
        "Flash that doesn't match its checksum",
        "flash:corrupt 1C",
        Off,
    ),
    ex("VDDIO2 missing at power-on", "vddio2:lost 1C", Off),
    ex(
        "VDDIO2 lost with the light on",
        "1C 1H:3s vddio2:lost wait:250ms",
        Off,
    ),
    ex(
        "VDDIO2 lost, then back",
        "1C vddio2:lost wait:1s vddio2:back 1C",
        Off,
    ),
    ex(
        "VDDIO2 lost with the light off",
        "wait:1s vddio2:lost 1C",
        Off,
    ),
    ex(
        "VDDIO2 lost and back while it rests",
        "wait:1s vddio2:lost wait:1s vddio2:back 1C",
        Level(52),
    ),
];

/// The example called `name`.
pub fn example(name: &str) -> &'static Example {
    EXAMPLES
        .iter()
        .find(|example| example.name == name)
        .unwrap_or_else(|| panic!("no example called {name:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_parses_and_has_its_own_name() {
        for (i, example) in EXAMPLES.iter().enumerate() {
            let mut words = parse(example.script.split_whitespace()).peekable();
            assert!(words.peek().is_some(), "{}: no script", example.name);
            if let Some(error) = words.find_map(Result::err) {
                panic!("{}: {error}", example.name);
            }
            assert!(
                EXAMPLES[..i].iter().all(|other| other.name != example.name),
                "two examples are called {:?}",
                example.name
            );
        }
    }

    #[test]
    fn checks_the_light() {
        assert!(Expect::Off.holds(0) && !Expect::Off.holds(1));
        assert!(Expect::Level(50).holds(50) && !Expect::Level(50).holds(51));
    }
}
