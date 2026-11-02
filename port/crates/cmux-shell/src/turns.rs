//! Prompt-line turn detection (M3.4, ADR-0011 Amendment 6): a Rust
//! translation of upstream's local PTY-stream detector
//! (`cmux-main/Packages/macOS/CmuxTerminalCore/Sources/CmuxTerminalCore/
//! PromptDetection/PromptLineTurnDetector.swift`), validated line-for-line
//! against upstream's own test suite ported below. Upstream attaches this
//! per surface to agent definitions that publish an exact idle prompt — the
//! notification-parity fallback for hook-less agents (the hook path already
//! covers claude/codex; upstream's only built-in with an attached prompt
//! configuration is Ollama, `cmux-main/Sources/
//! CmuxTaskManagerCodingAgentDefinition+BuiltIns.swift:71-86`).
//!
//! Out of scope (feature-parity row 10 records the split): upstream's
//! separate daemon plugin (`cmux-tui/bindings/examples/
//! rust-agent-screen-detection`) drives sidebar *badges* from rendered
//! viewport manifests, never notifications, and needs a Unix-socket SDK
//! transport the port does not have.
//!
//! Semantics, all verified against the Swift source and its tests
//! (`Packages/macOS/CmuxTerminalCore/Tests/CmuxTerminalCoreTests/
//! PromptLineTurnDetectorTests.swift`):
//!
//! - The detector becomes ready after seeing the configured prompt on a
//!   logical line; it then requires a non-empty echoed submission and
//!   subsequent output before a later prompt can complete a turn.
//! - The exact prompt first becomes a *pending* boundary (monotonic
//!   identifier); confirmations after the configured debounce consume it,
//!   and visible bytes on the same logical line invalidate it. A
//!   superseded candidate re-creates the pending later on the same logical
//!   line once it exactly matches a waiting-prompt line again (upstream's
//!   Ollama tests rely on this).
//! - ANSI CSI and OSC sequences are ignored; carriage returns, backspaces,
//!   and full-line CSI rewrites (`CSI G`/`CSI 1G`/`CSI 2K`) update or reset
//!   the logical line; oversized lines latch an overflow snapshot that
//!   keeps turn semantics across the storage cap (fail-closed on erase).
//! - Whole printable runs are skipped once the line can no longer
//!   influence detection, keeping the reader thread near memcpy cost on
//!   surfaces that never show an agent prompt.
//!
//! [`TurnRuntime`] mirrors upstream's per-surface tee timing
//! (`cmux-main/Sources/TerminalOutputTeeContext.swift:57-129`): a pending
//! boundary that passed its confirmation deadline is confirmed before the
//! next chunk is consumed, so the detector's phase never freezes behind a
//! delivered turn. This is derivation only — unlike upstream, the port
//! delivers from the frame loop at its own deadline with identifier-level
//! exactly-once tracking, so locally confirmed turns need no separate
//! forwarding lane.
//!
//! Divergences (ADR-0011 Amendment 6): upstream binds each turn to the
//! pane's foreground process and re-verifies it before delivering
//! (`cmux-main/Sources/PromptTurnNotificationHandler.swift:104-118`);
//! Windows/ConPTY exposes no foreground process-group query, so the port's
//! guards are pane-child liveness plus the identifier exactly-once rule.
//! `submission_count` is kept for parity but carries no PID binding.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Upstream's logical-line storage cap: an oversized pasted submission or
/// response keeps its turn semantics through the latched overflow snapshot
/// (`maximumLogicalLineBytes`, Swift line 41).
const MAX_LOGICAL_LINE_BYTES: usize = 4_096;
/// Upstream's CSI parameter cap: overlong parameter stacks are treated as
/// inert rather than growing the buffer (`maximumCSIParameterBytes`, Swift
/// line 42).
const MAX_CSI_PARAMETER_BYTES: usize = 12;

/// The interactive-agent prompt that brackets turns, plus the idle
/// placeholder lines it may display while waiting. A translation of
/// upstream's `PromptLineTurnDetectionConfiguration` (Swift lines 4-42):
/// the bare prompt itself is always a waiting line, each suffix composes
/// another one, and the confirmation delay defaults to 500 ms upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnConfig {
    prompt: Vec<u8>,
    waiting_prompt_lines: Vec<Vec<u8>>,
    /// The longest waiting line: lines past this can neither equal nor
    /// prefix a prompt (`maximumWaitingPromptLineByteCount`).
    max_waiting_prompt_line_bytes: usize,
    /// Visible (non-space) bytes in the prompt, so a prompt-prefixed line
    /// carrying only prompt-shaped content is recognized as idle rather
    /// than as a submission (`promptVisibleByteCount`).
    prompt_visible_bytes: usize,
    /// The debounce interval an exact prompt line must stay unchanged
    /// before the boundary confirms (`confirmationDelay`).
    pub confirmation_delay: Duration,
}

impl TurnConfig {
    /// Builds a configuration for one agent prompt. Mirrors upstream's
    /// `init` preconditions (non-empty prompt, non-empty suffixes, positive
    /// delay) — asserted, never silently repaired.
    pub fn new(
        prompt: &str,
        waiting_prompt_suffixes: &[&str],
        confirmation_delay: Duration,
    ) -> Self {
        assert!(
            !prompt.is_empty(),
            "A prompt-line detector requires a non-empty prompt"
        );
        assert!(
            !confirmation_delay.is_zero(),
            "A prompt-line detector requires a positive confirmation delay"
        );
        let mut waiting_prompt_lines = vec![prompt.as_bytes().to_vec()];
        for suffix in waiting_prompt_suffixes {
            assert!(
                !suffix.is_empty(),
                "Waiting prompt suffixes must be non-empty"
            );
            waiting_prompt_lines.push(format!("{prompt}{suffix}").into_bytes());
        }
        let max_waiting_prompt_line_bytes =
            waiting_prompt_lines.iter().map(Vec::len).max().unwrap_or(0);
        let prompt_visible_bytes = prompt
            .as_bytes()
            .iter()
            .filter(|&&byte| is_visible_content_byte(byte))
            .count();
        Self {
            prompt: prompt.as_bytes().to_vec(),
            waiting_prompt_lines,
            max_waiting_prompt_line_bytes,
            prompt_visible_bytes,
            confirmation_delay,
        }
    }
}

/// Which interactive agents get a prompt-line detector. Upstream's built-in
/// agent definitions attach a prompt configuration to exactly one entry
/// (`Sources/CmuxTaskManagerCodingAgentDefinition+BuiltIns.swift:71-86`);
/// the port mirrors that table until upstream extends it.
#[derive(Debug)]
pub struct BuiltinAgentDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
    pub config: TurnConfig,
}

/// The upstream built-ins carrying `promptTurnDetection`. Ollama's values
/// verbatim: prompt `">>> "`, idle placeholder
/// `"Send a message (/? for help)"`, and upstream's default 500 ms delay.
pub fn builtin_definitions() -> &'static [BuiltinAgentDefinition] {
    static BUILTINS: LazyLock<Vec<BuiltinAgentDefinition>> = LazyLock::new(|| {
        vec![BuiltinAgentDefinition {
            id: "ollama",
            display_name: "Ollama",
            config: TurnConfig::new(
                ">>> ",
                &["Send a message (/? for help)"],
                Duration::from_millis(500),
            ),
        }]
    });
    &BUILTINS
}

/// A prompt-shaped boundary awaiting its confirmation delay. Translation of
/// upstream's `PromptLineTurnConfirmation` (identifier + delay; the
/// completed-turn count is always one per identifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnConfirmation {
    pub identifier: u64,
    pub delay: Duration,
}

/// Stable phase names are `Copy` so a pending boundary can hold the phase
/// it returns to when invalidated (upstream's `pendingPrompt(previous:)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StablePhase {
    /// Before the first prompt was ever seen.
    SeekingInitialPrompt,
    /// Ready to observe an echoed submission.
    ReadyForSubmission,
    /// A submission was seen; waiting for (more) output before a turn can
    /// complete.
    AwaitingPrompt { observed_output: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Stable(StablePhase),
    /// An exact prompt line awaits confirmation; invalidation returns to
    /// the phase that preceded it.
    Pending(StablePhase),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ControlSequence {
    #[default]
    None,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

/// One turn-stream observation for the app layer (the channel between the
/// PTY reader thread and the frame loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    /// The boundary `identifier` began awaiting dispatch. A newer event
    /// supersedes (replaces) any earlier one still undelivered.
    Pending(TurnPending),
    /// That boundary was invalidated before its confirmation deadline.
    Invalidated { identifier: u64 },
}

/// The announcement payload for a pending boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnPending {
    pub identifier: u64,
    pub agent_id: &'static str,
    pub agent_name: &'static str,
    pub delay: Duration,
}

/// A faithful translation of upstream's `PromptLineTurnDetector` state
/// machine (`PromptLineTurnDetector.swift:21-456`) consuming an arbitrarily
/// fragmented PTY output stream.
#[derive(Debug)]
pub struct PromptLineTurnDetector {
    config: TurnConfig,
    phase: Phase,
    cs: ControlSequence,
    /// CSI parameter/intermediate bytes of the sequence being parsed, so
    /// the terminator can distinguish line-rewriting controls (`CSI 2K`,
    /// `CSI 1G`) from inert ones (SGR, cursor shifts). Overlong sequences
    /// are treated as inert rather than growing the buffer.
    csi_parameter_bytes: Vec<u8>,
    csi_parameter_overflowed: bool,
    logical_line: Vec<u8>,
    logical_line_overflowed: bool,
    /// Visible bytes currently in `logical_line`, maintained incrementally
    /// so per-byte evaluation stays O(1) on the read thread.
    visible_byte_count: usize,
    /// Printable bytes skipped after the line stopped mattering. They still
    /// count toward the logical line so backspace editing stays exact:
    /// while this is nonzero, backspaces consume skipped bytes before the
    /// stored (frozen, already disqualified) prefix.
    unstored_line_byte_count: usize,
    /// Snapshots taken when a logical line outgrows the storage cap.
    overflowed_line_started_with_prompt: bool,
    overflowed_submission_had_visible_content: bool,
    overflowed_line_had_visible_content: bool,
    next_confirmation_identifier: u64,

    /// Increments when an echoed submission starts a new turn. The port
    /// keeps the counter for parity; it carries no PID binding (see the
    /// module-level divergence note).
    pub submission_count: u64,
    /// The boundary awaiting debounce confirmation, if any.
    pending_confirmation: Option<TurnConfirmation>,
    /// Bumps whenever `pending_confirmation` is created or invalidated.
    pub confirmation_revision: u64,
}

impl PromptLineTurnDetector {
    pub fn new(config: TurnConfig) -> Self {
        Self {
            config,
            phase: Phase::Stable(StablePhase::SeekingInitialPrompt),
            cs: ControlSequence::None,
            csi_parameter_bytes: Vec::new(),
            csi_parameter_overflowed: false,
            logical_line: Vec::new(),
            logical_line_overflowed: false,
            visible_byte_count: 0,
            unstored_line_byte_count: 0,
            overflowed_line_started_with_prompt: false,
            overflowed_submission_had_visible_content: false,
            overflowed_line_had_visible_content: false,
            next_confirmation_identifier: 0,
            submission_count: 0,
            pending_confirmation: None,
            confirmation_revision: 0,
        }
    }

    /// The current pending boundary, if any.
    pub fn pending(&self) -> Option<&TurnConfirmation> {
        self.pending_confirmation.as_ref()
    }

    /// Confirms a prompt boundary after its debounce interval elapsed.
    /// Stale confirmations return zero, including candidates invalidated by
    /// later visible bytes on the same logical line. Returns one for a
    /// completed model turn, zero for an invalidated or already-confirmed
    /// candidate.
    pub fn confirm(&mut self, confirmation: &TurnConfirmation) -> u32 {
        let matches = self
            .pending_confirmation
            .as_ref()
            .is_some_and(|pending| pending.identifier == confirmation.identifier);
        if !matches || !matches!(self.phase, Phase::Pending(_)) {
            return 0;
        }
        self.phase = Phase::Stable(StablePhase::ReadyForSubmission);
        self.set_pending_confirmation(None);
        1
    }

    /// Consumes one PTY output chunk (arbitrary fragmentation).
    pub fn consume(&mut self, bytes: &[u8]) {
        let mut index = 0usize;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte >= 0x20
                && byte != 0x7F
                && self.cs == ControlSequence::None
                && self.printable_bytes_cannot_change_state()
            {
                // Skip the rest of the printable run without state machine
                // work; only control bytes below can change detection
                // state.
                let mut cursor = index + 1;
                while cursor < bytes.len() && bytes[cursor] >= 0x20 && bytes[cursor] != 0x7F {
                    cursor += 1;
                }
                self.unstored_line_byte_count += cursor - index;
                index = cursor;
                continue;
            }
            self.consume_byte(byte);
            index += 1;
        }
    }

    /// Whether appending printable bytes to the current line can still
    /// change any observable detector state. When false, whole printable
    /// runs are skipped, which keeps the read callback near memcpy cost on
    /// surfaces that never show the agent prompt.
    fn printable_bytes_cannot_change_state(&self) -> bool {
        let stable = match self.phase {
            Phase::Pending(_) => {
                // A pending prompt is decided byte by byte: every
                // candidate byte takes the state machine path, so a
                // suffix continuation can still complete the exact line
                // (upstream's Ollama tests rely on that re-creation).
                return false;
            }
            Phase::Stable(stable) => stable,
        };
        if !self.line_cannot_become_waiting_prompt() {
            return false;
        }
        match stable {
            StablePhase::SeekingInitialPrompt => {
                // Line content is only compared against prompt patterns
                // here, and this line is already disqualified.
                true
            }
            StablePhase::AwaitingPrompt { observed_output } => {
                // Before output is observed, the next visible byte flips
                // the flag; afterwards printable bytes are inert.
                observed_output
            }
            StablePhase::ReadyForSubmission => {
                // Printable bytes matter until the line's submission
                // shape is decided: either it diverged from the prompt
                // prefix entirely, or it already carries visible
                // submission content.
                if self.logical_line_overflowed {
                    return !self.overflowed_line_started_with_prompt
                        || self.overflowed_submission_had_visible_content;
                }
                if self.logical_line.starts_with(&self.config.prompt) {
                    return self.visible_byte_count > self.config.prompt_visible_bytes;
                }
                // Disqualified as a prompt prefix and diverged from the
                // prompt: this line can never become an echoed submission.
                true
            }
        }
    }

    fn line_cannot_become_waiting_prompt(&self) -> bool {
        if self.logical_line_overflowed || self.unstored_line_byte_count > 0 {
            return true;
        }
        if self.logical_line.len() > self.config.max_waiting_prompt_line_bytes {
            return true;
        }
        !self
            .config
            .waiting_prompt_lines
            .iter()
            .any(|waiting| waiting.starts_with(&self.logical_line))
    }

    /// One byte of stream logic. Guarded sequence states (escape/CSI/OSC)
    /// consume the byte and return, exactly as the Swift switch does; only
    /// a byte outside any sequence reaches the line machinery.
    fn consume_byte(&mut self, byte: u8) {
        match self.cs {
            ControlSequence::Escape => match byte {
                b'[' => self.cs = ControlSequence::Csi,
                b']' => self.cs = ControlSequence::Osc,
                _ => self.cs = ControlSequence::None,
            },
            ControlSequence::Csi => {
                if (0x40..=0x7E).contains(&byte) {
                    self.cs = ControlSequence::None;
                    self.handle_csi_terminator(byte);
                } else if self.csi_parameter_bytes.len() < MAX_CSI_PARAMETER_BYTES {
                    self.csi_parameter_bytes.push(byte);
                } else {
                    self.csi_parameter_overflowed = true;
                }
            }
            ControlSequence::Osc => {
                if byte == 0x07 {
                    self.cs = ControlSequence::None;
                } else if byte == 0x1B {
                    self.cs = ControlSequence::OscEscape;
                }
            }
            ControlSequence::OscEscape => {
                self.cs = if byte == b'\\' {
                    ControlSequence::None
                } else {
                    ControlSequence::Osc
                };
            }
            ControlSequence::None => match byte {
                0x1B => {
                    self.cs = ControlSequence::Escape;
                    self.csi_parameter_bytes.clear();
                    self.csi_parameter_overflowed = false;
                }
                0x0A | 0x0D => {
                    self.invalidate_pending_prompt();
                    self.handle_line_boundary();
                    self.reset_logical_line();
                }
                0x08 | 0x7F => {
                    self.invalidate_pending_prompt();
                    if self.logical_line_overflowed {
                        // Erasing from an overflowed line makes its exact
                        // content unknowable; fail closed on the latched
                        // submission snapshot so an erased oversized paste
                        // cannot count a submission at the boundary.
                        // Output-ness stays latched: the erased bytes were
                        // still visible output for turn observation.
                        self.overflowed_submission_had_visible_content = false;
                    }
                    if self.unstored_line_byte_count > 0 {
                        self.unstored_line_byte_count -= 1;
                    } else if !self.logical_line_overflowed {
                        let removed = self.logical_line.pop();
                        if removed.is_some_and(is_visible_content_byte) {
                            self.visible_byte_count -= 1;
                        }
                    }
                    self.evaluate_logical_line();
                }
                0x20..=0x7E | 0x80..=0xFF => {
                    if self.pending_confirmation.is_some() {
                        self.invalidate_pending_prompt();
                    }
                    self.append_to_logical_line(byte);
                    self.evaluate_logical_line();
                }
                _ => {}
            },
        }
    }

    fn append_to_logical_line(&mut self, byte: u8) {
        if self.logical_line_overflowed {
            if is_visible_content_byte(byte) {
                self.overflowed_line_had_visible_content = true;
                if self.overflowed_line_started_with_prompt {
                    self.overflowed_submission_had_visible_content = true;
                }
            }
            return;
        }
        if self.logical_line.len() >= MAX_LOGICAL_LINE_BYTES {
            // Overflow latch: snapshot turn semantics so an oversized
            // pasted submission or response line keeps them.
            self.overflowed_line_started_with_prompt =
                self.logical_line.starts_with(&self.config.prompt);
            self.overflowed_submission_had_visible_content = self
                .overflowed_line_started_with_prompt
                && contains_visible_content(&self.logical_line[self.config.prompt.len()..]);
            self.overflowed_line_had_visible_content =
                self.visible_byte_count > 0 || is_visible_content_byte(byte);
            self.logical_line.clear();
            self.visible_byte_count = 0;
            self.logical_line_overflowed = true;
            self.mark_output_observed();
            return;
        }
        self.logical_line.push(byte);
        if is_visible_content_byte(byte) {
            self.visible_byte_count += 1;
        }
    }

    fn evaluate_logical_line(&mut self) {
        // Fast paths: a line with skipped or overflowed bytes, or one
        // longer than every waiting-prompt pattern, can neither equal nor
        // prefix a prompt, so only output observation remains. Together
        // with the incremental visible-byte count this keeps per-byte cost
        // constant on the read thread.
        if self.unstored_line_byte_count > 0 || self.logical_line_overflowed {
            if self.current_line_has_visible_content() {
                self.mark_output_observed();
            }
            return;
        }
        if self.logical_line.len() > self.config.max_waiting_prompt_line_bytes {
            if self.visible_byte_count > 0 {
                self.mark_output_observed();
            }
            return;
        }
        let exact = self
            .config
            .waiting_prompt_lines
            .iter()
            .any(|waiting| waiting == &self.logical_line);
        let stable = match self.phase {
            Phase::Pending(_) => None,
            Phase::Stable(stable) => Some(stable),
        };
        if !(exact && stable.is_some()) {
            if !self
                .config
                .waiting_prompt_lines
                .iter()
                .any(|waiting| waiting.starts_with(&self.logical_line))
                && self.visible_byte_count > 0
            {
                self.mark_output_observed();
            }
            return;
        }
        // The incoming line is exactly a waiting line on a stable phase.
        match stable.expect("checked above") {
            StablePhase::SeekingInitialPrompt | StablePhase::ReadyForSubmission => {
                self.phase = Phase::Stable(StablePhase::ReadyForSubmission);
                return;
            }
            StablePhase::AwaitingPrompt {
                observed_output: false,
            } => {
                // A prompt redraw after an empty response: back to ready,
                // never a completed turn.
                self.phase = Phase::Stable(StablePhase::ReadyForSubmission);
                return;
            }
            StablePhase::AwaitingPrompt {
                observed_output: true,
            } => {}
        }
        self.next_confirmation_identifier = self.next_confirmation_identifier.wrapping_add(1);
        self.phase = Phase::Pending(StablePhase::AwaitingPrompt {
            observed_output: true,
        });
        self.set_pending_confirmation(Some(TurnConfirmation {
            identifier: self.next_confirmation_identifier,
            delay: self.config.confirmation_delay,
        }));
    }

    /// Handles a completed CSI sequence. REPL readline implementations
    /// (Ollama's liner among them) redraw their prompt line in place with
    /// "cursor to column 1" (`CSI G`/`CSI 1G`) and "erase entire line"
    /// (`CSI 2K`) instead of CR/LF — a model-load spinner emits dozens of
    /// such frames. Both rewrite the visible line from its start, so the
    /// logical line resets exactly as it does for a carriage return, minus
    /// the submission boundary: an erased or overwritten line was never
    /// submitted. Erase-to-right (bare `CSI K`) is not a full-line rewrite
    /// and stays inert because the detector does not track cursor columns.
    fn handle_csi_terminator(&mut self, terminator: u8) {
        let overflowed = std::mem::take(&mut self.csi_parameter_overflowed);
        let params = std::mem::take(&mut self.csi_parameter_bytes);
        if overflowed {
            return;
        }
        match terminator {
            b'G' if params.is_empty() || params == b"1" || params == b"0" => {
                self.invalidate_pending_prompt();
                self.reset_logical_line();
            }
            b'K' if params == b"2" => {
                self.invalidate_pending_prompt();
                self.reset_logical_line();
            }
            _ => {}
        }
    }

    fn invalidate_pending_prompt(&mut self) {
        let previous = match self.phase {
            Phase::Pending(previous) => previous,
            Phase::Stable(_) => return,
        };
        self.phase = Phase::Stable(previous);
        self.set_pending_confirmation(None);
    }

    fn handle_line_boundary(&mut self) {
        let stable = match self.phase {
            Phase::Pending(_) => return,
            Phase::Stable(stable) => stable,
        };
        match stable {
            StablePhase::ReadyForSubmission => {
                if self.logical_line_overflowed {
                    // A pasted submission longer than the storage cap
                    // still starts a turn when it began with the prompt
                    // and carried visible content after it.
                    if self.overflowed_line_started_with_prompt
                        && self.overflowed_submission_had_visible_content
                    {
                        self.phase = Phase::Stable(StablePhase::AwaitingPrompt {
                            observed_output: false,
                        });
                        self.submission_count = self.submission_count.wrapping_add(1);
                    }
                    return;
                }
                if !self.logical_line.starts_with(&self.config.prompt)
                    || self
                        .config
                        .waiting_prompt_lines
                        .iter()
                        .any(|waiting| waiting == &self.logical_line)
                {
                    return;
                }
                let submission_had_visible =
                    contains_visible_content(&self.logical_line[self.config.prompt.len()..]);
                if submission_had_visible {
                    self.phase = Phase::Stable(StablePhase::AwaitingPrompt {
                        observed_output: false,
                    });
                    self.submission_count = self.submission_count.wrapping_add(1);
                }
            }
            StablePhase::AwaitingPrompt { .. } => self.mark_output_observed(),
            StablePhase::SeekingInitialPrompt => {}
        }
    }

    fn current_line_has_visible_content(&self) -> bool {
        if self.logical_line_overflowed {
            return self.overflowed_line_had_visible_content;
        }
        self.visible_byte_count > 0
    }

    fn mark_output_observed(&mut self) {
        let observed_output = match self.phase {
            Phase::Stable(StablePhase::AwaitingPrompt { observed_output }) => observed_output,
            _ => return,
        };
        if observed_output
            || !self.current_line_has_visible_content()
            || self
                .config
                .waiting_prompt_lines
                .iter()
                .any(|waiting| waiting == &self.logical_line)
        {
            return;
        }
        self.phase = Phase::Stable(StablePhase::AwaitingPrompt {
            observed_output: true,
        });
    }

    fn set_pending_confirmation(&mut self, confirmation: Option<TurnConfirmation>) {
        if self.pending_confirmation == confirmation {
            return;
        }
        self.pending_confirmation = confirmation;
        self.confirmation_revision = self.confirmation_revision.wrapping_add(1);
    }

    fn reset_logical_line(&mut self) {
        self.logical_line.clear();
        self.logical_line_overflowed = false;
        self.visible_byte_count = 0;
        self.unstored_line_byte_count = 0;
        self.overflowed_line_started_with_prompt = false;
        self.overflowed_submission_had_visible_content = false;
        self.overflowed_line_had_visible_content = false;
    }
}

fn is_visible_content_byte(byte: u8) -> bool {
    byte > 0x20 && byte != 0x7F
}

fn contains_visible_content(bytes: &[u8]) -> bool {
    bytes.iter().any(|&byte| is_visible_content_byte(byte))
}

/// Per-pane timing owner around one agent's detector, mirroring upstream's
/// per-surface tee (`TerminalOutputTeeContext.swift:57-129`): a pending
/// boundary past its confirmation deadline is confirmed before the next
/// chunk is consumed, and the post-consume pending state is diffed against
/// what this runtime last announced, producing the channel events the app
/// layer delivers on.
#[derive(Debug)]
pub struct TurnRuntime {
    agent_id: &'static str,
    agent_name: &'static str,
    detector: PromptLineTurnDetector,
    /// The pending identifier this runtime last announced.
    forwarded: Option<u64>,
    /// When the announced boundary's debounce expires.
    deadline: Option<Instant>,
}

impl TurnRuntime {
    pub fn new(definition: &BuiltinAgentDefinition) -> Self {
        Self {
            agent_id: definition.id,
            agent_name: definition.display_name,
            detector: PromptLineTurnDetector::new(definition.config.clone()),
            forwarded: None,
            deadline: None,
        }
    }

    pub fn agent_id(&self) -> &'static str {
        self.agent_id
    }

    /// Feeds one PTY chunk at sample instant `now`; returns the events to
    /// publish. The production caller passes `Instant::now()`.
    pub fn consume(&mut self, bytes: &[u8], now: Instant) -> Vec<TurnEvent> {
        // Upstream's tee confirms a deadline-passed candidate before the
        // chunk is consumed. The transition is internal bookkeeping (the
        // app delivers at its own scheduled deadline), so the announced
        // slot clears without an invalidation event.
        if let (Some(deadline), Some(confirmation)) =
            (self.deadline, self.detector.pending().cloned())
        {
            if now >= deadline {
                self.detector.confirm(&confirmation);
                self.deadline = None;
                self.forwarded = None;
            }
        }

        self.detector.consume(bytes);
        let current = self.detector.pending().map(|pending| pending.identifier);
        let mut events = Vec::new();
        match (self.forwarded, current) {
            (Some(previous), Some(next)) if previous == next => {}
            (_, Some(next)) => {
                let delay = self.detector.pending().expect("pending present").delay;
                self.forwarded = Some(next);
                self.deadline = Some(now + delay);
                events.push(TurnEvent::Pending(TurnPending {
                    identifier: next,
                    agent_id: self.agent_id,
                    agent_name: self.agent_name,
                    delay,
                }));
            }
            (Some(previous), None) => {
                // Detector-rule invalidation only: the entry-confirm path
                // above resets `forwarded` itself.
                self.forwarded = None;
                self.deadline = None;
                events.push(TurnEvent::Invalidated {
                    identifier: previous,
                });
            }
            (None, None) => {}
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upstream's shared test configuration: the bare `">>> "` prompt with
    /// no suffixes and the default 500 ms delay.
    fn config_bare() -> TurnConfig {
        TurnConfig::new(">>> ", &[], Duration::from_millis(500))
    }

    /// Upstream's `readyDetector()` helper: a fresh detector fed the bare
    /// prompt (which becomes ready without creating a pending boundary).
    /// The ready state persists because no line boundary resets the stored
    /// prompt bytes.
    fn ready_detector() -> PromptLineTurnDetector {
        let mut detector = PromptLineTurnDetector::new(config_bare());
        detector.consume(b">>> ");
        assert!(detector.pending().is_none());
        detector
    }

    /// Unwrap the existing pending boundary (upstream's `#require`).
    fn pending_of(detector: &PromptLineTurnDetector) -> TurnConfirmation {
        detector
            .pending()
            .expect("expected a pending confirmation")
            .clone()
    }

    // The upstream `PromptLineTurnDetectorTests.swift` suite, ported 1:1.

    /// A streamed response followed by a confirmed prompt completes one
    /// turn (upstream `streamedResponseCompletesTurn`).
    #[test]
    fn streamed_response_completes_turn() {
        let mut detector = ready_detector();

        detector.consume(b"hel");
        detector.consume(b"lo\r\nThinking");
        detector.consume(b" about it...\r\nThe answer is 42.\r\n>>");
        detector.consume(b"> ");

        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// A response line beginning with prompt text invalidates the candidate
    /// (upstream `promptPrefixInsideResponseDoesNotCompleteTurn`).
    #[test]
    fn prompt_prefix_inside_response_does_not_complete_turn() {
        let mut detector = ready_detector();

        detector.consume(b"explain\r\nmodel output\r\n>>> ");
        let stale = pending_of(&detector);
        detector.consume(b"not a prompt");

        assert!(detector.pending().is_none());
        assert_eq!(detector.confirm(&stale), 0);
    }

    /// Regression (M3.4 live smoke): ConPTY's first-frame resize re-emits
    /// the current row plus every buffer row as `ESC[K` + CRLF pairs — the
    /// detector must digest the whole 255-byte chunk in one pass.
    #[test]
    fn conpty_resize_repaint_chunk_completes() {
        let mut detector = PromptLineTurnDetector::new(TurnConfig::new(
            ">>> ",
            &["Send a message (/? for help)"],
            Duration::from_millis(500),
        ));
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"\x1b[?25l"); // cursor hidden for the repaint
        chunk.extend_from_slice(b"\x1b[8;40;93t"); // ConPTY applies the pane grid
        chunk.extend_from_slice(b"\x1b[H"); // cursor home
        chunk.extend_from_slice(b">>> Return FINAL_OLLAMA_OK");
        for _ in 0..40 {
            chunk.extend_from_slice(b"\x1b[K\r\n"); // every row re-emitted
        }
        chunk.extend_from_slice(b"\x1b[2;1H"); // cursor back to row 2
        chunk.extend_from_slice(b"\x1b[?25h"); // cursor shown again
        detector.consume(&chunk);
    }

    /// An approved idle placeholder completes a current Ollama turn
    /// (upstream `currentOllamaIdlePlaceholderCompletesTurn`).
    #[test]
    fn current_ollama_idle_placeholder_completes_turn() {
        let mut detector = PromptLineTurnDetector::new(TurnConfig::new(
            ">>> ",
            &["Send a message (/? for help)"],
            Duration::from_millis(500),
        ));

        detector.consume(b">>> Send a message (/? for help)");
        detector.consume(b"\r>>> Return FINAL_OLLAMA_OK\r\nFINAL_OLLAMA_OK\r\n");
        detector.consume(b">>> Send a message (/? for help)");

        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// Typing echo without a submitted response never completes a turn
    /// (upstream `typingEchoDoesNotCompleteTurn`).
    #[test]
    fn typing_echo_does_not_complete_turn() {
        let mut detector = ready_detector();

        detector.consume(b"explain >>> please");
        detector.consume(b"\x08\x08se");

        assert!(detector.pending().is_none());
    }

    /// A prompt redraw after an empty response is not a completion
    /// (upstream `promptWithoutModelOutputDoesNotCompleteTurn`).
    #[test]
    fn prompt_without_model_output_does_not_complete_turn() {
        let mut detector = ready_detector();

        detector.consume(b"hello\r\n>>> ");
        assert!(detector.pending().is_none());
    }

    /// ANSI spinner redraws count as output but cannot impersonate the
    /// prompt (upstream `ansiSpinnerFramesAreHandledConservatively`).
    #[test]
    fn ansi_spinner_frames_are_handled_conservatively() {
        let mut detector = ready_detector();
        let stream = b"summarize\r\n\x1B[2K\r\xe2\xa0\x8b loading\x1B[2K\r\xe2\xa0\x99 loading\x1B[2K\rDone.\r\n>>> ";

        detector.consume(stream);
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);

        detector.consume(b"still waiting at the prompt");
        assert!(detector.pending().is_none());
    }

    /// Prompt text inside an OSC title is ignored (upstream
    /// `oscPayloadCannotCompleteTurn`).
    #[test]
    fn osc_payload_cannot_complete_turn() {
        let mut detector = ready_detector();
        let stream = b"title\r\n\x1b]0;>>> \x07response\r\n>>> ";

        detector.consume(stream);
        let confirmation = pending_of(&detector);

        assert_eq!(detector.confirm(&confirmation), 1);
    }

    /// Each echoed submission increments the submission count once
    /// (upstream `submissionCountTracksEchoedSubmissions`).
    #[test]
    fn submission_count_tracks_echoed_submissions() {
        let mut detector = ready_detector();
        assert_eq!(detector.submission_count, 0);

        detector.consume(b"first\r\n");
        assert_eq!(detector.submission_count, 1);

        detector.consume(b"output\r\n>>> ");
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
        assert_eq!(detector.submission_count, 1);

        detector.consume(b"second\r\n");
        assert_eq!(detector.submission_count, 2);
    }

    /// A pathological run of invisible bytes cannot wedge turn detection
    /// (upstream `longInvisiblePrefixLineRemainsDetectable`).
    #[test]
    fn long_invisible_prefix_line_remains_detectable() {
        let mut detector = ready_detector();
        detector.consume(b"ask\r\n");

        detector.consume(&[b' '; 8_192]);
        assert!(detector.pending().is_none());

        detector.consume(b"\r\nvisible tail\r\n>>> ");
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// Backspaced typing that restores the prompt can still confirm a turn
    /// (upstream `backspaceRestoresPromptCandidate`).
    #[test]
    fn backspace_restores_prompt_candidate() {
        let mut detector = ready_detector();
        detector.consume(b"ask\r\nanswer\r\n");

        detector.consume(b">>> x");
        assert!(detector.pending().is_none());

        detector.consume(b"\x7F");
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// A pasted submission longer than the storage cap still starts a turn
    /// (upstream `oversizedPastedSubmissionStartsTurn`).
    #[test]
    fn oversized_pasted_submission_starts_turn() {
        let mut detector = ready_detector();

        detector.consume(&[b'a'; 5_000]);
        detector.consume(b"\r\n");
        assert_eq!(detector.submission_count, 1);

        detector.consume(b"output\r\n>>> ");
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// Backspacing an overflowed submission fails closed at the boundary
    /// (upstream `backspacedOverflowedSubmissionDoesNotStartTurn`).
    #[test]
    fn backspaced_overflowed_submission_does_not_start_turn() {
        let mut detector = ready_detector();

        // Spaces after the prompt store without tripping the printable-run
        // skip (visible count stays at the prompt's), so the line genuinely
        // overflows the storage cap. A visible byte then latches the
        // overflowed-submission snapshot.
        detector.consume(&[b' '; 5_000]);
        detector.consume(b"a");
        // Erasing from an overflowed line makes its exact content
        // unknowable, so the boundary must not count a submission from the
        // latched snapshot.
        detector.consume(b"\x7F");
        detector.consume(b"\r");
        assert_eq!(detector.submission_count, 0);
    }

    /// An oversized response line still counts as observed output
    /// (upstream `oversizedOutputLineMarksObservedOutput`).
    #[test]
    fn oversized_output_line_marks_observed_output() {
        let mut detector = ready_detector();
        detector.consume(b"ask\r\n");

        detector.consume(&[b'x'; 5_000]);
        detector.consume(b"\r\n>>> ");

        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// A long preamble line does not affect initial prompt detection
    /// (upstream `longPreambleLineStillDetectsPrompt`).
    #[test]
    fn long_preamble_line_still_detects_prompt() {
        let mut detector = PromptLineTurnDetector::new(config_bare());

        detector.consume(&"log ".repeat(2_000).into_bytes());
        detector.consume(b"\r\n");
        detector.consume(b">>> ");
        detector.consume(b"hi\r\nanswer\r\n>>> ");

        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// Backspaces across skipped bytes keep line editing exact
    /// (upstream `backspacesAcrossSkippedBytesStayExact`).
    #[test]
    fn backspaces_across_skipped_bytes_stay_exact() {
        let mut detector = ready_detector();

        detector.consume(b"hello");
        detector.consume(&[b'\x7F'; 5]);
        detector.consume(b"ok\r\n");
        assert_eq!(detector.submission_count, 1);

        detector.consume(b"out\r\n>>> ");
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// A submission padded past the cap with spaces still starts a turn
    /// (upstream `overflowedPaddedSubmissionStartsTurn`).
    #[test]
    fn overflowed_padded_submission_starts_turn() {
        let mut detector = ready_detector();

        detector.consume(&[b' '; 5_000]);
        detector.consume(b"hello\r\n");
        assert_eq!(detector.submission_count, 1);

        detector.consume(b"output\r\n>>> ");
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// Visible output after an invisible overflow still completes the turn
    /// (upstream `visibleOutputAfterInvisibleOverflowCompletesTurn`).
    #[test]
    fn visible_output_after_invisible_overflow_completes_turn() {
        let mut detector = ready_detector();
        detector.consume(b"ask\r\n");

        detector.consume(&[b' '; 5_000]);
        detector.consume(b"done\r\n>>> ");

        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// Spinner redraws via CSI 1G/2K do not poison Ollama prompt detection
    /// (upstream `spinnerRedrawsDoNotPoisonPromptDetection`). Byte idioms
    /// captured from a real `ollama run` PTY: the model-load spinner
    /// redraws its braille frame in place with CSI 1G + CSI K and never
    /// emits CR/LF, then the idle prompt is drawn after CSI 2K.
    #[test]
    fn spinner_redraws_do_not_poison_ollama_prompt_detection() {
        let mut detector = PromptLineTurnDetector::new(TurnConfig::new(
            ">>> ",
            &["Send a message (/? for help)"],
            Duration::from_millis(500),
        ));

        detector.consume(b"Last login: Sat Jul 11 19:53:41 on ttys151\r\n");
        for frame in [
            &b"\xe2\xa0\x99"[..], // ⠙
            &b"\xe2\xa0\xb9"[..], // ⠹
            &b"\xe2\xa0\xb8"[..], // ⠸
            &b"\xe2\xa0\xbc"[..], // ⠼
        ] {
            let mut bytes = b"\x1B[?2026h\x1B[?25l\x1B[1G".to_vec();
            bytes.extend_from_slice(frame);
            bytes.extend_from_slice(b" \x1B[K\x1B[?25h\x1B[?2026l");
            detector.consume(&bytes);
        }
        detector.consume(b"\x1B[2K\x1B[1G\x1B[?25h\x1B[?2004h>>> \x1B[38;5;245mSend a message (/? for help)\x1B[28D\x1B[0m");
        detector.consume(b"\x1B[Kname one metal, one word\r\n");
        assert_eq!(detector.submission_count, 1);

        detector.consume(b"\x1B[?2026h\x1B[?25l\x1B[1G\xe2\xa0\x99 \x1B[K\x1B[?25h\x1B[?2026l");
        detector.consume(b"\x1B[2K\x1B[1G\x1B[38;5;245m\x1B[1mThinking...\r\n");
        detector.consume(b"\x1B[0mIron.\r\n\r\n");
        detector.consume(b"\x1B[?2026h\x1B[?25l\x1B[1G\xe2\xa0\x99 \x1B[K\x1B[?25h\x1B[?2026l");
        detector.consume(b"\x1B[2K\x1B[1G\x1B[?25h>>> \x1B[38;5;245mSend a message (/? for help)\x1B[28D\x1B[0m\x1B[K");

        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    // Port-specific coverage (no upstream counterpart): fragmentation and
    // runtime behavior.

    /// Byte-fragmented input reaches the same state as unfragmented input:
    /// chunk boundaries may split the prompt, escape sequences, and line
    /// boundaries arbitrarily.
    #[test]
    fn fragmented_input_reaches_the_same_state() {
        let mut detector = PromptLineTurnDetector::new(TurnConfig::new(
            ">>> ",
            &["Send a message (/? for help)"],
            Duration::from_millis(500),
        ));

        detector.consume(b">>> Send a message (/? for help)");
        detector.consume(b"\r>>> \" probe\r\n");
        detector.consume(b"FINAL_WITH_FRAGMENT\r\n");
        // The idle placeholder, split one byte at a time.
        for byte in b">>> Send a message (/? for help)" {
            detector.consume(&[*byte]);
        }

        assert_eq!(detector.confirm(&pending_of(&detector)), 1);
    }

    /// A prompt preceded by a submitted-and-answered turn completes it;
    /// after the confirm, a bare prompt redraw opens no new pending.
    #[test]
    fn confirm_consumes_the_turn_and_redraw_is_silent() {
        let mut detector = PromptLineTurnDetector::new(TurnConfig::new(
            ">>> ",
            &["Send a message (/? for help)"],
            Duration::from_millis(500),
        ));

        detector.consume(b">>> ask\r\noutput\r\n");
        detector.consume(b">>> Send a message (/? for help)");
        assert!(detector.pending().is_some());
        assert_eq!(detector.confirm(&pending_of(&detector)), 1);

        detector.consume(b"\r\n");
        detector.consume(b">>> Send a message (/? for help)");
        assert!(detector.pending().is_none());
    }

    #[test]
    fn empty_prompt_is_rejected() {
        let result =
            std::panic::catch_unwind(|| TurnConfig::new("", &["x"], Duration::from_millis(500)));
        assert!(result.is_err());
    }

    #[test]
    fn empty_suffix_is_rejected() {
        let result =
            std::panic::catch_unwind(|| TurnConfig::new(">>> ", &[""], Duration::from_millis(500)));
        assert!(result.is_err());
    }

    #[test]
    fn zero_delay_is_rejected() {
        let result = std::panic::catch_unwind(|| TurnConfig::new(">>> ", &[], Duration::ZERO));
        assert!(result.is_err());
    }

    fn ollama_runtime() -> TurnRuntime {
        TurnRuntime::new(&builtin_definitions()[0])
    }

    /// A full Ollama-shaped turn announces one pending event; an unchanged
    /// pending does not announce twice.
    #[test]
    fn runtime_announces_pending_once() {
        let mut runtime = ollama_runtime();
        let start = Instant::now();

        assert!(runtime.consume(b">>> typed prompt\r\n", start).is_empty());
        assert!(runtime
            .consume(b"answer lines\r\n", start + Duration::from_millis(10))
            .is_empty());
        let events = runtime.consume(
            b">>> Send a message (/? for help)",
            start + Duration::from_millis(20),
        );
        assert!(
            matches!(events.last(), Some(TurnEvent::Pending(_))),
            "the idle placeholder must open a boundary: {events:?}"
        );

        let again = runtime.consume(b"", start + Duration::from_millis(30));
        assert!(again.is_empty(), "unchanged pending must not re-announce");
    }

    /// A boundary invalidated by later visible bytes emits the invalidation
    /// and clears its announcement.
    #[test]
    fn runtime_reports_invalidation() {
        let mut runtime = ollama_runtime();
        let start = Instant::now();

        runtime.consume(b">>> prompt\r\n", start);
        runtime.consume(b"output\r\n", start + Duration::from_millis(10));
        let events = runtime.consume(b">>> ", start + Duration::from_millis(20));
        let Some(TurnEvent::Pending(pending)) = events.first() else {
            panic!("expected a single pending event, got {events:?}");
        };
        assert_eq!(pending.agent_id, "ollama");
        assert_eq!(pending.agent_name, "Ollama");

        // A response line starting with prompt text invalidates before the
        // deadline.
        let events = runtime.consume(b">>> noisy response\r\n", start + Duration::from_millis(30));
        assert_eq!(events.len(), 1, "{events:?}");
        match &events[0] {
            TurnEvent::Invalidated { identifier } => {
                assert_eq!(*identifier, pending.identifier)
            }
            other => panic!("expected an invalidation event, got {other:?}"),
        }
    }

    /// A pending that passed its deadline confirms internally on the next
    /// chunk (upstream's tee ordering); the phase frees without an
    /// invalidation event so the app delivers exactly once at its own
    /// deadline, and later turns can open fresh boundaries.
    #[test]
    fn runtime_confirms_at_deadline_silently() {
        let mut runtime = ollama_runtime();
        let start = Instant::now();

        runtime.consume(b">>> typed submission\r\n", start);
        runtime.consume(b"final answer\r\n", start + Duration::from_millis(10));
        let events = runtime.consume(
            b">>> Send a message (/? for help)",
            start + Duration::from_millis(20),
        );
        let Some(TurnEvent::Pending(pending)) = events.last() else {
            panic!("expected a pending event, got {events:?}");
        };
        let first_id = pending.identifier;
        let deadline = start + Duration::from_millis(20) + pending.delay;

        // Bytes arriving after the deadline: the tee-equivalent confirms
        // the stale boundary first, then consumes — no invalidation event.
        let events = runtime.consume(b"\r\n", deadline + Duration::from_millis(5));
        assert!(
            events.is_empty(),
            "confirmed boundary stays silent: {events:?}"
        );

        // The detector is back in ready; a fresh turn can open a boundary.
        runtime.consume(b">>> next probe\r\n", deadline + Duration::from_millis(10));
        runtime.consume(b"next answer\r\n", deadline + Duration::from_millis(15));
        let events = runtime.consume(
            b">>> Send a message (/? for help)",
            deadline + Duration::from_millis(20),
        );
        let Some(TurnEvent::Pending(next)) = events.last() else {
            panic!("expected the re-created pending event, got {events:?}");
        };
        assert!(next.identifier > first_id);
    }

    /// A chunk that invalidates one candidate and re-creates another
    /// announces a strictly increasing pending sequence.
    #[test]
    fn runtime_supersedes_recreated_pending() {
        let mut runtime = ollama_runtime();
        let start = Instant::now();

        runtime.consume(b">>> summarize this\r\n", start);
        runtime.consume(b"working on it\r\n", start + Duration::from_millis(10));
        // One chunk: the prompt candidate is created, the CR/LF boundary
        // invalidates it, and the chunk ends on the exact idle placeholder
        // — the final candidate is the one that survives.
        let events = runtime.consume(
            b">>> \r\n>>> Send a message (/? for help)",
            start + Duration::from_millis(800),
        );
        assert!(matches!(events.last(), Some(TurnEvent::Pending(_))));
        let mut last_id = 0u64;
        for event in &events {
            match event {
                TurnEvent::Pending(pending) => {
                    assert!(pending.identifier > last_id);
                    last_id = pending.identifier;
                }
                TurnEvent::Invalidated { identifier } => {
                    assert_eq!(*identifier, last_id, "{events:?}");
                }
            }
        }
    }

    /// The built-in table carries exactly upstream's prompt-detection
    /// agents.
    #[test]
    fn builtin_definitions_match_upstream() {
        let builtins = builtin_definitions();
        assert_eq!(builtins.len(), 1);
        assert_eq!(builtins[0].id, "ollama");
        assert_eq!(builtins[0].display_name, "Ollama");
        assert_eq!(
            builtins[0].config.confirmation_delay,
            Duration::from_millis(500)
        );
    }

    /// The M3.4 live-smoke emitter's byte sequence, driven through a real
    /// `TurnRuntime` with the same chunk boundaries ConPTY delivers. Pins
    /// the full hand-over semantics the smoke asserts live: an idle prompt
    /// that paints and sits only arms the detector (a terminated waiting
    /// line can never count — handle_line_boundary invalidates the fresh
    /// pending on the very CR that closes it), the submission echo + its
    /// response are internal phase transitions, the re-painted idle prompt
    /// (unterminated, as a real UI draws it) announces exactly one pending
    /// turn, and a late chunk past the confirmation deadline is
    /// pre-confirmed silently (tee parity — no second announcement).
    #[test]
    fn smoke_handover_sequence_announces_exactly_once() {
        let mut runtime = TurnRuntime::new(&builtin_definitions()[0]);
        let t0 = Instant::now();
        let at = |offset: u64| t0 + Duration::from_millis(offset);

        // ConPTY's spawn probes (win32-input-mode + focus reporting): inert.
        let events = runtime.consume(b"\x1b[?9001h\x1b[?1004h", at(0));
        assert!(events.is_empty(), "{events:?}");

        // The idle prompt paints and sits (no line break, as Ollama draws
        // it): the detector only arms — no turn boundary exists yet.
        let events = runtime.consume(b">>> Send a message (/? for help)", at(50));
        assert!(events.is_empty(), "{events:?}");

        // The echoed submission starts a turn and its response is observed
        // (both lines terminated, one chunk, as a fast CLI echo produces).
        // Internal phase transitions only.
        let events = runtime.consume(
            b">>> Return FINAL_OLLAMA_OK\r\nFINAL_OLLAMA_OK\r\n",
            at(1_050),
        );
        assert!(events.is_empty(), "{events:?}");

        // ...and the re-painted idle prompt closes the turn boundary:
        // exactly one pending announcement. Unterminated — the boundary is
        // the line sitting unchanged, not a stray CR behind it.
        let events = runtime.consume(b">>> Send a message (/? for help)", at(2_100));
        assert_eq!(events.len(), 1, "{events:?}");
        let TurnEvent::Pending(pending) = &events[0] else {
            panic!("not a pending announcement: {events:?}");
        };
        assert_eq!(pending.delay, Duration::from_millis(500));
        let identifier = pending.identifier;
        assert_ne!(identifier, 0);

        // A chunk after the confirmation deadline: the tee rule confirms
        // the stale boundary before consuming it — no second
        // announcement, no invalidation.
        let events = runtime.consume(b"done\r\n", at(5_000));
        assert!(
            events.is_empty(),
            "late chunk after the deadline re-announced: {events:?}"
        );
    }
}
