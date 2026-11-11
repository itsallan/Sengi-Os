# sengi-layer

Runs Slint components on wlr-layer-shell surfaces. Slint has no layer-shell
backend, so the Sengi OS shell programs (sengi-bar, sengi-dock,
sengi-launcher) share this one. It owns the Wayland connection, puts a Slint
component on a layer surface, feeds it pointer and keyboard input, and copies
software-rendered frames into shared-memory buffers.

## Using it

    [dependencies]
    sengi-layer = { path = "../sengi-layer" }
    slint = { version = "1.18", default-features = false, features = ["compat-1-2", "std"] }

Do not use the workspace `slint` entry: it pulls in the winit backend.
sengi-layer turns on the software renderer, system fonts and path rendering.

A launcher-like client: centred, holds the keyboard, quits on Escape.

    use sengi_layer::{Config, KeyboardMode, Layer};
    use slint::ComponentHandle;

    slint::include_modules!();

    fn main() -> Result<(), sengi_layer::Error> {
        let config = Config::new("sengi-launcher")
            .layer(Layer::Overlay)
            .size(640, 48)
            .keyboard(KeyboardMode::Exclusive);

        sengi_layer::run(config, |context| {
            let launcher = Launcher::new()?;
            let handle = context.handle();
            launcher.on_dismissed({
                let handle = handle.clone();
                move || handle.quit()
            });
            // Grow and shrink with the results.
            launcher.on_height_changed(move |height| {
                let _ = handle.set_size(640, height as u32);
            });
            Ok(launcher)
        })
    }

The component needs `forward-focus` to its text input, and gets Escape and
the other keys through `FocusScope`/`key-pressed` as in any Slint program.
`examples/typing.rs` is a complete program of this kind.

A bar-like client: stretched along the top, reserves its height, no
keyboard, fed by a worker thread.

    use sengi_layer::{Anchor, App, Config, Instances, Layer, Outputs};
    use slint::ComponentHandle;

    slint::include_modules!();

    fn main() -> Result<(), sengi_layer::Error> {
        let app = App::new()?;
        let bars: Instances<Bar> = Instances::new();

        let volume = {
            let bars = bars.clone();
            app.handle().channel(move |volume: i32| {
                bars.for_each(|bar| bar.set_volume(volume));
            })?
        };
        std::thread::spawn(move || loop {
            if volume.send(read_volume()).is_err() {
                return; // the loop has ended
            }
            std::thread::sleep(std::time::Duration::from_secs(15));
        });

        let config = Config::new("sengi-bar")
            .layer(Layer::Top)
            .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
            .size(0, 28)
            .exclusive_zone(28)
            .outputs(Outputs::All);

        app.run(config, move |_context| {
            let bar = Bar::new()?;
            bars.add(&bar);
            Ok(bar)
        })
    }

## API

All sizes and margins are logical pixels.

### `Config`

Built with `Config::new(namespace)` and chained setters; the fields are also
public. `config.validate()` reports combinations the compositor would reject.

| Setter | Meaning | Default |
| --- | --- | --- |
| `layer(Layer)` | `Background`, `Bottom`, `Top`, `Overlay` | `Top` |
| `anchor(Anchor)` | `Anchor::TOP \| BOTTOM \| LEFT \| RIGHT`, or `Anchor::NONE` to centre | `NONE` |
| `size(width, height)` | 0 stretches along that axis and needs both of its edges anchored | `(1, 1)` |
| `margins(Margins)` | `Margins { top, right, bottom, left }` or `Margins::all(n)` | 0 |
| `exclusive_zone(i32)` | Space reserved at the anchored edge; -1 ignores other zones | 0 |
| `keyboard(KeyboardMode)` | `None`, `OnDemand`, `Exclusive` | `None` |
| `outputs(Outputs)` | `Primary`: one surface, the compositor picks the output. `All`: one per output | `Primary` |

### Running

- `sengi_layer::run(config, build) -> Result<(), Error>` connects, builds,
  shows and runs until quit.
- `App::new()? ` then `app.handle()` and `app.run(config, build)` is the same
  in two steps, for when the handle is needed before the loop starts. One
  `App` per process.
- `build` is `FnMut(&Context) -> Result<C, Box<dyn std::error::Error>>` where
  `C` is the Slint component. It runs after the Slint platform is installed.
  With `Outputs::Primary` it runs once. With `Outputs::All` it runs once per
  output, and again when an output is plugged in; the instance is dropped
  when its output goes away. Do not call `show()` or `run()` on the
  component yourself.
- `Context::handle()` gives the `Handle`; `Context::output_name()` gives the
  output's name (`eDP-1`) with `Outputs::All`.
- `run` returns `Ok(())` after a quit, and an `Error` (`Config`, `Wayland`,
  `Platform`, `Build`) when it cannot start or the connection is lost.

### `Handle`

Cloneable, UI thread only. Changes apply to every surface of the app and
take effect before the loop next sleeps.

| Method | Effect |
| --- | --- |
| `set_size(width, height) -> Result<(), ConfigError>` | Resize; the component follows when the compositor confirms |
| `size() -> (u32, u32)` | The size last asked for |
| `set_exclusive_zone(i32)` | Change the reserved space |
| `set_margins(Margins)` | Change the margins |
| `set_keyboard(KeyboardMode)` | Change keyboard interactivity |
| `hide()` / `show()` / `is_visible()` | Unmap and remap. The component, its state and timers stay alive. `hide()` before `run` starts hidden |
| `quit()` | End the loop |
| `channel(receive) -> Result<Sender<T>, Error>` | `Sender::send` from any thread wakes the loop and calls `receive(message)` on the UI thread |
| `connection() -> wayland_client::Connection` | For other protocols: make your own event queue and dispatch it on your own thread |

`sengi_layer::wayland_client` re-exports the crate the connection belongs to.

### `Instances<C>`

A list of the live component instances: `add(&component)` in the build
closure, `for_each(|component| ...)` from timers and channel callbacks.
Instances whose output has gone are skipped.

### Slint integration

- Timers and animations work as usual. The loop sleeps until the next timer
  or event and ticks at frame rate only while something animates.
- `slint::invoke_from_event_loop`, `Weak::upgrade_in_event_loop` and
  `slint::quit_event_loop` work from any thread.
- Pointer: move, enter and leave, left, right, middle, back and forward
  buttons, wheel and touchpad scrolling. The cursor is set to the default
  arrow on enter.
- Keyboard: text follows the active layout, with dead keys and compose.
  Escape, Return, Backspace, Tab, Delete, arrows, Home, End, Page Up and
  Down, F1 to F12 and the modifiers arrive as Slint `Key` codes. Held keys
  repeat at the compositor's rate. The keyboard is only bound when the mode
  is not `None`, so a bar never loads a keymap.
- Copy and paste work inside the program only.
- HiDPI: integer buffer scale, per surface.
- The Slint `Window` background may be transparent or translucent.

### Outputs

With `Outputs::Primary`, when the compositor closes the surface (its output
was unplugged) the same component is put on another output, or waits for one
to appear. With `Outputs::All`, each output has its own instance.

## Building

Needs `libxkbcommon-dev` at build time and `libxkbcommon0` at run time.
