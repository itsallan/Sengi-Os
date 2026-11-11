//! What a layer surface looks like to the compositor: where it sits, how big
//! it is and whether it takes the keyboard.

use std::fmt;
use std::ops::{BitOr, BitOrAssign};

/// Which layer of the desktop the surface is drawn in, from back to front.
/// Normal windows sit between `Bottom` and `Top`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    /// Behind everything, where a wallpaper goes.
    Background,
    /// Above the wallpaper, below windows.
    Bottom,
    /// Above windows. Bars and docks live here.
    #[default]
    Top,
    /// Above everything, fullscreen windows included. For launchers and
    /// anything else that must not be covered.
    Overlay,
}

/// The screen edges a surface is attached to. Combine edges with `|`.
///
/// No edges centres the surface. One edge centres it along that edge. Two
/// opposite edges let it stretch between them, see [`Config::size`].
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Anchor(u8);

impl Anchor {
    /// Attached to nothing: centred on the output.
    pub const NONE: Anchor = Anchor(0);
    /// The top edge.
    pub const TOP: Anchor = Anchor(1);
    /// The bottom edge.
    pub const BOTTOM: Anchor = Anchor(2);
    /// The left edge.
    pub const LEFT: Anchor = Anchor(4);
    /// The right edge.
    pub const RIGHT: Anchor = Anchor(8);

    /// True if every edge in `other` is also in `self`.
    pub const fn contains(self, other: Anchor) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Anchor {
    type Output = Anchor;

    fn bitor(self, other: Anchor) -> Anchor {
        Anchor(self.0 | other.0)
    }
}

impl BitOrAssign for Anchor {
    fn bitor_assign(&mut self, other: Anchor) {
        self.0 |= other.0;
    }
}

impl fmt::Debug for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [
            (Anchor::TOP, "TOP"),
            (Anchor::BOTTOM, "BOTTOM"),
            (Anchor::LEFT, "LEFT"),
            (Anchor::RIGHT, "RIGHT"),
        ];
        let set: Vec<&str> = names
            .iter()
            .filter(|(edge, _)| self.contains(*edge))
            .map(|(_, name)| *name)
            .collect();
        if set.is_empty() {
            f.write_str("NONE")
        } else {
            f.write_str(&set.join(" | "))
        }
    }
}

/// Distance kept between the surface and the edges it is anchored to, in
/// logical pixels. A margin on an edge the surface is not anchored to does
/// nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Margins {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

impl Margins {
    /// The same margin on all four sides.
    pub const fn all(margin: i32) -> Margins {
        Margins {
            top: margin,
            right: margin,
            bottom: margin,
            left: margin,
        }
    }
}

/// Whether the surface receives key presses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyboardMode {
    /// Never. The right choice for a bar or a dock.
    #[default]
    None,
    /// Like a normal window: it gets the keyboard when the user clicks it
    /// and loses it when they click elsewhere.
    OnDemand,
    /// It holds the keyboard for as long as it is shown. The right choice
    /// for a launcher. Only honoured on the `Top` and `Overlay` layers.
    Exclusive,
}

/// Which outputs get a surface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Outputs {
    /// One surface on whichever output the compositor picks, normally the
    /// one with the pointer or the focused window. This is not a "primary
    /// monitor": Wayland has no such thing. If that output goes away the
    /// same component moves to another, or waits for one to appear.
    #[default]
    Auto,
    /// One surface on every output, each with its own instance of the
    /// component. The build closure runs once per output, again whenever an
    /// output is plugged in, and the instance is dropped when its output
    /// goes away.
    All,
    /// One surface on the output with this name, such as `eDP-1`. The
    /// instance is built when that output is there and dropped when it
    /// goes; while it is absent there is no instance at all.
    Named(String),
}

impl Outputs {
    /// The old name of [`Outputs::Auto`].
    #[deprecated(note = "renamed to Outputs::Auto; the compositor picks the output")]
    #[allow(non_upper_case_globals)]
    pub const Primary: Outputs = Outputs::Auto;
}

/// Everything the compositor needs to know to place a surface.
///
/// Sizes and margins are in logical pixels. Build one with [`Config::new`]
/// and the chained setters:
///
/// ```
/// use sengi_layer::{Anchor, Config, Layer};
///
/// let bar = Config::new("sengi-bar")
///     .layer(Layer::Top)
///     .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
///     .size(0, 28)
///     .exclusive_zone(28);
/// assert!(bar.validate().is_ok());
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Tells the compositor what kind of surface this is, so its rules can
    /// match it. Use the program name.
    pub namespace: String,
    pub layer: Layer,
    pub anchor: Anchor,
    /// Width and height. Zero means "stretch along this axis", which needs
    /// the surface anchored to both edges of that axis.
    pub size: (u32, u32),
    pub margins: Margins,
    /// How much of the screen edge to reserve so windows do not cover the
    /// surface. 0 reserves nothing but still avoids other surfaces' zones;
    /// -1 ignores other surfaces' zones too.
    pub exclusive_zone: i32,
    pub keyboard: KeyboardMode,
    pub outputs: Outputs,
}

impl Config {
    /// A centred 1 x 1 surface on the top layer with no keyboard. Set at
    /// least the size before using it.
    pub fn new(namespace: impl Into<String>) -> Config {
        Config {
            namespace: namespace.into(),
            layer: Layer::default(),
            anchor: Anchor::NONE,
            size: (1, 1),
            margins: Margins::default(),
            exclusive_zone: 0,
            keyboard: KeyboardMode::default(),
            outputs: Outputs::default(),
        }
    }

    pub fn layer(mut self, layer: Layer) -> Config {
        self.layer = layer;
        self
    }

    pub fn anchor(mut self, anchor: Anchor) -> Config {
        self.anchor = anchor;
        self
    }

    /// Width and height in logical pixels; 0 stretches along that axis.
    pub fn size(mut self, width: u32, height: u32) -> Config {
        self.size = (width, height);
        self
    }

    pub fn margins(mut self, margins: Margins) -> Config {
        self.margins = margins;
        self
    }

    pub fn exclusive_zone(mut self, zone: i32) -> Config {
        self.exclusive_zone = zone;
        self
    }

    pub fn keyboard(mut self, keyboard: KeyboardMode) -> Config {
        self.keyboard = keyboard;
        self
    }

    pub fn outputs(mut self, outputs: Outputs) -> Config {
        self.outputs = outputs;
        self
    }

    /// Checks the combinations a compositor would answer with a protocol
    /// error, which kills the program.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_size(self.anchor, self.size)
    }
}

pub(crate) fn validate_size(
    anchor: Anchor,
    (width, height): (u32, u32),
) -> Result<(), ConfigError> {
    if width == 0 && !anchor.contains(Anchor::LEFT | Anchor::RIGHT) {
        return Err(ConfigError::WidthNeedsLeftAndRight);
    }
    if height == 0 && !anchor.contains(Anchor::TOP | Anchor::BOTTOM) {
        return Err(ConfigError::HeightNeedsTopAndBottom);
    }
    // The protocol carries sizes and margins as 32-bit signed numbers.
    if width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(ConfigError::TooLarge);
    }
    Ok(())
}

/// A surface configuration the compositor would reject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Width 0 means "stretch", which needs both `LEFT` and `RIGHT`.
    WidthNeedsLeftAndRight,
    /// Height 0 means "stretch", which needs both `TOP` and `BOTTOM`.
    HeightNeedsTopAndBottom,
    /// A size that does not fit the protocol.
    TooLarge,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ConfigError::WidthNeedsLeftAndRight => {
                "a width of 0 needs the surface anchored to both the left and right edges"
            }
            ConfigError::HeightNeedsTopAndBottom => {
                "a height of 0 needs the surface anchored to both the top and bottom edges"
            }
            ConfigError::TooLarge => "the surface size is too large",
        })
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_stretches_between_the_edges_it_is_anchored_to() {
        let bar = Config::new("bar")
            .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
            .size(0, 28);
        assert_eq!(bar.validate(), Ok(()));
    }

    #[test]
    fn stretching_without_both_edges_is_rejected() {
        let config = Config::new("x").anchor(Anchor::TOP | Anchor::LEFT);
        assert_eq!(
            config.clone().size(0, 28).validate(),
            Err(ConfigError::WidthNeedsLeftAndRight)
        );
        assert_eq!(
            config.size(200, 0).validate(),
            Err(ConfigError::HeightNeedsTopAndBottom)
        );
        assert_eq!(
            Config::new("x").size(0, 0).validate(),
            Err(ConfigError::WidthNeedsLeftAndRight)
        );
    }

    #[test]
    fn a_centred_surface_with_a_fixed_size_is_fine() {
        assert_eq!(Config::new("launcher").size(640, 48).validate(), Ok(()));
        let fullscreen = Config::new("scrim")
            .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
            .size(0, 0);
        assert_eq!(fullscreen.validate(), Ok(()));
    }

    #[test]
    fn sizes_beyond_the_protocol_are_rejected() {
        let config = Config::new("x").size(u32::MAX, 10);
        assert_eq!(config.validate(), Err(ConfigError::TooLarge));
    }

    #[test]
    fn anchors_combine_and_print() {
        let mut anchor = Anchor::TOP | Anchor::LEFT;
        assert!(anchor.contains(Anchor::TOP));
        assert!(!anchor.contains(Anchor::TOP | Anchor::RIGHT));
        anchor |= Anchor::RIGHT;
        assert!(anchor.contains(Anchor::LEFT | Anchor::RIGHT));
        assert_eq!(format!("{anchor:?}"), "TOP | LEFT | RIGHT");
        assert_eq!(format!("{:?}", Anchor::NONE), "NONE");
    }
}
