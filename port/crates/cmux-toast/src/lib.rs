//! Windows desktop toasts for the port (M3.2b, ADR-0011).
//!
//! Windows shows a toast for an unpackaged desktop app only when the app's
//! **AppUserModelID** is registered by a Start Menu shortcut whose shell
//! item property store carries `System.AppUserModel.ID`. This crate owns
//! that contract end to end:
//!
//! 1. an idempotent per-user shortcut
//!    (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\cmux-shell.lnk`,
//!    icon + description + AUMID property);
//! 2. `ToastNotificationManager` posts (generic template, optional sound).
//!
//! The delivery runs on a dedicated worker thread so the UI thread only
//! pays a channel send; failures are logged, never fatal — the ring in the
//! sidebar is the notification of record, the toast is best effort.
//!
//! Upstream parity notes (macOS app): cmux uses `UNUserNotificationCenter`
//! with opt-in permissions; the Windows analogue has no per-app permission
//! prompt — the shortcut creates the Settings entry, and effects come from
//! the same M3.2a policy (`effects.desktop` gates the post, `effects.sound`
//! maps to the toast's `silent` attribute).

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

/// The AppUserModelID the shortcut carries (stable identity for Windows
/// notification grouping: `Company.Product`-style, short, no spaces).
pub const DEFAULT_AUMID: &str = "cmux.shell";
/// Shown as the toast attribution (from the shortcut's description).
pub const DISPLAY_NAME: &str = "cmux";

mod win;

pub mod xml;

/// Delivery-failure reasons; all carry an actionable message.
#[derive(Debug)]
pub enum ToastError {
    /// The environment lacks a usable per-user Start Menu location.
    NoProgramsDirectory(String),
    /// A shell/storage call failed.
    Windows(String),
}

impl std::fmt::Display for ToastError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToastError::NoProgramsDirectory(what) => {
                write!(f, "no Start Menu Programs directory ({what})")
            }
            ToastError::Windows(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for ToastError {}

/// The per-user shortcut path that registers [`DEFAULT_AUMID`]: the
/// authoritative `SHGetKnownFolderPath(Start Menu\Programs)`, plus the
/// leaf name cmux owns.
pub fn shortcut_path() -> Result<PathBuf, ToastError> {
    let mut path = win::known_programs_dir()?;
    std::fs::create_dir_all(&path)
        .map_err(|e| ToastError::NoProgramsDirectory(format!("cannot create {path:?}: {e}")))?;
    path.push("cmux-shell.lnk");
    Ok(path)
}

/// One queued toast.
struct ToastJob {
    title: String,
    body: String,
    silent: bool,
}

/// Best-effort toast delivery. `show` is fire-and-forget: it queues the
/// post onto the worker thread and returns immediately; the worker logs
/// its own failures (`cmux-toast: …` on stderr). The first queued job also
/// creates (or repairs) the AUMID shortcut, so no setup is required up
/// front and broken shortcuts heal on the next notification.
#[derive(Debug, Default)]
pub struct ToastDelivery {
    aumid: String,
    jobs: OnceLock<Sender<ToastJob>>,
}

impl ToastDelivery {
    /// Delivery under [`DEFAULT_AUMID`].
    pub fn new() -> Self {
        Self::with_aumid(DEFAULT_AUMID)
    }

    pub fn with_aumid(aumid: impl Into<String>) -> Self {
        Self {
            aumid: aumid.into(),
            jobs: OnceLock::new(),
        }
    }

    /// Queues one toast. `Ok(())` means queued; the outcome is logged by
    /// the worker (`cmux-toast: …`). Blocks only on the channel send.
    pub fn show(&self, title: &str, body: &str, silent: bool) -> Result<(), ToastError> {
        let sender = self.jobs.get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<ToastJob>();
            let aumid = self.aumid.clone();
            std::thread::Builder::new()
                .name("cmux-toast".to_owned())
                .spawn(move || worker(aumid, receiver))
                .expect("spawning the cmux-toast worker must succeed");
            sender
        });
        sender
            .send(ToastJob {
                title: title.to_owned(),
                body: body.to_owned(),
                silent,
            })
            .map_err(|_| ToastError::Windows("toast worker has shut down".to_owned()))
    }
}

/// The worker: one thread, one COM apartment (MTA), shortcut repair on
/// first use, then a show loop. All failures log with the `cmux-toast:`
/// prefix so the smoke contract can assert them.
fn worker(aumid: String, receiver: mpsc::Receiver<ToastJob>) {
    if let Err(error) = win::initialize_apartment() {
        eprintln!("cmux-toast: COM init failed: {error} — toasts are disabled");
        // Drain and drop: the channel must not look dead to the UI.
        for _ in receiver { /* discarded */ }
        return;
    }
    if let Err(error) = ensure_shortcut(&aumid) {
        eprintln!(
            "cmux-toast: shortcut registration failed: {error} — toasts disabled until it succeeds"
        );
        for _ in receiver { /* discarded */ }
        return;
    }
    eprintln!("cmux-toast: ready (AUMID {}) ", aumid);
    for job in receiver {
        let xml = xml::toast_xml(&job.title, &job.body, job.silent, DISPLAY_NAME);
        match win::post_toast(&aumid, &xml) {
            Ok(()) => eprintln!(
                "cmux-toast: posted (title '{}', silent={})",
                job.title, job.silent
            ),
            Err(error) => eprintln!("cmux-toast: post failed: {error}"),
        }
    }
}

/// Creates or repairs the AUMID shortcut (idempotent).
pub fn ensure_shortcut(aumid: &str) -> Result<PathBuf, ToastError> {
    let path = shortcut_path()?;
    let exe = std::env::current_exe()
        .map_err(|e| ToastError::Windows(format!("cannot locate cmux exe: {e}")))?;
    // The registered AUMID must match the one we post under, or Windows
    // silent-drops the toast; repair on any mismatch.
    let matches = win::shortcut_matches(&path, aumid).unwrap_or(false);
    if !matches {
        win::write_shortcut(&exe, &path, aumid, DISPLAY_NAME)
            .map_err(|e| ToastError::Windows(format!("could not register {path:?}: {e}")))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_lands_in_the_per_user_programs_menu() {
        let path = shortcut_path().expect("APPDATA exists on Windows CI/dev machines");
        let rendered = path.to_string_lossy().to_lowercase();
        assert!(rendered.ends_with(r"\cmux-shell.lnk"), "{rendered}");
        assert!(
            rendered.contains(r"microsoft\windows\start menu\programs"),
            "{rendered}"
        );
    }

    #[test]
    fn aumid_has_the_notification_identity_shape() {
        assert_eq!(DEFAULT_AUMID, "cmux.shell");
        assert!(!DEFAULT_AUMID.contains(' '));
    }

    #[test]
    fn delivery_is_default_constructible() {
        let delivery = ToastDelivery::new();
        assert_eq!(delivery.aumid, "cmux.shell");
    }
}
