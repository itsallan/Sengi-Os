//! Smithay reports problems through `tracing`. Without a subscriber they
//! vanish, so this prints them on stderr: warnings and errors by default,
//! more with `SENGI_COMPOSITOR_LOG=info|debug|trace`.

use std::fmt::{Debug, Write as _};

use tracing::{
    field::{Field, Visit},
    level_filters::LevelFilter,
    span, Event, Level, Metadata, Subscriber,
};

/// The environment variable that sets how much is printed.
pub const VARIABLE: &str = "SENGI_COMPOSITOR_LOG";

pub fn parse_level(name: &str) -> Option<Level> {
    match name {
        "error" => Some(Level::ERROR),
        "warn" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

struct Stderr {
    level: Level,
}

impl Subscriber for Stderr {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= self.level
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.level.into())
    }

    fn event(&self, event: &Event<'_>) {
        let mut line = Line(String::new());
        event.record(&mut line);
        let metadata = event.metadata();
        eprintln!(
            "sengi-compositor: {} {}:{}",
            metadata.level(),
            metadata.target(),
            line.0
        );
    }

    // Spans are not printed, so one id serves them all.
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        let _ = if field.name() == "message" {
            write!(self.0, " {value:?}")
        } else {
            write!(self.0, " {}={value:?}", field.name())
        };
    }
}

pub fn init() {
    let level = std::env::var(VARIABLE)
        .ok()
        .and_then(|name| parse_level(&name))
        .unwrap_or(Level::WARN);
    // Fails only if one is set already, which nothing else here does.
    let _ = tracing::subscriber::set_global_default(Stderr { level });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_names_are_read() {
        assert_eq!(parse_level("trace"), Some(Level::TRACE));
        assert_eq!(parse_level("warn"), Some(Level::WARN));
        assert_eq!(parse_level("loud"), None);
        // More talkative levels compare greater.
        assert!(Level::WARN <= Level::DEBUG);
    }
}
