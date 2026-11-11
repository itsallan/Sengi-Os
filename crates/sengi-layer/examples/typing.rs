//! A centred text field that holds the keyboard, to try keyboard input and
//! the runtime changes by hand or from a test.
//!
//! Type, and the surface grows a second row that counts the characters.
//! Tab hides the surface for a second. Escape prints the text and quits.
//!
//!     cargo run -p sengi-layer --example typing
//!
//! With `--fill` the surface covers the whole output and is transparent
//! around the box, the way a launcher that closes on a click outside has
//! to be built. It is also the costly case for drawing.
//!
//!     cargo run -p sengi-layer --example typing -- --fill
//!
//! F1 switches between the two while it runs.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use sengi_layer::{Anchor, Config, KeyboardMode, Layer};
use slint::{ComponentHandle, Timer};

slint::slint! {
    import { Tokens, SengiSurface, SengiLabel, SengiText } from "../../../ui/components.slint";

    export component Typing inherits Window {
        in property <bool> fill;
        callback dismissed();
        callback toggled();
        callback blinked();
        callback edited();
        out property <string> text <=> input.text;
        // Read by the program to size the layer surface.
        out property <length> surface-width: Tokens.launcher-width;
        out property <length> row-height: Tokens.bar-height + Tokens.space-xl;
        out property <length> surface-height: input.text == "" ? root.row-height : root.row-height * 2;

        background: transparent;
        default-font-family: Tokens.font-family;
        forward-focus: input;

        SengiSurface {
            raised: true;
            // On a surface that covers the output the box sits in the
            // middle; otherwise it is the surface.
            width: root.fill ? root.surface-width : parent.width;
            height: root.fill ? root.surface-height : parent.height;

            VerticalLayout {
                padding-left: Tokens.space-md;
                padding-right: Tokens.space-md;

                HorizontalLayout {
                    height: root.row-height;
                    spacing: Tokens.space-md;

                    SengiLabel {
                        text: "Type";
                        vertical-alignment: center;
                    }

                    FocusScope {
                        horizontal-stretch: 1;
                        key-pressed(event) => {
                            if event.text == Key.Escape {
                                root.dismissed();
                                return accept;
                            }
                            if event.text == Key.F1 {
                                root.toggled();
                                return accept;
                            }
                            if event.text == Key.Tab {
                                root.blinked();
                                return accept;
                            }
                            reject
                        }

                        input := TextInput {
                            color: Tokens.color-text;
                            selection-background-color: Tokens.color-accent;
                            selection-foreground-color: Tokens.color-accent-text;
                            font-size: Tokens.font-size-lg;
                            single-line: true;
                            vertical-alignment: center;
                            edited => { root.edited(); }
                        }
                    }
                }

                if input.text != "": SengiText {
                    height: root.row-height;
                    text: input.text.character-count + " characters";
                    color: Tokens.color-text-muted;
                    vertical-alignment: center;
                }
            }
        }
    }
}

fn pixels(length: f32) -> u32 {
    length.round().max(1.0) as u32
}

fn main() -> Result<(), sengi_layer::Error> {
    let everywhere = Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
    let fill = Rc::new(Cell::new(
        std::env::args().any(|argument| argument == "--fill"),
    ));
    let mut config = Config::new("sengi-layer-typing")
        .layer(Layer::Overlay)
        .keyboard(KeyboardMode::Exclusive);
    if fill.get() {
        config = config.anchor(everywhere).size(0, 0);
    }

    sengi_layer::run(config, move |context| {
        let typing = Typing::new()?;
        let handle = context.handle();
        let surface = context.surface();
        typing.set_fill(fill.get());

        // The box grows with what is typed. Covering the output, the
        // surface stays as it is and the box grows inside it.
        let width = pixels(typing.get_surface_width());
        if !fill.get() {
            surface.set_size(width, pixels(typing.get_surface_height()))?;
        }
        typing.on_edited({
            let fill = fill.clone();
            let surface = surface.clone();
            let weak = typing.as_weak();
            move || {
                if let (false, Some(typing)) = (fill.get(), weak.upgrade()) {
                    let _ = surface.set_size(width, pixels(typing.get_surface_height()));
                }
            }
        });

        typing.on_toggled({
            let fill = fill.clone();
            let handle = handle.clone();
            let weak = typing.as_weak();
            move || {
                let Some(typing) = weak.upgrade() else {
                    return;
                };
                fill.set(!fill.get());
                typing.set_fill(fill.get());
                let _ = if fill.get() {
                    handle.set_anchor_and_size(everywhere, 0, 0)
                } else {
                    let height = pixels(typing.get_surface_height());
                    handle.set_anchor_and_size(Anchor::NONE, width, height)
                };
            }
        });

        // Said on stderr so a test can follow along.
        surface.on_focus_changed(|focused| eprintln!("typing: focus {focused}"));
        surface.on_output_changed(|output| eprintln!("typing: on {output:?}"));

        typing.on_blinked({
            let handle = handle.clone();
            move || {
                handle.hide();
                let handle = handle.clone();
                Timer::single_shot(Duration::from_secs(1), move || handle.show());
            }
        });

        let weak = typing.as_weak();
        typing.on_dismissed(move || {
            if let Some(typing) = weak.upgrade() {
                println!("{}", typing.get_text());
            }
            handle.quit();
        });
        Ok(typing)
    })
}
