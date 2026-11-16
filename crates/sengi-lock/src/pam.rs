//! Asking PAM whether a password is right.
//!
//! libpam is declared here by hand: six functions and two structs of an
//! interface that has not changed in decades, against a binding crate
//! that would bring a code generator and its tree with it.
//!
//! One call to [`authenticate`] is one PAM transaction, started and ended
//! inside it. It blocks for as long as the modules take, which with a
//! network or a fingerprint reader behind them can be long, so it is run
//! on a thread of its own and never on the one that draws.
//!
//! The password is lent to this module, not given. PAM is handed a copy,
//! as its interface demands; that copy is allocated here, belongs to PAM
//! from the moment the conversation returns, and is PAM's to wipe.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use sengi_system::secret::Secret;

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_PERM_DENIED: c_int = 6;
const PAM_AUTH_ERR: c_int = 7;
const PAM_CRED_INSUFFICIENT: c_int = 8;
const PAM_USER_UNKNOWN: c_int = 10;
const PAM_MAXTRIES: c_int = 11;
const PAM_NEW_AUTHTOK_REQD: c_int = 12;
const PAM_ACCT_EXPIRED: c_int = 13;
const PAM_CONV_ERR: c_int = 19;

const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;

const PAM_REFRESH_CRED: c_int = 0x10;

/// The most messages one call of the conversation may carry.
const PAM_MAX_NUM_MSG: c_int = 32;

/// The longest message from a module that is kept, in characters.
const MAX_SAID: usize = 120;

#[repr(C)]
struct Message {
    style: c_int,
    text: *const c_char,
}

#[repr(C)]
struct Response {
    text: *mut c_char,
    retcode: c_int,
}

type Converse =
    unsafe extern "C" fn(c_int, *mut *const Message, *mut *mut Response, *mut c_void) -> c_int;

#[repr(C)]
struct Conversation {
    converse: Option<Converse>,
    data: *mut c_void,
}

#[repr(C)]
struct Handle {
    _opaque: [u8; 0],
}

#[link(name = "pam")]
extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conversation: *const Conversation,
        handle: *mut *mut Handle,
    ) -> c_int;
    fn pam_authenticate(handle: *mut Handle, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut Handle, flags: c_int) -> c_int;
    fn pam_setcred(handle: *mut Handle, flags: c_int) -> c_int;
    fn pam_end(handle: *mut Handle, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut Handle, status: c_int) -> *const c_char;
}

/// What PAM made of a password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The password is right and the account may be used.
    Granted,
    /// The password is wrong.
    Denied,
    /// The password is right but the account is closed: expired, or
    /// barred by a module.
    Barred,
    /// PAM could not say: no configuration for the service, a module that
    /// failed. Carries what there is to tell the user, already safe to
    /// show; the details go to standard error.
    Unavailable(Option<String>),
}

/// What the conversation function works from.
struct Talk<'a> {
    password: &'a [u8],
    /// The last thing a module wanted the user told.
    said: Option<String>,
}

/// A copy of `bytes` as a C string from `calloc`, which is what PAM
/// frees its answers with. Null if there is no memory.
unsafe fn c_copy(bytes: &[u8]) -> *mut c_char {
    let copy = libc::calloc(bytes.len() + 1, 1) as *mut u8;
    if !copy.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len());
    }
    copy.cast()
}

/// Wipes and frees answers that PAM will never see.
unsafe fn discard(answers: *mut Response, count: usize) {
    for index in 0..count {
        let text = (*answers.add(index)).text;
        if !text.is_null() {
            let length = libc::strlen(text);
            for offset in 0..length {
                std::ptr::write_volatile(text.add(offset), 0);
            }
            libc::free(text.cast());
        }
    }
    libc::free(answers.cast());
}

/// PAM's questions, answered. A prompt that does not echo is the one for
/// the password and gets it. One that echoes is asking for something
/// this screen does not have, a user name or a code, and gets an empty
/// answer. Messages are kept for the user and need no answer.
unsafe extern "C" fn converse(
    count: c_int,
    messages: *mut *const Message,
    answers_out: *mut *mut Response,
    data: *mut c_void,
) -> c_int {
    if count <= 0
        || count > PAM_MAX_NUM_MSG
        || messages.is_null()
        || answers_out.is_null()
        || data.is_null()
    {
        return PAM_CONV_ERR;
    }
    let count = count as usize;
    let talk = &mut *(data as *mut Talk);
    let answers = libc::calloc(count, std::mem::size_of::<Response>()) as *mut Response;
    if answers.is_null() {
        return PAM_BUF_ERR;
    }
    for index in 0..count {
        let message = *messages.add(index);
        if message.is_null() {
            discard(answers, count);
            return PAM_CONV_ERR;
        }
        let answer = match (*message).style {
            PAM_PROMPT_ECHO_OFF => c_copy(talk.password),
            PAM_PROMPT_ECHO_ON => c_copy(b""),
            PAM_ERROR_MSG | PAM_TEXT_INFO => {
                if !(*message).text.is_null() {
                    let text = CStr::from_ptr((*message).text).to_string_lossy();
                    let text = sengi_text::line(&text, MAX_SAID);
                    if !text.is_empty() {
                        talk.said = Some(text);
                    }
                }
                continue;
            }
            _ => {
                discard(answers, count);
                return PAM_CONV_ERR;
            }
        };
        if answer.is_null() {
            discard(answers, count);
            return PAM_BUF_ERR;
        }
        (*answers.add(index)).text = answer;
    }
    *answers_out = answers;
    PAM_SUCCESS
}

fn describe(handle: *mut Handle, status: c_int) -> String {
    // SAFETY: pam_strerror returns a static string or null.
    let text = unsafe { pam_strerror(handle, status) };
    if text.is_null() {
        format!("PAM error {status}")
    } else {
        // SAFETY: not null, and NUL-terminated.
        let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
        format!("{text} (PAM error {status})")
    }
}

/// Asks the PAM service `service` whether `password` is `user`'s, and
/// whether the account may be used. Blocks until PAM answers.
pub fn authenticate(service: &str, user: &str, password: &Secret) -> Outcome {
    let (Ok(service), Ok(user)) = (CString::new(service), CString::new(user)) else {
        return Outcome::Unavailable(None);
    };
    let mut talk = Talk {
        password: password.expose().as_bytes(),
        said: None,
    };
    let conversation = Conversation {
        converse: Some(converse),
        data: (&mut talk as *mut Talk).cast(),
    };
    let mut handle: *mut Handle = std::ptr::null_mut();
    // SAFETY: the strings and the conversation outlive the transaction,
    // which ends with pam_end below, and `talk` is only touched by PAM
    // from inside the calls made here, on this thread.
    unsafe {
        let started = pam_start(service.as_ptr(), user.as_ptr(), &conversation, &mut handle);
        if started != PAM_SUCCESS || handle.is_null() {
            eprintln!(
                "sengi-lock: PAM would not start: {}",
                describe(std::ptr::null_mut(), started)
            );
            return Outcome::Unavailable(None);
        }
        let mut status = pam_authenticate(handle, 0);
        let authenticated = status == PAM_SUCCESS;
        if authenticated {
            status = pam_acct_mgmt(handle, 0);
            // The password has run out and wants changing. That cannot be
            // done from here, and the session it would lock the owner out
            // of is already theirs.
            if status == PAM_NEW_AUTHTOK_REQD {
                status = PAM_SUCCESS;
            }
        }
        if status == PAM_SUCCESS {
            // For modules that hold tickets with a lifetime, as swaylock
            // does. A failure here does not undo a right password.
            let _ = pam_setcred(handle, PAM_REFRESH_CRED);
        }
        let outcome = match status {
            PAM_SUCCESS => Outcome::Granted,
            _ if authenticated => {
                eprintln!(
                    "sengi-lock: the account was refused: {}",
                    describe(handle, status)
                );
                Outcome::Barred
            }
            PAM_AUTH_ERR
            | PAM_USER_UNKNOWN
            | PAM_MAXTRIES
            | PAM_PERM_DENIED
            | PAM_CRED_INSUFFICIENT
            | PAM_ACCT_EXPIRED => Outcome::Denied,
            _ => {
                eprintln!(
                    "sengi-lock: the password could not be checked: {}",
                    describe(handle, status)
                );
                Outcome::Unavailable(talk.said.take())
            }
        };
        pam_end(handle, status);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the conversation the way a module would.
    fn ask(talk: &mut Talk, styles: &[(c_int, &str)]) -> Result<Vec<Option<String>>, c_int> {
        let texts: Vec<CString> = styles
            .iter()
            .map(|(_, text)| CString::new(*text).unwrap())
            .collect();
        let messages: Vec<Message> = styles
            .iter()
            .zip(&texts)
            .map(|((style, _), text)| Message {
                style: *style,
                text: text.as_ptr(),
            })
            .collect();
        let mut pointers: Vec<*const Message> =
            messages.iter().map(|m| m as *const Message).collect();
        let mut answers: *mut Response = std::ptr::null_mut();
        // SAFETY: laid out as PAM lays it out.
        unsafe {
            let status = converse(
                pointers.len() as c_int,
                pointers.as_mut_ptr(),
                &mut answers,
                (talk as *mut Talk).cast(),
            );
            if status != PAM_SUCCESS {
                assert!(answers.is_null());
                return Err(status);
            }
            let read = (0..styles.len())
                .map(|index| {
                    let text = (*answers.add(index)).text;
                    (!text.is_null()).then(|| CStr::from_ptr(text).to_str().unwrap().to_owned())
                })
                .collect();
            discard(answers, styles.len());
            Ok(read)
        }
    }

    #[test]
    fn only_a_prompt_that_hides_its_answer_gets_the_password() {
        let mut talk = Talk {
            password: "s3cr\u{e9}t".as_bytes(),
            said: None,
        };
        let answers = ask(
            &mut talk,
            &[
                (PAM_PROMPT_ECHO_OFF, "Password: "),
                (PAM_PROMPT_ECHO_ON, "login: "),
                (PAM_TEXT_INFO, "Place your finger\non the reader"),
                (PAM_ERROR_MSG, ""),
            ],
        )
        .unwrap();
        assert_eq!(
            answers,
            [
                Some("s3cr\u{e9}t".to_string()),
                Some(String::new()),
                None,
                None
            ]
        );
        // Kept on one line, and an empty message does not replace it.
        assert_eq!(
            talk.said.as_deref(),
            Some("Place your finger on the reader")
        );
    }

    #[test]
    fn a_conversation_it_does_not_understand_is_refused() {
        let mut talk = Talk {
            password: b"x",
            said: None,
        };
        assert_eq!(
            ask(&mut talk, &[(PAM_PROMPT_ECHO_OFF, "Password: "), (99, "?")]),
            Err(PAM_CONV_ERR)
        );
        assert_eq!(ask(&mut talk, &[]), Err(PAM_CONV_ERR));
        let too_many = vec![(PAM_TEXT_INFO, "x"); 33];
        assert_eq!(ask(&mut talk, &too_many), Err(PAM_CONV_ERR));
    }

    #[test]
    fn a_service_with_no_configuration_never_grants() {
        // Falls through to PAM's "other" service, which denies, or fails
        // outright where PAM is not set up at all. Never a yes.
        let password = Secret::new("anything".to_string());
        let outcome = authenticate("sengi-lock-no-such-service", "nobody", &password);
        assert_ne!(outcome, Outcome::Granted);
    }
}
