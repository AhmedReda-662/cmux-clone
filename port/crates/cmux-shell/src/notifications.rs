//! Desktop-notification OSC recognition, lifted from the PTY byte stream
//! one buffer at a time (M3.1, ADR-0011).
//!
//! Wire contract (verified against upstream's parser mirror
//! `cmux-main/Packages/macOS/CmuxRemoteSession/Sources/CmuxRemoteSession/
//! RemoteTmux/RemoteTmuxNotificationOSCFilter.swift`):
//!
//! - `ESC ] 777;notify;<title>;<body> BEL|ST` (rxvt/wezterm style), or
//! - `ESC ] 9;<body> BEL|ST` (iTerm2 style; no title at wire level).
//!
//! Terminator is BEL (`0x07`) or ST (`ESC \`); non-`notify` OSC 777
//! subcommands and every other OSC are skipped without affecting the
//! stream — the raw bytes still reach the terminal untouched (the engine
//! ignores what it does not render).
//!
//! The scanner is a pure state machine: `scan` accepts arbitrarily
//! fragmented buffers (a sequence may split anywhere, even inside a
//! UTF-8 title, or across an ESC/`\` terminator pair) and carries only
//! the partial sequence.

/// One recognized desktop notification from an OSC sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscNotification {
    /// Empty for OSC 9 events (they carry no title at wire level).
    pub title: String,
    pub body: String,
}

/// The ESC byte that opens escape sequences.
const ESC: u8 = 0x1B;
/// The BEL byte that terminates (most) OSC sequences.
const BEL: u8 = 0x07;
/// The `\` byte that, following ESC, forms ST (`ESC \`).
const BACKSLASH: u8 = 0x5C;
/// The `]` byte that, following ESC, opens an OSC.
const OS_BRACKET: u8 = 0x5D;

/// Give up on a sequence whose payload would grow past this: the
/// terminator is effectively lost and the content is not worth an
/// unbounded buffer. (Upstream's OSC filter uses the same ceiling.)
const MAX_PAYLOAD_BYTES: usize = 4096;

/// A parser that finds terminal-initiated desktop notifications in a
/// fragmented PTY output stream.
#[derive(Debug, Default)]
pub struct NotificationScanner {
    state: State,
    /// Accumulating bytes of the current section (selector digits, a
    /// param word, the title, or the body — the filled section depends
    /// on [`State`]).
    payload: Vec<u8>,
    /// An OSC 777 title, held aside between `notify;` and the following
    /// `;` while `payload` collects the body.
    title: Vec<u8>,
    /// An `ESC` seen mid-sequence: the next byte decides whether it was
    /// ST (`ESC \`) or payload noise.
    esc_pending: bool,
}

#[derive(Debug, Default, PartialEq)]
enum State {
    /// Between sequences; only an `ESC` matters.
    #[default]
    BeforeOsc,
    /// `ESC ]` seen; accumulating the numeric selector until `;`.
    Intro,
    /// `777;` seen; accumulating the first param word (only `notify`
    /// continues to a title/body payload).
    Param,
    /// `notify;` seen; `payload` collects the title until `;`.
    Title,
    /// Title collected; `payload` collects the body until BEL/ST.
    Body,
    /// `9;` seen; `payload` collects the body until BEL/ST.
    Body9,
    /// Inside a sequence we do not report; watch only the terminator so
    /// its BEL cannot be mistaken for a later sequence's introducer.
    Skip,
    /// A payload overflowed; drop the event but keep watching the
    /// terminator so the next sequence parses cleanly.
    Overflown,
}

impl NotificationScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one buffer and returns every complete notification it
    /// contained. The bytes are not consumed — the caller still forwards
    /// them to the terminal emulator.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<OscNotification> {
        let mut events = Vec::new();
        for &byte in bytes {
            if let Some(event) = self.step(byte) {
                events.push(event);
            }
        }
        events
    }

    /// Steps one byte; `Some` when this byte closed a notification
    /// sequence.
    fn step(&mut self, byte: u8) -> Option<OscNotification> {
        if self.esc_pending {
            self.esc_pending = false;
            if byte == BACKSLASH {
                return self.terminator();
            }
            if byte == OS_BRACKET {
                // `ESC ]` opens a new OSC — only when the stream is between
                // sequences (inside an unterminated one it is payload text;
                // upstream abandons the candidate).
                if self.state == State::BeforeOsc {
                    self.reset();
                    self.state = State::Intro;
                } else {
                    self.abandon_candidate();
                }
                return None;
            }
            if byte == ESC {
                // A held ESC followed by another ESC: upstream replays any
                // candidate payload and holds the fresh ESC.
                self.abandon_candidate();
                self.esc_pending = true;
                return None;
            }
            // ESC + anything else is malformed inside a candidate (upstream
            // replays the bytes as text); the byte itself is consumed.
            self.abandon_candidate();
            return None;
        }

        if byte == ESC {
            self.esc_pending = true;
            return None;
        }
        if byte == BEL {
            return self.terminator();
        }

        match self.state {
            State::BeforeOsc => None,
            State::Intro => match byte {
                b'0'..=b'9' => {
                    self.push_payload(byte);
                    None
                }
                b';' => {
                    // Byte-exact selector (upstream prefix-matches `9;` and
                    // `777;notify;`; `09;` there is not a notification).
                    self.state = match self.payload.as_slice() {
                        b"9" => State::Body9,
                        b"777" => State::Param,
                        _ => State::Skip,
                    };
                    self.payload.clear();
                    None
                }
                _ => {
                    self.state = State::Skip;
                    None
                }
            },
            State::Param => match byte {
                b';' => {
                    let is_notify = self.payload.as_slice() == b"notify";
                    self.payload.clear();
                    self.state = if is_notify { State::Title } else { State::Skip };
                    None
                }
                _ => {
                    self.push_payload(byte);
                    None
                }
            },
            State::Title => match byte {
                b';' => {
                    self.title = std::mem::take(&mut self.payload);
                    self.state = State::Body;
                    None
                }
                _ => {
                    self.push_payload(byte);
                    None
                }
            },
            State::Body | State::Body9 => {
                self.push_payload(byte);
                None
            }
            State::Skip | State::Overflown => None,
        }
    }

    /// BEL or ST closed a sequence; a notification is complete only in
    /// the section states that had collected content.
    fn terminator(&mut self) -> Option<OscNotification> {
        let event = match self.state {
            State::Title => {
                let title = std::mem::take(&mut self.payload);
                Some(OscNotification {
                    title: lossy(&title),
                    body: String::new(),
                })
            }
            State::Body | State::Body9 => {
                let body = std::mem::take(&mut self.payload);
                let title = std::mem::take(&mut self.title);
                Some(OscNotification {
                    title: lossy(&title),
                    body: lossy(&body),
                })
            }
            _ => None,
        };
        self.reset();
        event
    }

    /// A bare ESC mid-candidate is malformed: everything collected up to
    /// it is dropped (upstream replays it as text instead of stripping).
    fn abandon_candidate(&mut self) {
        if matches!(
            self.state,
            State::Intro | State::Param | State::Title | State::Body | State::Body9
        ) {
            self.reset();
        }
    }

    fn push_payload(&mut self, byte: u8) {
        if self.payload.len() >= MAX_PAYLOAD_BYTES {
            self.payload.clear();
            self.title.clear();
            self.state = State::Overflown;
            return;
        }
        self.payload.push(byte);
    }

    fn reset(&mut self) {
        self.state = State::BeforeOsc;
        self.payload.clear();
        self.title.clear();
        self.esc_pending = false;
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one sequence through the scanner both as one buffer and split
    /// at every byte, asserting the same outcome.
    fn assert_sequence(bytes: &[u8], expected: Vec<OscNotification>) {
        let mut one_buffer = NotificationScanner::new();
        assert_eq!(one_buffer.scan(bytes), expected, "unsplit");

        for split in 1..bytes.len() {
            let mut scanner = NotificationScanner::new();
            let mut events = scanner.scan(&bytes[..split]);
            events.extend(scanner.scan(&bytes[split..]));
            assert_eq!(
                events, expected,
                "splitting at byte {split} must not change the result"
            );
        }
    }

    #[test]
    fn recognizes_osc9_with_bel() {
        assert_sequence(
            b"\x1b]9;backend finished\x07",
            Vec::from([OscNotification {
                title: String::new(),
                body: "backend finished".to_owned(),
            }]),
        );
    }

    #[test]
    fn recognizes_osc9_with_st_terminator() {
        assert_sequence(
            b"\x1b]9;backend finished\x1b\\",
            Vec::from([OscNotification {
                title: String::new(),
                body: "backend finished".to_owned(),
            }]),
        );
    }

    #[test]
    fn recognizes_osc777_notify_title_body() {
        assert_sequence(
            b"\x1b]777;notify;Task 42;All checks passed\x07",
            Vec::from([OscNotification {
                title: "Task 42".to_owned(),
                body: "All checks passed".to_owned(),
            }]),
        );
    }

    #[test]
    fn ignores_non_notify_osc777_subcommands() {
        assert_sequence(b"\x1b]777;cursor;#00ff00\x07", Vec::new());
        assert_sequence(b"\x1b]777;clipboard;text\x1b\\", Vec::new());
    }

    #[test]
    fn ignores_other_osc_selectors() {
        assert_sequence(b"\x1b]0;host\x07", Vec::new());
        assert_sequence(b"\x1b]133;A\x07", Vec::new());
        assert_sequence(b"\x1b]7;file:///tmp\x07", Vec::new());
        assert_sequence(b"\x1b]8;;link\x1b\\", Vec::new());
    }

    #[test]
    fn titleless_osc777_keeps_the_payload_as_the_title() {
        // `notify;` followed directly by a terminator: what arrived is
        // the title and there is no body section.
        assert_sequence(
            b"\x1b]777;notify;Only a title\x07",
            Vec::from([OscNotification {
                title: "Only a title".to_owned(),
                body: String::new(),
            }]),
        );
    }

    #[test]
    fn osc777_empty_title_still_carries_the_body() {
        assert_sequence(
            b"\x1b]777;notify;;body only\x07",
            Vec::from([OscNotification {
                title: String::new(),
                body: "body only".to_owned(),
            }]),
        );
    }

    #[test]
    fn bodies_keep_semicolons() {
        assert_sequence(
            b"\x1b]777;notify;t;a;b;c\x07",
            Vec::from([OscNotification {
                title: "t".to_owned(),
                body: "a;b;c".to_owned(),
            }]),
        );
        assert_sequence(
            b"\x1b]9;1;2;3\x07",
            Vec::from([OscNotification {
                title: String::new(),
                body: "1;2;3".to_owned(),
            }]),
        );
    }

    #[test]
    fn several_events_in_one_buffer() {
        assert_sequence(
            b"\x1b]9;first\x07== between ==\x1b]777;notify;A;B\x07",
            Vec::from([
                OscNotification {
                    title: String::new(),
                    body: "first".to_owned(),
                },
                OscNotification {
                    title: "A".to_owned(),
                    body: "B".to_owned(),
                },
            ]),
        );
    }

    #[test]
    fn ordinary_output_yields_nothing() {
        assert_sequence(
            b"user@host $ ls \x1b[32m\x1b[0m\r\nplain \x9b paragraph\x07",
            Vec::new(),
        );
    }

    #[test]
    fn utf8_title_split_mid_character() {
        // é (0xC3 0xA9) and → (0xE2 0x86 0x92) split across scan calls.
        let mut scanner = NotificationScanner::new();
        let mut events = scanner.scan(b"\x1b]777;notify;\xc3\xa9l");
        events.extend(scanner.scan("lo→;done\x07".as_bytes()));
        assert_eq!(
            events,
            Vec::from([OscNotification {
                title: "éllo→".to_owned(),
                body: "done".to_owned(),
            }])
        );
    }

    #[test]
    fn selector_must_be_byte_exact() {
        // Upstream prefix-matches `9;` — `09;` is not a notification.
        assert_sequence(b"\x1b]09;two\x07", Vec::new());
        // A jumbo selector never matches `777`.
        assert_sequence(b"\x1b]7779;notify;a;b\x07", Vec::new());
        // No `;` at all: `ESC ] 9 BEL` passes through untouched.
        assert_sequence(b"\x1b]9\x07", Vec::new());
    }

    #[test]
    fn bare_esc_inside_a_payload_abandons_the_candidate() {
        // Upstream treats the bare ESC as malformed: everything up to it
        // is replayed as text and nothing opens — not even the `]` pair.
        assert_sequence(b"\x1b]9;abc\x1b]9;def\x07", Vec::new());
    }

    #[test]
    fn double_esc_inside_a_payload_reopens_the_scan() {
        // `ESC ESC` replays the candidate and holds a fresh ESC, so the
        // `ESC ]` pair after it opens a new sequence.
        assert_sequence(
            b"\x1b]9;abc\x1b\x1b]9;def\x07",
            Vec::from([OscNotification {
                title: String::new(),
                body: "def".to_owned(),
            }]),
        );
    }

    #[test]
    fn overflowing_payload_is_abandoned_but_the_machine_survives() {
        let mut scanner = NotificationScanner::new();
        let long = "x".repeat(MAX_PAYLOAD_BYTES + 100);
        let mut bytes = b"\x1b]9;".to_vec();
        bytes.extend_from_slice(long.as_bytes());
        bytes.push(BEL);
        assert!(scanner.scan(&bytes).is_empty());

        // The next sequence still parses cleanly.
        let events = scanner.scan(b"\x1b]9;short\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].body, "short");
    }

    /// BEL inside a skipped sequence's payload is the terminator, not an
    /// introducer: the following notification must still parse.
    #[test]
    fn bel_terminating_a_skipped_sequence_does_not_leak() {
        let mut scanner = NotificationScanner::new();
        let events = scanner.scan(b"\x1b]0;user drops BEL\x07\x1b]9;real\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].body, "real");
    }
}
