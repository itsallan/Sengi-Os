//! What the indicator shows, and how it is passed from one run of the
//! program to the one that has the panel open.

/// What a level is the level of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Volume,
    Brightness,
    Keyboard,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Volume => "volume",
            Kind::Brightness => "brightness",
            Kind::Keyboard => "keyboard",
        }
    }

    pub fn parse(word: &str) -> Option<Kind> {
        [Kind::Volume, Kind::Brightness, Kind::Keyboard]
            .into_iter()
            .find(|kind| kind.word() == word)
    }
}

/// One thing to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub kind: Kind,
    /// 0 to 100.
    pub percent: u32,
    /// Only for volume: the sound is off, whatever the level.
    pub muted: bool,
}

impl Level {
    pub fn new(kind: Kind, percent: u32, muted: bool) -> Level {
        Level {
            kind,
            percent: percent.min(100),
            muted: muted && kind == Kind::Volume,
        }
    }

    /// The Phosphor Bold icon for it.
    pub fn icon(&self) -> &'static str {
        match self.kind {
            Kind::Volume if self.muted => "speaker-x",
            Kind::Volume => "speaker-high",
            Kind::Brightness => "sun",
            Kind::Keyboard => "keyboard",
        }
    }

    /// How full the bar is. A muted sound shows an empty bar.
    pub fn bar(&self) -> u32 {
        if self.muted {
            0
        } else {
            self.percent
        }
    }

    /// The line that carries it from one run to another.
    pub fn encode(&self) -> String {
        format!(
            "{} {} {}",
            self.kind.word(),
            self.percent,
            u8::from(self.muted)
        )
    }

    /// The reverse of [`Level::encode`]. Anything else is not a level.
    pub fn decode(line: &str) -> Option<Level> {
        let mut words = line.split_whitespace();
        let kind = Kind::parse(words.next()?)?;
        let percent: u32 = words.next()?.parse().ok()?;
        let muted = match words.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        words
            .next()
            .is_none()
            .then(|| Level::new(kind, percent, muted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_survives_the_trip() {
        for level in [
            Level::new(Kind::Volume, 45, false),
            Level::new(Kind::Volume, 0, true),
            Level::new(Kind::Brightness, 100, false),
            Level::new(Kind::Keyboard, 33, false),
        ] {
            assert_eq!(Level::decode(&level.encode()), Some(level));
        }
    }

    #[test]
    fn what_is_not_a_level_is_refused() {
        for line in [
            "",
            "volume",
            "volume 50",
            "volume x 0",
            "volume 50 2",
            "volume 50 0 extra",
            "loudness 50 0",
            "volume -5 0",
        ] {
            assert_eq!(Level::decode(line), None, "{line:?}");
        }
    }

    #[test]
    fn levels_are_held_to_a_hundred_and_only_sound_mutes() {
        assert_eq!(Level::new(Kind::Volume, 500, false).percent, 100);
        assert!(!Level::new(Kind::Brightness, 50, true).muted);
    }

    #[test]
    fn each_kind_has_its_icon_and_a_muted_sound_an_empty_bar() {
        assert_eq!(Level::new(Kind::Volume, 40, false).icon(), "speaker-high");
        let muted = Level::new(Kind::Volume, 40, true);
        assert_eq!((muted.icon(), muted.bar()), ("speaker-x", 0));
        assert_eq!(Level::new(Kind::Brightness, 40, false).icon(), "sun");
        assert_eq!(Level::new(Kind::Keyboard, 40, false).icon(), "keyboard");
        assert_eq!(Level::new(Kind::Brightness, 40, false).bar(), 40);
    }
}
