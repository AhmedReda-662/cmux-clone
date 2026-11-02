//! Shell-side model library: the pure pane tree (`panes::PaneTree`) that
//! turns `cmux-layout` plans into live split-pane state. The `cmux-shell`
//! binary owns terminals, ConPTY wiring, and rendering (see its module docs
//! for the smoke-mode contract).

pub mod agent_cards;
#[cfg(test)]
pub mod emulator_replies;
pub mod fonts;
pub mod git_state;
pub mod notifications;
pub mod panes;
pub mod policy;
pub mod port_state;
pub mod shell_session;
pub mod sidebar;
pub mod turns;
