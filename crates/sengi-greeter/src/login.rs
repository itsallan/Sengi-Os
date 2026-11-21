//! One login after another, spoken to greetd.
//!
//! This runs on a thread of its own, so that a PAM module that takes its
//! time holds up nothing on the screen. The screen sends a [`Command`]
//! and gets exactly one [`Event`] back for each `Begin` and each `Answer`,
//! and none for a `Cancel`.
//!
//! What was typed before Return answers the first question that is to be
//! kept secret, which is the password on every ordinary account. Any
//! question after that, and any question whose answer may be seen, goes
//! back to the screen to be asked.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use sengi_lock::screen::Verdict;
use sengi_system::secret::Secret;

use crate::ipc::{read_reply, Asks, ReadError, Reply, Request};

pub enum Command {
    /// Log `user` in. `password` is what was typed, which may be nothing.
    Begin { user: String, password: Secret },
    /// The answer to the question last asked.
    Answer(Secret),
    /// Give up the login that a question is open on.
    Cancel,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// The login cannot go on without an answer to this.
    Asked {
        question: String,
        /// The answer is no secret and may be shown as it is typed.
        visible: bool,
        /// What a PAM module had to say before it asked.
        said: Option<String>,
    },
    /// The login is over. `Right` means greetd has taken the session and
    /// starts it when this program has exited.
    Judged(Verdict),
    /// greetd is not there any more and cannot be reached again.
    Gone,
}

/// A connection to greetd: bytes both ways, and a limit on how long a
/// reply is waited for.
pub trait Link: Read + Write {
    /// From now on a read that gets nothing for `limit` fails.
    fn wait_at_most(&mut self, limit: Duration) -> io::Result<()>;
}

impl Link for UnixStream {
    fn wait_at_most(&mut self, limit: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(limit))
    }
}

/// How long greetd is given to answer. Without a limit a greetd that has
/// stopped answering would leave the field on "Checking…" for good, with
/// no way back but the power button.
#[derive(Clone, Copy, Debug)]
pub struct Patience {
    /// For what greetd answers by itself: starting a login, giving one
    /// up, taking the session. It has nothing to wait for there.
    pub greetd: Duration,
    /// For the answer to something we handed on to PAM. A module may
    /// rightly take long over it: pam_unix alone sits two seconds on a
    /// wrong password, others ask a server on the network or wait for a
    /// finger on a reader. Two minutes is longer than any of that and
    /// still an end.
    pub pam: Duration,
}

impl Default for Patience {
    fn default() -> Patience {
        Patience {
            greetd: Duration::from_secs(10),
            pam: Duration::from_secs(120),
        }
    }
}

/// The connection is of no more use: it broke, greetd sent something
/// that is no reply, or no reply came in time.
struct Lost;

impl From<io::Error> for Lost {
    fn from(_: io::Error) -> Lost {
        Lost
    }
}

impl From<ReadError> for Lost {
    fn from(_: ReadError) -> Lost {
        Lost
    }
}

struct Talk<S> {
    stream: S,
    /// greetd is waiting for an answer from us.
    open: bool,
    /// What PAM modules have said in this login that was not a question
    /// and has not been shown yet, in the order it came.
    said: Vec<String>,
    patience: Patience,
}

/// No login says more than this many things worth keeping.
const MOST_SAID: usize = 8;

/// Several messages as one line: each a sentence of its own.
fn joined(said: &mut Vec<String>) -> Option<String> {
    let mut line = String::new();
    for message in said.drain(..) {
        if line.ends_with(['.', '!', '?', ':', ';']) {
            line.push(' ');
        } else if !line.is_empty() {
            line.push_str(". ");
        }
        line.push_str(message.trim());
    }
    Some(line).filter(|line| !line.is_empty())
}

/// greetd's own words about what went wrong are for whoever reads the
/// journal. They name PAM calls and internal states, not anything the
/// person at the login screen can act on.
fn log(what: &str, description: &str) {
    eprintln!(
        "sengi-greeter: greetd {what}: {}",
        sengi_text::line(description, 200)
    );
}

impl<S: Link> Talk<S> {
    /// Sends `request` and waits for the reply, `limit` at the longest.
    fn ask(&mut self, request: Request, limit: Duration) -> Result<Reply, Lost> {
        self.stream.wait_at_most(limit)?;
        request.send(&mut self.stream)?;
        // The request, and the password if it held one, is wiped here.
        drop(request);
        Ok(read_reply(&mut self.stream)?)
    }

    fn cancel(&mut self) -> Result<(), Lost> {
        self.open = false;
        self.said.clear();
        // Whatever it answers, there is nothing more to do about it.
        self.ask(Request::cancel_session(), self.patience.greetd)
            .map(drop)
    }

    /// Carries a login on from `request`, whose reply is given `limit`,
    /// until it ends or needs the screen. `typed` answers the first
    /// secret question if it is there.
    fn carry_on(
        &mut self,
        request: Request,
        limit: Duration,
        mut typed: Option<Secret>,
        session: &[String],
    ) -> Result<Event, Lost> {
        let Patience { greetd, pam } = self.patience;
        let mut reply = self.ask(request, limit)?;
        loop {
            reply = match reply {
                Reply::Success => {
                    self.open = false;
                    return match self.ask(Request::start_session(session, &[]), greetd)? {
                        Reply::Success => Ok(Event::Judged(Verdict::Right)),
                        Reply::Error { description, .. } => {
                            log("would not start the session", &description);
                            self.cancel()?;
                            Ok(Event::Judged(Verdict::Unchecked(None)))
                        }
                        Reply::AuthMessage { .. } => Err(Lost),
                    };
                }
                Reply::Error { auth, description } => {
                    let said = joined(&mut self.said);
                    if !auth {
                        log("could not check the login", &description);
                    }
                    self.cancel()?;
                    return Ok(Event::Judged(if auth {
                        Verdict::Wrong(said)
                    } else {
                        Verdict::Unchecked(said)
                    }));
                }
                Reply::AuthMessage {
                    asks: Asks::Secret,
                    message,
                } => match typed.take().filter(|typed| !typed.is_empty()) {
                    Some(typed) => {
                        let answer = Request::post_auth_message_response(Some(typed.expose()));
                        self.ask(answer, pam)?
                    }
                    None => return Ok(self.put(message, false)),
                },
                Reply::AuthMessage {
                    asks: Asks::Visible,
                    message,
                } => return Ok(self.put(message, true)),
                Reply::AuthMessage { message, .. } => {
                    if !message.trim().is_empty() && self.said.len() < MOST_SAID {
                        self.said.push(message);
                    }
                    self.ask(Request::post_auth_message_response(None), pam)?
                }
            };
        }
    }

    fn put(&mut self, question: String, visible: bool) -> Event {
        self.open = true;
        Event::Asked {
            question,
            visible,
            said: joined(&mut self.said),
        }
    }
}

/// Serves commands until the screen is gone or greetd is. `connect` is
/// called once at the start, and again whenever a connection has failed:
/// greetd closes a connection it could make no sense of, and a login that
/// broke that way should not take the login screen with it. The same
/// goes for a reply that did not come within `patience`.
pub fn serve<S: Link>(
    mut connect: impl FnMut() -> io::Result<S>,
    commands: Receiver<Command>,
    mut tell: impl FnMut(Event) -> bool,
    session: &[String],
    patience: Patience,
) {
    let mut talk: Option<Talk<S>> = None;
    for command in commands {
        if talk.is_none() {
            match connect() {
                Ok(stream) => {
                    talk = Some(Talk {
                        stream,
                        open: false,
                        said: Vec::new(),
                        patience,
                    })
                }
                Err(_) => {
                    tell(Event::Gone);
                    return;
                }
            }
        }
        let Some(current) = talk.as_mut() else {
            return;
        };
        let outcome = match command {
            Command::Begin { user, password } => (|| {
                if current.open {
                    current.cancel()?;
                }
                current.said.clear();
                let create = Request::create_session(&user);
                current.carry_on(create, patience.greetd, Some(password), session)
            })(),
            Command::Answer(answer) if current.open => {
                current.open = false;
                let request = Request::post_auth_message_response(Some(answer.expose()));
                drop(answer);
                current.carry_on(request, patience.pam, None, session)
            }
            // Nothing was asked. The screen is owed a verdict all the same.
            Command::Answer(_) => Ok(Event::Judged(Verdict::Unchecked(None))),
            Command::Cancel => {
                // The screen expects nothing back for this, and has moved
                // on. If the connection breaks over it, the connection is
                // dropped and nothing is said: a verdict now would be
                // taken for the next login's.
                if current.open && current.cancel().is_err() {
                    talk = None;
                }
                continue;
            }
        };
        let event = match outcome {
            Ok(event) => event,
            Err(Lost) => {
                talk = None;
                Event::Judged(Verdict::Unchecked(None))
            }
        };
        if !tell(event) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::thread;

    /// For a reply: read the request and then say nothing, for as long as
    /// the connection is there.
    const SILENCE: &str = "(silence)";

    /// A greetd that expects these requests in this order and gives these
    /// replies. An empty reply means: hang up instead of answering.
    fn greetd(script: Vec<(&'static str, &'static str)>) -> (UnixStream, thread::JoinHandle<()>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let played = thread::spawn(move || {
            let mut stream = theirs;
            for (expected, reply) in script {
                let mut length = [0u8; 4];
                stream.read_exact(&mut length).unwrap();
                let mut payload = vec![0u8; u32::from_ne_bytes(length) as usize];
                stream.read_exact(&mut payload).unwrap();
                assert_eq!(String::from_utf8(payload).unwrap(), expected);
                if reply.is_empty() {
                    return;
                }
                if reply == SILENCE {
                    let _ = stream.read_to_end(&mut Vec::new());
                    return;
                }
                stream
                    .write_all(&(reply.len() as u32).to_ne_bytes())
                    .unwrap();
                stream.write_all(reply.as_bytes()).unwrap();
            }
            // Nothing more may come.
            let mut rest = Vec::new();
            stream.read_to_end(&mut rest).unwrap();
            assert!(rest.is_empty(), "an unexpected request");
        });
        (ours, played)
    }

    fn begin(user: &str, password: &str) -> Command {
        Command::Begin {
            user: user.to_string(),
            password: Secret::new(password.to_string()),
        }
    }

    fn answer(text: &str) -> Command {
        Command::Answer(Secret::new(text.to_string()))
    }

    /// Runs `commands` against greetds that play `scripts`, one after the
    /// other as connections are made, and gives back what the screen was
    /// told.
    fn run(scripts: Vec<Vec<(&'static str, &'static str)>>, commands: Vec<Command>) -> Vec<Event> {
        run_with(Patience::default(), scripts, commands)
    }

    fn run_with(
        patience: Patience,
        scripts: Vec<Vec<(&'static str, &'static str)>>,
        commands: Vec<Command>,
    ) -> Vec<Event> {
        let mut daemons = Vec::new();
        let mut streams = Vec::new();
        for script in scripts {
            let (stream, played) = greetd(script);
            streams.push(stream);
            daemons.push(played);
        }
        streams.reverse();
        let (send, receive) = mpsc::channel();
        for command in commands {
            send.send(command).unwrap();
        }
        drop(send);
        let mut events = Vec::new();
        serve(
            || streams.pop().ok_or(io::ErrorKind::NotFound.into()),
            receive,
            |event| {
                events.push(event);
                true
            },
            &["/usr/bin/sengi-session".to_string()],
            patience,
        );
        drop(streams);
        for played in daemons {
            played.join().unwrap();
        }
        events
    }

    const CREATE: &str = r#"{"type":"create_session","username":"live"}"#;
    const ASK_PASSWORD: &str =
        r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#;
    const START: &str = r#"{"type":"start_session","cmd":["/usr/bin/sengi-session"],"env":[]}"#;
    const CANCEL: &str = r#"{"type":"cancel_session"}"#;
    const SUCCESS: &str = r#"{"type":"success"}"#;
    const NOTHING: &str = r#"{"type":"post_auth_message_response"}"#;
    const DENIED: &str =
        r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#;

    #[test]
    fn the_right_password_starts_the_session() {
        let events = run(
            vec![vec![
                (CREATE, ASK_PASSWORD),
                (
                    r#"{"type":"post_auth_message_response","response":"live"}"#,
                    SUCCESS,
                ),
                (START, SUCCESS),
            ]],
            vec![begin("live", "live")],
        );
        assert_eq!(events, [Event::Judged(Verdict::Right)]);
    }

    #[test]
    fn a_wrong_password_cancels_the_session_and_the_next_try_starts_over() {
        let wrong = r#"{"type":"post_auth_message_response","response":"wrong"}"#;
        let events = run(
            vec![vec![
                (CREATE, ASK_PASSWORD),
                (wrong, DENIED),
                (CANCEL, SUCCESS),
                (CREATE, ASK_PASSWORD),
                (wrong, DENIED),
                // Even a greetd that will not hear of the cancel.
                (
                    CANCEL,
                    r#"{"type":"error","error_type":"error","description":"no session active"}"#,
                ),
                (CREATE, ASK_PASSWORD),
                (
                    r#"{"type":"post_auth_message_response","response":"live"}"#,
                    SUCCESS,
                ),
                (START, SUCCESS),
            ]],
            vec![
                begin("live", "wrong"),
                begin("live", "wrong"),
                begin("live", "live"),
            ],
        );
        assert_eq!(
            events,
            [
                Event::Judged(Verdict::Wrong(None)),
                Event::Judged(Verdict::Wrong(None)),
                Event::Judged(Verdict::Right)
            ]
        );
    }

    #[test]
    fn an_account_with_no_password_is_let_in_without_one() {
        let events = run(
            vec![vec![(CREATE, SUCCESS), (START, SUCCESS)]],
            vec![begin("live", "")],
        );
        assert_eq!(events, [Event::Judged(Verdict::Right)]);
    }

    #[test]
    fn return_on_an_empty_field_brings_the_question_and_then_the_answer_goes() {
        let events = run(
            vec![vec![
                (CREATE, ASK_PASSWORD),
                (
                    r#"{"type":"post_auth_message_response","response":"live"}"#,
                    SUCCESS,
                ),
                (START, SUCCESS),
            ]],
            vec![begin("live", ""), answer("live")],
        );
        assert_eq!(
            events,
            [
                Event::Asked {
                    question: "Password: ".to_string(),
                    visible: false,
                    said: None
                },
                Event::Judged(Verdict::Right)
            ]
        );
    }

    #[test]
    fn every_kind_of_message_is_handled() {
        // A notice, the password, a problem, a code that may be seen and a
        // second secret: none answered with the password but the first
        // secret one.
        let events = run(
            vec![vec![
                (
                    CREATE,
                    r#"{"type":"auth_message","auth_message_type":"info","auth_message":"Two steps are needed"}"#,
                ),
                (NOTHING, ASK_PASSWORD),
                (
                    r#"{"type":"post_auth_message_response","response":"live"}"#,
                    r#"{"type":"auth_message","auth_message_type":"error","auth_message":"The token is not plugged in"}"#,
                ),
                (
                    NOTHING,
                    r#"{"type":"auth_message","auth_message_type":"visible","auth_message":"Code: "}"#,
                ),
                (
                    r#"{"type":"post_auth_message_response","response":"123 456"}"#,
                    r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"PIN: "}"#,
                ),
                (
                    r#"{"type":"post_auth_message_response","response":""}"#,
                    SUCCESS,
                ),
                (START, SUCCESS),
            ]],
            vec![begin("live", "live"), answer("123 456"), answer("")],
        );
        assert_eq!(
            events,
            [
                Event::Asked {
                    question: "Code: ".to_string(),
                    visible: true,
                    said: Some("Two steps are needed. The token is not plugged in".to_string())
                },
                Event::Asked {
                    question: "PIN: ".to_string(),
                    visible: false,
                    said: None
                },
                Event::Judged(Verdict::Right)
            ]
        );
    }

    #[test]
    fn what_a_module_said_goes_with_the_refusal() {
        let events = run(
            vec![vec![
                (CREATE, ASK_PASSWORD),
                (
                    r#"{"type":"post_auth_message_response","response":"wrong"}"#,
                    r#"{"type":"auth_message","auth_message_type":"error","auth_message":"2 attempts left"}"#,
                ),
                (NOTHING, DENIED),
                (CANCEL, SUCCESS),
            ]],
            vec![begin("live", "wrong")],
        );
        assert_eq!(
            events,
            [Event::Judged(Verdict::Wrong(Some(
                "2 attempts left".to_string()
            )))]
        );
    }

    #[test]
    fn everything_the_modules_said_is_kept_and_reads_as_sentences() {
        let mut said = vec![
            "Your password expires in 3 days".to_string(),
            " 2 attempts left. ".to_string(),
            "Touch the key!".to_string(),
            "Now".to_string(),
        ];
        assert_eq!(
            joined(&mut said).as_deref(),
            Some("Your password expires in 3 days. 2 attempts left. Touch the key! Now")
        );
        assert!(said.is_empty());
        assert_eq!(joined(&mut said), None);

        // What PAM said is shown with a failure that is not a wrong
        // password; what greetd said about it is not.
        let events = run(
            vec![vec![
                (
                    CREATE,
                    r#"{"type":"auth_message","auth_message_type":"error","auth_message":"The directory is not answering"}"#,
                ),
                (
                    NOTHING,
                    r#"{"type":"error","error_type":"error","description":"pam_authenticate: AUTHINFO_UNAVAIL"}"#,
                ),
                (CANCEL, SUCCESS),
            ]],
            vec![begin("live", "live")],
        );
        assert_eq!(
            events,
            [Event::Judged(Verdict::Unchecked(Some(
                "The directory is not answering".to_string()
            )))]
        );
    }

    #[test]
    fn an_error_that_is_not_about_the_password_is_not_counted_as_one() {
        let events = run(
            vec![vec![
                (
                    CREATE,
                    r#"{"type":"error","error_type":"error","description":"a session is already being configured"}"#,
                ),
                (CANCEL, SUCCESS),
                // And a session that greetd will not start.
                (CREATE, SUCCESS),
                (
                    START,
                    r#"{"type":"error","error_type":"error","description":"no such command"}"#,
                ),
                (CANCEL, SUCCESS),
            ]],
            vec![begin("live", "live"), begin("live", "live")],
        );
        assert_eq!(
            events,
            [
                // greetd's wording goes to the journal, not to the screen.
                Event::Judged(Verdict::Unchecked(None)),
                Event::Judged(Verdict::Unchecked(None))
            ]
        );
    }

    #[test]
    fn a_question_can_be_given_up() {
        let events = run(
            vec![vec![
                (CREATE, ASK_PASSWORD),
                (CANCEL, SUCCESS),
                (CREATE, ASK_PASSWORD),
                // A new login while a question is open gives the old one up.
                (CANCEL, SUCCESS),
                (
                    r#"{"type":"create_session","username":"other"}"#,
                    ASK_PASSWORD,
                ),
            ]],
            vec![
                begin("live", ""),
                Command::Cancel,
                // With nothing open this is nothing.
                Command::Cancel,
                begin("live", ""),
                begin("other", ""),
            ],
        );
        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .all(|event| matches!(event, Event::Asked { .. })));
    }

    #[test]
    fn a_cancel_that_loses_the_connection_says_nothing() {
        let events = run(
            vec![
                vec![(CREATE, ASK_PASSWORD), (CANCEL, "")],
                vec![(CREATE, SUCCESS), (START, SUCCESS)],
            ],
            vec![begin("live", ""), Command::Cancel, begin("live", "live")],
        );
        // The question, and then the next login's own verdict. No verdict
        // for the cancel in between.
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], Event::Asked { .. }));
        assert_eq!(events[1], Event::Judged(Verdict::Right));
    }

    #[test]
    fn an_answer_to_nothing_is_not_sent() {
        let events = run(vec![vec![]], vec![answer("live")]);
        assert_eq!(events, [Event::Judged(Verdict::Unchecked(None))]);
    }

    #[test]
    fn a_greetd_that_hangs_up_costs_one_try_and_is_called_again() {
        let events = run(
            vec![
                vec![(CREATE, "")],
                vec![(CREATE, SUCCESS), (START, SUCCESS)],
            ],
            vec![begin("live", "live"), begin("live", "live")],
        );
        assert_eq!(
            events,
            [
                Event::Judged(Verdict::Unchecked(None)),
                Event::Judged(Verdict::Right)
            ]
        );
    }

    #[test]
    fn a_greetd_that_stops_answering_is_not_waited_for_for_ever() {
        let short = Duration::from_millis(60);
        let live = r#"{"type":"post_auth_message_response","response":"live"}"#;
        // Silent on each kind of request in turn; after each the next
        // login gets a new connection.
        let events = run_with(
            Patience {
                greetd: short,
                pam: short,
            },
            vec![
                vec![(CREATE, SILENCE)],
                vec![(CREATE, ASK_PASSWORD), (live, SILENCE)],
                vec![(CREATE, SUCCESS), (START, SILENCE)],
                vec![(CREATE, ASK_PASSWORD), (live, DENIED), (CANCEL, SILENCE)],
                vec![(CREATE, SUCCESS), (START, SUCCESS)],
            ],
            (0..5).map(|_| begin("live", "live")).collect(),
        );
        let unchecked = || Event::Judged(Verdict::Unchecked(None));
        assert_eq!(
            events,
            [
                unchecked(),
                unchecked(),
                unchecked(),
                unchecked(),
                Event::Judged(Verdict::Right)
            ]
        );
    }

    #[test]
    fn pam_is_given_longer_than_greetd_itself() {
        // The answer comes after greetd's own limit has long passed and
        // well within PAM's.
        let (ours, theirs) = UnixStream::pair().unwrap();
        let slow = thread::spawn(move || {
            let mut stream = theirs;
            let mut say = |reply: &str, after: Duration| {
                let mut length = [0u8; 4];
                stream.read_exact(&mut length).unwrap();
                let mut payload = vec![0u8; u32::from_ne_bytes(length) as usize];
                stream.read_exact(&mut payload).unwrap();
                thread::sleep(after);
                stream
                    .write_all(&(reply.len() as u32).to_ne_bytes())
                    .unwrap();
                stream.write_all(reply.as_bytes()).unwrap();
            };
            say(ASK_PASSWORD, Duration::ZERO);
            say(SUCCESS, Duration::from_millis(250));
            say(SUCCESS, Duration::ZERO);
        });
        let (send, receive) = mpsc::channel();
        send.send(begin("live", "live")).unwrap();
        drop(send);
        let mut ours = Some(ours);
        let mut events = Vec::new();
        serve(
            || ours.take().ok_or(io::ErrorKind::NotFound.into()),
            receive,
            |event| {
                events.push(event);
                true
            },
            &["/usr/bin/sengi-session".to_string()],
            Patience {
                greetd: Duration::from_millis(50),
                pam: Duration::from_secs(20),
            },
        );
        slow.join().unwrap();
        assert_eq!(events, [Event::Judged(Verdict::Right)]);
    }

    #[test]
    fn a_greetd_that_is_gone_for_good_is_reported_as_gone() {
        let events = run(
            vec![vec![(CREATE, "")]],
            vec![
                begin("live", "live"),
                begin("live", "live"),
                begin("live", "live"),
            ],
        );
        assert_eq!(
            events,
            [Event::Judged(Verdict::Unchecked(None)), Event::Gone]
        );
        // And when it was never there.
        assert_eq!(run(vec![], vec![begin("live", "live")]), [Event::Gone]);
    }

    #[test]
    fn nonsense_from_greetd_ends_the_connection_and_not_the_program() {
        let events = run(
            vec![
                vec![(CREATE, r#"{"type":"surprise"}"#)],
                vec![(CREATE, SUCCESS), (START, ASK_PASSWORD)],
            ],
            vec![begin("live", "live"), begin("live", "live")],
        );
        assert_eq!(
            events,
            [
                Event::Judged(Verdict::Unchecked(None)),
                Event::Judged(Verdict::Unchecked(None))
            ]
        );
    }
}
