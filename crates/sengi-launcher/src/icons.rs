//! App icons for the panel, loaded in the background. The launcher links no
//! image decoder (the image build checks): it asks the dock, which has one,
//! to find and decode each icon in a child process, a few names at a time.

use std::path::PathBuf;
use std::time::Duration;

use sengi_apps::decoder::{Decoder, Pixels};

/// The program that answers the decode request. The dock already carries
/// the PNG and SVG code, and is always in the image.
const DECODER_PROGRAM: &str = "/usr/bin/sengi-dock";

/// How long one icon may take before the next is tried.
const TIMEOUT: Duration = Duration::from_secs(1);

/// The icon names to try for one candidate, best first.
pub struct Request {
    pub index: usize,
    pub names: Vec<String>,
}

pub fn decoder() -> Decoder {
    Decoder::new(PathBuf::from(DECODER_PROGRAM))
}

/// Decodes the icons at `pixels` per side, one after another in a thread of
/// its own, and gives each result to `deliver`. A candidate none of whose
/// names has an icon that draws something is delivered with `None`.
pub fn start(
    requests: Vec<Request>,
    pixels: u32,
    decoder: Decoder,
    deliver: impl Fn(usize, Option<Pixels>) + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("icons".into())
        .spawn(move || {
            for request in requests {
                let icon = request
                    .names
                    .iter()
                    .find_map(|name| decoder.decode(name, "", pixels, TIMEOUT));
                deliver(request.index, icon);
            }
        });
    if let Err(error) = spawned {
        eprintln!("sengi-launcher: cannot load icons: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn every_request_is_answered_even_when_nothing_can_decode() {
        // A decoder that does not exist gives nothing, and the answers still
        // come, in order.
        let (sender, receiver) = mpsc::channel();
        let requests = (0..3)
            .map(|index| Request {
                index,
                names: vec![format!("icon-{index}")],
            })
            .collect();
        start(
            requests,
            16,
            Decoder::new(PathBuf::from("/no/such/decoder")),
            move |index, icon| {
                sender.send((index, icon.is_some())).unwrap();
            },
        );
        let answers: Vec<_> = receiver.iter().take(3).collect();
        assert_eq!(answers, [(0, false), (1, false), (2, false)]);
    }
}
