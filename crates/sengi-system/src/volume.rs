//! The default output's volume, through `wpctl`, and the queue that keeps
//! a dragged slider from starting a process for every pixel.

/// The slider's range. PipeWire can go louder; the panel does not.
pub const MAX_PERCENT: u32 = 100;

/// The default output's volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    /// In percent. PipeWire allows more than 100.
    pub percent: u32,
    /// Whether the output is muted.
    pub muted: bool,
}

/// Parses `wpctl get-volume` output, for example `Volume: 0.40 [MUTED]`.
pub fn parse(output: &str) -> Option<Volume> {
    let rest = output.trim().strip_prefix("Volume:")?.trim();
    let mut parts = rest.split_whitespace();
    let level: f32 = parts.next()?.parse().ok()?;
    if !level.is_finite() {
        return None;
    }
    Some(Volume {
        // Far above anything real, but it keeps the arithmetic in range.
        percent: (level * 100.0).round().clamp(0.0, 1000.0) as u32,
        muted: parts.any(|part| part == "[MUTED]"),
    })
}

/// The arguments for `wpctl` that set the volume of `target`, which is a
/// node's number or one of wpctl's names for a default. `-l 1` holds it at
/// 100% whatever is asked for.
pub fn volume_args(target: &str, percent: u32) -> [String; 5] {
    [
        "set-volume".to_string(),
        "-l".to_string(),
        "1".to_string(),
        target.to_string(),
        format!("{}%", percent.min(MAX_PERCENT)),
    ]
}

/// The arguments for `wpctl` that mute or unmute `target`.
pub fn mute_args(target: &str, muted: bool) -> [String; 3] {
    [
        "set-mute".to_string(),
        target.to_string(),
        if muted { "1" } else { "0" }.to_string(),
    ]
}

/// The arguments for `wpctl` that set the default output's volume.
pub fn set_volume_args(percent: u32) -> [String; 5] {
    volume_args(crate::commands::SINK, percent)
}

/// The arguments for `wpctl` that mute or unmute the default output.
pub fn set_mute_args(muted: bool) -> [String; 3] {
    mute_args(crate::commands::SINK, muted)
}

/// A change asked for by a slider, numbered so its echo can be told from
/// a stale one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// Counts up with every change one slider asks for.
    pub sequence: u64,
    /// The value asked for, in percent.
    pub value: u32,
}

/// Of everything that queued up while the last change was being applied,
/// only the newest matters: the slider has moved on from the rest.
pub fn newest(queued: impl IntoIterator<Item = Step>) -> Option<Step> {
    queued.into_iter().max_by_key(|step| step.sequence)
}

/// The UI's side of a slider that is set through a slow worker. Readings
/// come back numbered with the last change the worker had applied when it
/// took them; one taken before the slider's latest move is out of date and
/// must not drag the knob back.
#[derive(Debug, Default)]
pub struct Echo {
    sent: u64,
}

impl Echo {
    /// Number the next change.
    pub fn next(&mut self, value: u32) -> Step {
        self.sent += 1;
        Step {
            sequence: self.sent,
            value,
        }
    }

    /// Whether a reading that saw changes up to `sequence` is current.
    pub fn current(&self, sequence: u64) -> bool {
        sequence >= self.sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_is_rounded_to_percent() {
        assert_eq!(
            parse("Volume: 0.40\n"),
            Some(Volume {
                percent: 40,
                muted: false
            })
        );
        assert_eq!(parse("Volume: 0.335").unwrap().percent, 34);
        assert_eq!(parse("Volume: 1.20").unwrap().percent, 120);
    }

    #[test]
    fn muted_volume() {
        let volume = parse("Volume: 0.65 [MUTED]\n").unwrap();
        assert!(volume.muted);
        assert_eq!(volume.percent, 65);
    }

    #[test]
    fn nonsense_is_not_a_volume() {
        assert_eq!(parse("Could not connect to PipeWire"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("Volume: loud"), None);
        assert_eq!(parse("Volume: NaN"), None);
        assert_eq!(parse("Volume: inf"), None);
        assert_eq!(parse("Volume: -3").unwrap().percent, 0);
        assert_eq!(parse("Volume: 1e30").unwrap().percent, 1000);
    }

    #[test]
    fn the_volume_command_is_capped() {
        assert_eq!(set_volume_args(40)[4], "40%");
        assert_eq!(set_volume_args(400)[4], "100%");
        assert_eq!(set_volume_args(40)[3], "@DEFAULT_AUDIO_SINK@");
        assert_eq!(set_mute_args(true)[2], "1");
        assert_eq!(set_mute_args(false)[2], "0");
    }

    #[test]
    fn a_node_is_addressed_by_its_number() {
        assert_eq!(
            volume_args("46", 40),
            ["set-volume", "-l", "1", "46", "40%"]
        );
        assert_eq!(volume_args("46", 400)[4], "100%");
        assert_eq!(mute_args("46", true), ["set-mute", "46", "1"]);
    }

    fn steps(values: &[u32]) -> Vec<Step> {
        let mut echo = Echo::default();
        values.iter().map(|&value| echo.next(value)).collect()
    }

    #[test]
    fn a_drag_collapses_to_its_last_position() {
        let drag = steps(&[40, 41, 43, 47, 52, 58, 61]);
        assert_eq!(
            newest(drag),
            Some(Step {
                sequence: 7,
                value: 61
            })
        );
        assert_eq!(newest(Vec::new()), None);
        // Whatever order they are looked at in.
        let mut reversed = steps(&[10, 20, 30]);
        reversed.reverse();
        assert_eq!(newest(reversed).unwrap().value, 30);
    }

    #[test]
    fn stale_readings_do_not_move_the_slider() {
        let mut echo = Echo::default();
        // Something else changed the volume before the slider was touched.
        assert!(echo.current(0));
        let first = echo.next(50);
        let second = echo.next(60);
        // The worker applied the first and reports what it then read.
        assert!(!echo.current(first.sequence));
        assert!(!echo.current(0));
        assert!(echo.current(second.sequence));
    }
}
