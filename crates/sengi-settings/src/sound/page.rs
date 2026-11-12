//! The Sound page in the window.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use slint::ComponentHandle;

use super::{Audio, AudioDevices, Command, Latest, Node, Side, MAX_PERCENT};
use crate::app::{self, App, Choice, Controller};
use crate::ui::{SoundData, SoundSide};
use crate::worker::Handle;

pub struct SoundPage {
    service: Arc<dyn AudioDevices>,
    worker: RefCell<Option<Handle<Command>>>,
    latest: Arc<Latest>,
    audio: RefCell<Option<Audio>>,
}

fn side_of(output: bool) -> Side {
    if output {
        Side::Output
    } else {
        Side::Input
    }
}

/// What the page shows for one direction.
fn shown(nodes: &[Node]) -> SoundSide {
    let default = nodes.iter().find(|node| node.default);
    SoundSide {
        present: default.is_some(),
        device: default
            .map(|node| node.name.as_str())
            .unwrap_or_default()
            .into(),
        choice: nodes.len() > 1 || (default.is_none() && !nodes.is_empty()),
        muted: default.is_some_and(|node| node.muted),
    }
}

impl SoundPage {
    pub fn attach(app: &Rc<App>, service: Arc<dyn AudioDevices>) -> Rc<SoundPage> {
        let page = Rc::new(SoundPage {
            service,
            worker: RefCell::default(),
            latest: Arc::default(),
            audio: RefCell::default(),
        });
        let data = app.window.global::<SoundData>();
        let weak = Rc::downgrade(&page);
        data.on_volume(move |output, percent| {
            let Some(page) = weak.upgrade() else { return };
            let side = side_of(output);
            if let Some(id) = page.default_of(side) {
                page.send(Command::Volume {
                    side,
                    id,
                    percent: percent.clamp(0, MAX_PERCENT as i32) as u32,
                    sequence: page.latest.next(side),
                });
            }
        });
        let weak = Rc::downgrade(&page);
        data.on_mute(move |output, muted| {
            let Some(page) = weak.upgrade() else { return };
            if let Some(id) = page.default_of(side_of(output)) {
                page.send(Command::Mute(id, muted));
            }
        });
        let (weak, weak_app) = (Rc::downgrade(&page), Rc::downgrade(app));
        data.on_choose(move |output| {
            let (Some(page), Some(app)) = (weak.upgrade(), weak_app.upgrade()) else {
                return;
            };
            let choices: Vec<Choice> = match page.audio.borrow().as_ref() {
                Some(audio) => audio
                    .side(side_of(output))
                    .iter()
                    .map(|node| Choice {
                        id: node.id.to_string(),
                        title: node.name.clone(),
                        detail: String::new(),
                        current: node.default,
                    })
                    .collect(),
                None => return,
            };
            let weak = Rc::downgrade(&page);
            let title = if output {
                "Output device"
            } else {
                "Input device"
            };
            app.choose(title, false, choices, move |_, id| {
                if let (Some(page), Ok(id)) = (weak.upgrade(), id.parse()) {
                    page.send(Command::Default(id));
                }
            });
        });
        page
    }

    fn default_of(&self, side: Side) -> Option<u32> {
        let audio = self.audio.borrow();
        let nodes = audio.as_ref()?.side(side);
        nodes.iter().find(|node| node.default).map(|node| node.id)
    }

    fn send(&self, command: Command) {
        if let Some(worker) = self.worker.borrow().as_ref() {
            worker.send(command);
        }
    }

    pub fn on_reading(&self, app: &Rc<App>, audio: Option<Audio>) {
        let data = app.window.global::<SoundData>();
        data.set_note(
            if audio.is_some() {
                ""
            } else {
                "The sound service (PipeWire) is not running, so sound cannot be set here."
            }
            .into(),
        );
        let empty = Audio::default();
        let now = audio.as_ref().unwrap_or(&empty);
        data.set_output(shown(&now.outputs));
        data.set_input(shown(&now.inputs));
        // A slider under the finger is the user's, not PipeWire's.
        let percent = |node: Option<&Node>| node.map_or(0, |n| n.percent.min(MAX_PERCENT) as i32);
        if !data.get_output_held() {
            data.set_output_volume(percent(now.output()));
        }
        if !data.get_input_held() {
            data.set_input_volume(percent(now.input()));
        }
        *self.audio.borrow_mut() = audio;
    }
}

impl Controller for SoundPage {
    fn show(&self, _app: &Rc<App>) {
        let mut worker = self.worker.borrow_mut();
        match worker.as_ref() {
            Some(worker) => worker.refresh(),
            None => {
                *worker = Some(super::start(
                    self.service.clone(),
                    self.latest.clone(),
                    |audio| {
                        app::post(move |app| {
                            if let Some(page) = app.page::<SoundPage>() {
                                page.on_reading(app, audio);
                            }
                        });
                    },
                ));
            }
        }
    }

    fn shutdown(&self) {
        if let Some(worker) = self.worker.borrow().as_ref() {
            worker.stop();
        }
        // Not left to the worker, which may be in the middle of a wpctl:
        // the monitor must be gone before this process is.
        self.service.close();
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sound::testing::node;

    #[test]
    fn a_side_shows_its_default_device() {
        let mut nodes = vec![node(1, "Speakers", 40, true), node(2, "HDMI", 100, false)];
        nodes[0].muted = true;
        let side = shown(&nodes);
        assert!(side.present && side.choice && side.muted);
        assert_eq!(side.device, "Speakers");
        // One device: nothing to choose between.
        assert!(!shown(&nodes[..1]).choice);
        // None at all.
        let none = shown(&[]);
        assert!(!none.present && !none.choice && none.device.is_empty());
        // Devices but no default: one can be picked.
        let orphan = shown(&[node(3, "USB", 50, false)]);
        assert!(!orphan.present && orphan.choice);
    }
}
