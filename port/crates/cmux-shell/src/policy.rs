//! Desktop-notification delivery policy (M3.2a, ADR-0011) — a faithful
//! port of upstream's resolver
//! (`cmux-main/Sources/TerminalNotificationDeliveryDecision.swift`) and
//! the effects model
//! (`cmux-main/Packages/macOS/CmuxNotifications/Sources/CmuxNotifications/
//! NotificationPolicyEffects.swift`).
//!
//! Upstream rules, verified in that source:
//! 1. A muted workspace drops every effect (the all-suppressed gate).
//! 2. Otherwise, when the app is NOT focused, the workspace NOT active,
//!    or the surface NOT focused, the arrival is external delivery with
//!    the requested effects — with one detour: while cmux is the active
//!    app and `suppress_when_app_focused` is on, the desktop banner is
//!    skipped (the disposition becomes focused inline; sound, pane flash
//!    and command are kept).
//! 3. When the notifying surface is exactly what the user is looking at,
//!    the arrival is focused inline: no banner, no pane flash, the
//!    workspace stays in place, and the sound is silenced unless
//!    `sound_when_focused` opts in (the focused pane shows its ring and
//!    flash only). History (`record`), unread state (`mark_unread`) and
//!    the custom command survive every non-muted path.

/// The delivery effects a terminal notification triggers. Every effect
/// defaults on; callers turn individual effects off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEffects {
    /// Keep the notification in history and the notifications panel.
    pub record: bool,
    /// Mark the workspace and surface unread.
    pub mark_unread: bool,
    /// Move the workspace up in the sidebar.
    pub reorder_workspace: bool,
    /// Post a native desktop banner (a Windows toast in the port).
    pub desktop: bool,
    /// Play the notification sound.
    pub sound: bool,
    /// Run the user's `notifications.command`.
    pub command: bool,
    /// Flash the pane ring.
    pub pane_flash: bool,
}

impl Default for PolicyEffects {
    fn default() -> Self {
        Self::all_on()
    }
}

impl PolicyEffects {
    /// The policy default: every effect on.
    pub fn all_on() -> Self {
        Self {
            record: true,
            mark_unread: true,
            reorder_workspace: true,
            desktop: true,
            sound: true,
            command: true,
            pane_flash: true,
        }
    }

    /// Every delivery effect disabled. Workspace mute is an admission
    /// gate; an exhaustive constructor keeps a newly added effect from
    /// leaking through a muted workspace (upstream's same rule).
    pub fn all_suppressed() -> Self {
        Self {
            record: false,
            mark_unread: false,
            reorder_workspace: false,
            desktop: false,
            sound: false,
            command: false,
            pane_flash: false,
        }
    }
}

/// Where a notification goes after the shared focus and mute gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivalDisposition {
    /// The workspace is muted: nothing below history/the panel observes
    /// the arrival.
    Muted,
    /// Everything the effects ask for, banners included.
    ExternalDelivery,
    /// The surface is visible; in-app feedback only (no banner).
    FocusedInline,
}

impl ArrivalDisposition {
    /// Stable short name for logs and the smoke contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            ArrivalDisposition::Muted => "muted",
            ArrivalDisposition::ExternalDelivery => "external",
            ArrivalDisposition::FocusedInline => "focused-inline",
        }
    }
}

/// The focus configuration one arrival is decided against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryContext {
    /// The workspace's mute state (no mute UI in the port yet, so callers
    /// pass `false` until that lands with the toast slice).
    pub is_muted: bool,
    /// cmux has keyboard focus (the egui viewport focus).
    pub is_app_focused: bool,
    /// The event's workspace is the sidebar's active one.
    pub is_active_workspace: bool,
    /// The event's pane is that workspace's focused pane.
    pub is_focused_surface: bool,
    /// `notifications.soundWhenFocused` (upstream default `false`).
    pub sound_when_focused: bool,
    /// `notifications.suppressWhenAppFocused` (upstream default `false`).
    pub suppress_when_app_focused: bool,
}

impl Default for DeliveryContext {
    fn default() -> Self {
        Self {
            is_muted: false,
            is_app_focused: true,
            is_active_workspace: true,
            is_focused_surface: true,
            sound_when_focused: false,
            suppress_when_app_focused: false,
        }
    }
}

/// One resolved notification arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryDecision {
    pub disposition: ArrivalDisposition,
    pub effects: PolicyEffects,
}

/// Upstream's gate order: mute → (app ∧ active ∧ focused) → external,
/// with the `suppressWhenAppFocused` detour before the external path.
/// `base` is the requested effect set (the port passes
/// [`PolicyEffects::all_on`]; hook per-call overrides arrive with the
/// hook subsystem).
pub fn resolve(context: &DeliveryContext, base: PolicyEffects) -> DeliveryDecision {
    if context.is_muted {
        return DeliveryDecision {
            disposition: ArrivalDisposition::Muted,
            effects: PolicyEffects::all_suppressed(),
        };
    }

    let focused_surface =
        context.is_app_focused && context.is_active_workspace && context.is_focused_surface;
    if !focused_surface {
        if context.is_app_focused && context.suppress_when_app_focused {
            // cmux is the active app: skip the banner, keep the sound,
            // pane flash, and custom command for this arrival.
            let mut effects = base;
            effects.desktop = false;
            return DeliveryDecision {
                disposition: ArrivalDisposition::FocusedInline,
                effects,
            };
        }
        return DeliveryDecision {
            disposition: ArrivalDisposition::ExternalDelivery,
            effects: base,
        };
    }

    // The active surface is already visible. Preserve history/unread and
    // the custom automation hook while suppressing external feedback; the
    // workspace the user is typing in must not move.
    let mut effects = base;
    effects.reorder_workspace = false;
    effects.desktop = false;
    effects.pane_flash = false;
    effects.sound = effects.sound && context.sound_when_focused;
    DeliveryDecision {
        disposition: ArrivalDisposition::FocusedInline,
        effects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The all-on base every case starts from (the port's request).
    fn base() -> PolicyEffects {
        PolicyEffects::all_on()
    }

    #[test]
    fn muted_drops_every_effect() {
        let context = DeliveryContext {
            is_muted: true,
            ..DeliveryContext::default()
        };
        let decision = resolve(&context, base());
        assert_eq!(decision.disposition, ArrivalDisposition::Muted);
        assert_eq!(decision.effects, PolicyEffects::all_suppressed());
    }

    #[test]
    fn unfocused_app_is_external_with_all_effects() {
        let context = DeliveryContext {
            is_app_focused: false,
            ..DeliveryContext::default()
        };
        let decision = resolve(&context, base());
        assert_eq!(decision.disposition, ArrivalDisposition::ExternalDelivery);
        assert_eq!(decision.effects, base());
    }

    #[test]
    fn background_workspace_while_app_focused_is_external() {
        // Another workspace is active; upstream still banners.
        let context = DeliveryContext {
            is_active_workspace: false,
            ..DeliveryContext::default()
        };
        let decision = resolve(&context, base());
        assert_eq!(decision.disposition, ArrivalDisposition::ExternalDelivery);
        assert_eq!(decision.effects, base());
    }

    #[test]
    fn other_pane_in_the_active_workspace_is_external() {
        let context = DeliveryContext {
            is_focused_surface: false,
            ..DeliveryContext::default()
        };
        let decision = resolve(&context, base());
        assert_eq!(decision.disposition, ArrivalDisposition::ExternalDelivery);
        assert_eq!(decision.effects, base());
    }

    #[test]
    fn focused_surface_is_inline_without_banner_or_flash() {
        let decision = resolve(&DeliveryContext::default(), base());
        assert_eq!(decision.disposition, ArrivalDisposition::FocusedInline);
        let effects = decision.effects;
        // History, unread and the command survive; external feedback goes.
        assert!(effects.record);
        assert!(effects.mark_unread);
        assert!(effects.command);
        assert!(!effects.desktop);
        assert!(!effects.pane_flash);
        assert!(!effects.reorder_workspace);
        // The focused pane stays quiet unless opted in (default off).
        assert!(!effects.sound);
    }

    #[test]
    fn sound_when_focused_opts_the_focused_pane_back_in() {
        let context = DeliveryContext {
            sound_when_focused: true,
            ..DeliveryContext::default()
        };
        let decision = resolve(&context, base());
        assert!(decision.effects.sound);
        assert!(!decision.effects.desktop);
    }

    #[test]
    fn suppress_when_app_focused_takes_only_the_banner() {
        let context = DeliveryContext {
            // App focused but this is a background workspace: not the
            // focused-surface path — the suppress setting detours it.
            is_active_workspace: false,
            suppress_when_app_focused: true,
            ..DeliveryContext::default()
        };
        let decision = resolve(&context, base());
        assert_eq!(decision.disposition, ArrivalDisposition::FocusedInline);
        assert!(!decision.effects.desktop);
        // Sound, pane flash and command are kept (upstream's comment).
        assert!(decision.effects.sound);
        assert!(decision.effects.pane_flash);
        assert!(decision.effects.command);
        assert!(decision.effects.mark_unread);
    }

    #[test]
    fn base_effects_pass_through_the_external_path() {
        // A caller that turns an effect off gets exactly that request
        // back on the external path (hook overrides arrive later).
        let mut requested = base();
        requested.sound = false;
        requested.pane_flash = false;
        let context = DeliveryContext {
            is_app_focused: false,
            ..DeliveryContext::default()
        };
        let decision = resolve(&context, requested);
        assert!(!decision.effects.sound);
        assert!(!decision.effects.pane_flash);
        assert!(decision.effects.desktop);
    }
}
