//! Agent activity cards (design lane 5, feature row the design pane 1:257
//! carries): the data model behind the agent activity stream pane.
//!
//! The stream renders REAL events only: hook notifications that arrived
//! over OSC, events delivered through the hook pipe, and prompt-turn
//! deliveries. The painter never invents state: every card's title, body,
//! and elapsed time come from `AgentCard`/live lane facts. OS wiring
//! (ingest sites, the pane body) lives in the executable; this module
//! holds the pure model so retention and the headline rule are
//! unit-testable.

use std::collections::VecDeque;
use std::time::Instant;

/// The real event kinds the stream renders as completed cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardData {
    /// A terminal-initiated hook notification (OSC 9/777). `title` may be
    /// empty at wire level (OSC 9 carries no title); the headline rule
    /// then lifts the body's first line (see [`AgentCard::headline`]).
    Notification { title: String, body: String },
    /// A prompt-turn delivery. Upstream's handler posts the agent's
    /// display name with the "Task completed" body and the turn-complete
    /// category; the card holds both verbatim.
    TurnCompleted { agent_name: String, body: String },
}

/// One card in the stream: the event plus when it arrived. `at` feeds the
/// design card's right-aligned elapsed time (real, per event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCard {
    pub at: Instant,
    pub data: CardData,
}

/// Retention cap per workspace: bounded memory, and the stream reads as
/// "recent activity" not an unbounded log.
pub const CARDS_MAX_PER_WORKSPACE: usize = 24;

impl AgentCard {
    /// The card's headline: the notification title when present, else the
    /// body's first line (OSC 9); turn cards headline the agent name.
    /// Empty when the event carries no text at all.
    pub fn headline(&self) -> String {
        match &self.data {
            CardData::Notification { title, body } => {
                let trimmed = title.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_owned();
                }
                headline_from_body(body)
            }
            CardData::TurnCompleted { agent_name, .. } => {
                let trimmed = agent_name.trim();
                if trimmed.is_empty() {
                    String::new()
                } else {
                    trimmed.to_owned()
                }
            }
        }
    }

    /// The card's detail body: the notification body verbatim; turn
    /// cards keep their fact text. Notification bodies whose first line
    /// was lifted into the headline drop that line here.
    pub fn detail(&self) -> String {
        match &self.data {
            CardData::Notification { title, body } => {
                if title.trim().is_empty() {
                    body_after_headline(body)
                } else {
                    body.trim().to_owned()
                }
            }
            CardData::TurnCompleted { body, .. } => body.trim().to_owned(),
        }
    }

    /// Whether the card came from a turn delivery (the stream adds the
    /// agent facet to its footer rather than repeating the headline).
    pub fn is_turn(&self) -> bool {
        matches!(self.data, CardData::TurnCompleted { .. })
    }
}

/// First line of a body as the headline (OSC 9 has no title). Trims
/// whitespace; an empty body yields an empty headline.
fn headline_from_body(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// The body minus the line that became the headline: the first non-empty
/// line onward. A single-line body yields an empty detail.
fn body_after_headline(body: &str) -> String {
    let mut lines = body.lines().map(str::trim).peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        let rest: Vec<&str> = lines.skip_while(|later| later.is_empty()).collect();
        let joined = rest.join("\n");
        if joined.is_empty() {
            return String::new();
        }
        return joined;
    }
    String::new()
}

/// Per-workspace bounded retention, indexed like the port's other
/// per-workspace slots (`workspace_alerts`). Workspace close/order
/// shifts re-lengthen or truncate rather than remap - the SAME recorded
/// semantics the services cadence carries (A/FS lane 4 notes).
#[derive(Debug, Default)]
pub struct CardStore {
    slots: Vec<VecDeque<AgentCard>>,
}

impl CardStore {
    /// Lengthens (fresh empty slots) or truncates (tail removal) to the
    /// current workspace count. Cheap at every push; painting reads
    /// through the same check.
    pub fn sync_len(&mut self, workspace_count: usize) {
        if self.slots.len() != workspace_count {
            self.slots.resize_with(workspace_count, VecDeque::new);
        }
    }

    /// Appends a card into a workspace slot, evicting past the cap.
    /// No-op when the slot is not live (stale index after a close).
    pub fn push(&mut self, workspace_index: usize, card: AgentCard) {
        if workspace_index >= self.slots.len() {
            return;
        }
        let slot = &mut self.slots[workspace_index];
        slot.push_back(card);
        while slot.len() > CARDS_MAX_PER_WORKSPACE {
            slot.pop_front();
        }
    }

    /// The workspace slot's cards, oldest first. Empty when absent.
    pub fn cards(&self, workspace_index: usize) -> impl Iterator<Item = &AgentCard> {
        self.slots.get(workspace_index).into_iter().flatten()
    }

    /// Forgets a workspace's cards (its close path keeps retention
    /// bounded and prevents stale streams on a reused index).
    pub fn clear(&mut self, workspace_index: usize) {
        if let Some(slot) = self.slots.get_mut(workspace_index) {
            slot.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(data: CardData) -> AgentCard {
        AgentCard {
            at: Instant::now(),
            data,
        }
    }

    #[test]
    fn headline_prefers_notification_title() {
        let notification = card(CardData::Notification {
            title: "Build finished".to_owned(),
            body: "cargo build completed in 3.1s".to_owned(),
        });
        assert_eq!(notification.headline(), "Build finished");
        assert_eq!(notification.detail(), "cargo build completed in 3.1s");
        assert!(!notification.is_turn());
    }

    #[test]
    fn headline_without_title_lifts_first_body_line() {
        let notification = card(CardData::Notification {
            title: String::new(),
            body: "  Parsed 14 files across src/server/**.\nSecond line here.  ".to_owned(),
        });
        assert_eq!(
            notification.headline(),
            "Parsed 14 files across src/server/**."
        );
        assert_eq!(notification.detail(), "Second line here.");
    }

    #[test]
    fn single_line_body_becomes_headline_only() {
        let notification = card(CardData::Notification {
            title: String::new(),
            body: "just one line".to_owned(),
        });
        assert_eq!(notification.headline(), "just one line");
        assert_eq!(notification.detail(), "");
    }

    #[test]
    fn empty_notification_is_blank_rather_than_invented() {
        let notification = card(CardData::Notification {
            title: String::new(),
            body: "   \n  \n".to_owned(),
        });
        assert_eq!(notification.headline(), "");
        assert_eq!(notification.detail(), "");
    }

    #[test]
    fn turn_card_headline_is_agent_name_and_flags_is_turn() {
        let turn = card(CardData::TurnCompleted {
            agent_name: "Ollama".to_owned(),
            body: "Task completed".to_owned(),
        });
        assert_eq!(turn.headline(), "Ollama");
        assert_eq!(turn.detail(), "Task completed");
        assert!(turn.is_turn());
    }

    #[test]
    fn push_evicts_oldest_past_the_cap() {
        let mut store = CardStore::default();
        store.sync_len(1);
        for n in 0..CARDS_MAX_PER_WORKSPACE + 3 {
            store.push(
                0,
                card(CardData::TurnCompleted {
                    agent_name: format!("agent-{n}"),
                    body: String::new(),
                }),
            );
        }
        let seen: Vec<String> = store.cards(0).map(|entry| entry.headline()).collect();
        assert_eq!(seen.len(), CARDS_MAX_PER_WORKSPACE);
        assert_eq!(seen.first().expect("oldest retained"), "agent-3");
        assert_eq!(
            seen.last().expect("newest retained"),
            &format!("agent-{}", CARDS_MAX_PER_WORKSPACE + 2)
        );
    }

    #[test]
    fn sync_len_grows_and_shrinks_like_workspace_slots() {
        let mut store = CardStore::default();
        store.sync_len(2);
        store.push(
            1,
            card(CardData::Notification {
                title: "t".to_owned(),
                body: String::new(),
            }),
        );
        store.sync_len(3);
        assert_eq!(store.cards(2).count(), 0);
        store.sync_len(1);
        // Truncation dropped the tail slot along with its card.
        assert_eq!(store.cards(1).count(), 0);
    }

    #[test]
    fn push_rejects_stale_workspace_index() {
        let mut store = CardStore::default();
        store.sync_len(1);
        store.push(
            5,
            card(CardData::Notification {
                title: "t".to_owned(),
                body: String::new(),
            }),
        );
        assert_eq!(store.cards(5).count(), 0);
    }

    #[test]
    fn clear_empty_a_slot_only() {
        let mut store = CardStore::default();
        store.sync_len(2);
        store.push(
            0,
            card(CardData::Notification {
                title: "t".to_owned(),
                body: String::new(),
            }),
        );
        store.clear(0);
        assert_eq!(store.cards(0).count(), 0);
    }
}
