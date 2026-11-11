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

A launcher-like client: centred, holds the keyboard, quits on Escape or when
the keyboard goes elsewhere.

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
            let surface = context.surface();

            launcher.on_dismissed({
                let handle = handle.clone();
                move || handle.quit()
            });
            surface.on_focus_changed(move |focused| {
                if !focused {
                    handle.quit();
                }
            });
            // Grow and shrink with the results.
            launcher.on_height_changed(move |height| {
                let _ = surface.set_size(640, height as u32);
            });
            Ok(launcher)
        })
    }

The component needs `forward-focus` to its text input, and gets Escape and
the other keys through `FocusScope`/`key-pressed` as in any Slint program.
`examples/typing.rs` is a complete program of this kind.

A bar-like client: one per output, stretched along the top, reserves its
height, no keyboard, fed by a worker thread.

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

        app.run(config, move |context| {
            let bar = Bar::new()?;
            if let Some(output) = context.output() {
                bar.set_compact(output.size.is_some_and(|(width, _)| width < 1000));
            }
            bars.add(&bar);
            Ok(bar)
        })
    }

## Things to know

- Never call `show()`, `hide()` or `run()` on the component. The loop shows
  it, and Slint treats hiding the last window as a request to quit the
  program. Use `Handle::hide` or `SurfaceHandle::hide`.
- A layer surface is never told about clicks outside itself. A launcher that
  should close on a click elsewhere needs a surface covering the output
  (all four anchors, size `(0, 0)`) that draws its box in the middle and
  handles the click around it in its own UI. Closing when the keyboard goes
  elsewhere is `SurfaceHandle::on_focus_changed`.
- Sizes can come from the component: read them in the build closure and call
  `context.surface().set_size(..)` before returning. The surface is created
  after the closure returns.
- One `App` per process.

## API

All sizes, margins and rectangles are logical pixels.

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
| `outputs(Outputs)` | See below | `Auto` |

`Outputs`:

- `Auto`: one surface on the output the compositor picks. Built once, at
  start. If its output goes away the same component moves to another output
  or waits for one. (`Outputs::Primary` is the old, deprecated name.)
- `All`: one instance of the component per output. Built when an output
  appears, freed when it goes.
- `Named(String)`: one instance on the output with that name, such as
  `eDP-1`; none while that output is absent.

### Running

- `sengi_layer::run(config, build) -> Result<(), Error>` connects, builds,
  shows and runs until quit.
- `App::new()?`, then `app.handle()` and `app.run(config, build)`, is the
  same in two steps, for when the handle is needed before the loop starts.
- `build` is `FnMut(&Context) -> Result<C, Box<dyn std::error::Error>>` where
  `C` is the Slint component. It runs after the Slint platform is installed.
- `run` returns `Ok(())` after a quit, and an `Error` (`Config`, `Wayland`,
  `Platform`, `Build`) when it cannot start or the connection is lost.

### `Context`

| Method | Gives |
| --- | --- |
| `handle() -> Handle` | The app-wide handle |
| `surface() -> SurfaceHandle` | The handle for this instance's surface |
| `output() -> Option<Output>` | This instance's output. Known at build time with `All` and `Named`; `None` with `Auto` until the compositor has picked |
| `output_name() -> Option<&str>` | Its name alone |

`Output { name: Option<String>, size: Option<(u32, u32)>, scale: u32 }`; the
size is logical.

### `Handle`

Cloneable, UI thread only. Changes apply to every surface of the app and
take effect before the loop next sleeps.

| Method | Effect |
| --- | --- |
| `set_size(width, height) -> Result<(), ConfigError>` | Resize; the component follows when the compositor confirms. Replaces per-surface sizes |
| `size() -> (u32, u32)` | The app-wide size last asked for |
| `set_anchor(Anchor) -> Result<(), ConfigError>` | Change the anchored edges |
| `set_anchor_and_size(Anchor, width, height) -> Result<(), ConfigError>` | Both at once, when neither is valid with the other's old value |
| `set_layer(Layer)` | Move to another layer |
| `set_exclusive_zone(i32)` | Change the reserved space |
| `set_margins(Margins)` | Change the margins |
| `set_keyboard(KeyboardMode)` | Change keyboard interactivity |
| `set_input_region(Option<&[Rect]>)` | Take pointer input only inside these rectangles; `None` is the whole surface, `Some(&[])` nothing. Replaces per-surface regions |
| `hide()` / `show()` / `is_visible()` | Unmap and remap. The component, its state and timers stay alive. `hide()` before `run` starts hidden |
| `quit()` | End the loop |
| `channel(receive) -> Result<Sender<T>, Error>` | `Sender::send` from any thread wakes the loop and calls `receive(message)` on the UI thread |
| `connection() -> wayland_client::Connection` | For other protocols; see below |

### `SurfaceHandle`

Cloneable, UI thread only. Affects one instance's surface. For sizes and
input regions the most recent call wins, whether it was made here or on
`Handle`.

| Method | Effect |
| --- | --- |
| `set_size(width, height) -> Result<(), ConfigError>` | Resize this surface only |
| `size() -> (u32, u32)` | Its real size on screen; `(0, 0)` before it is shown |
| `scale() -> u32` | Its integer scale |
| `set_input_region(Option<&[Rect]>)` | Input region for this surface only |
| `hide()` / `show()` / `is_visible()` | Unmap and remap this surface only |
| `output() -> Option<Output>` | The output it is on |
| `on_output_changed(impl FnMut(Output))` | Called when the output becomes known or changes size, scale or identity |
| `has_focus() -> bool` | Whether it has the keyboard |
| `on_focus_changed(impl FnMut(bool))` | Called when it gains or loses the keyboard |

`Rect { x, y, width, height }` or `Rect::new(x, y, width, height)`, all `i32`.

### `Instances<C>`

A list of the live component instances: `add(&component)` in the build
closure, `for_each(|component| ...)` from timers and channel callbacks. An
instance whose output has gone has been freed and is not visited.

### Other protocols: `Handle::connection()`

`sengi_layer::wayland_client` re-exports the crate the connection belongs
to. Use the connection from a thread of your own with an event queue of your
own: `connection.new_event_queue()`, a registry on that queue, and a loop
around `blocking_dispatch`; send results to the UI with `Handle::channel`.
`examples/soak.rs` does this next to an animating, resizing UI.

- Do not dispatch on the UI thread or create objects on the UI's queue.
- On the worker, wait only inside `blocking_dispatch`. Never hold a read
  guard from `prepare_read` while waiting on anything else: the UI loop
  waits for that guard too.
- A protocol error on one of the worker's objects ends the shared
  connection and with it the UI.
- The worker is not told when the UI loop ends. It finds out when
  `Sender::send` fails or `blocking_dispatch` errors.

### Slint integration

- Timers and animations work as usual. The loop sleeps until the next timer
  or event. Drawing is paced by the compositor's frame callbacks, so it
  follows the display and stops while the surface cannot be seen.
- `slint::invoke_from_event_loop`, `Weak::upgrade_in_event_loop` and
  `slint::quit_event_loop` work from any thread.
- Pointer: move, enter and leave, left, right, middle, back and forward
  buttons, wheel and touchpad scrolling. The cursor is set to the default
  arrow on enter.
- Keyboard: text follows the active layout. Dead keys and compose sequences
  produce their result and nothing else; they need the X11 locale data
  (`libx11-data`) and a UTF-8 locale. Escape, Return, Backspace, Tab,
  Delete, Insert, arrows, Home, End, Page Up and Down, F1 to F12 and the
  modifiers arrive as Slint `Key` codes. Text, editing and navigation keys
  repeat at the compositor's rate. The keyboard is only bound when the mode
  is not `None`, so a bar never loads a keymap.
- Copy and paste work inside the program only.
- HiDPI: integer buffer scale, per surface.
- The Slint `Window` background may be transparent or translucent.

## Testing

    cargo test -p sengi-layer
    cargo build -p sengi-layer --examples
    crates/sengi-layer/tests/keyboard.sh target/debug/examples/typing
    target/debug/examples/soak 120      # inside a Wayland session

`keyboard.sh` starts its own headless labwc and needs `labwc` and `wtype`.

## Building

Needs `libxkbcommon-dev` at build time and `libxkbcommon0` at run time.
