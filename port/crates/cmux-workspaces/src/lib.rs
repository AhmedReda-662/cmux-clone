//! Session snapshot persistence for the port — the M2.4 equivalent of the
//! upstream `CmuxWorkspaces` session store, reusing Apple's snapshot shapes
//! where they install cleanly (verified against
//! `cmux-main/Sources/SessionPersistence.swift` and
//! `Packages/macOS/CmuxPanes/Sources/CmuxPanes/Values/`).
//!
//! What mirrors upstream exactly:
//! - versioned JSON payloads (`AppSessionSnapshot { version, createdAt,
//!   windows[] }`), the layout wire enum (`{"type":"pane","pane":…}` /
//!   `{"type":"split","split":{orientation, dividerPosition, first,
//!   second}}`), leaf tab membership (`panelIds` + `selectedPanelId`), and
//!   `PanelType` raw values (`"terminal"` …).
//! - the `SessionSnapshotRepository` semantics: sorted/deterministic
//!   encoding, atomic writes with identical-content skip, a primary +
//!   `-previous` backup pair, version-mismatch → unusable, and the
//!   empty-window snapshot being unusable on load.
//! - `SessionSnapshotRichness { workspaces, panels }` ordering: panels
//!   count first, workspaces break ties.
//!
//! Recorded divergences (ADR-0009): the DTO graph keeps only the fields the
//! port renders (ids, titles, directory, terminal type) — upstream's
//! agent/browser/markdown fields simply stop appearing; there is one
//! backup file instead of the rotated history directory; no 8 s autosave
//! yet (the port saves on exit); and storage lands in
//! `%APPDATA%\cmux\session.json` (Application Support equivalent) rather
//! than `~/.config/cmux/` so the two stores follow the same split upstream
//! uses (user config vs. app state).

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

/// Current snapshot schema version. Snapshots with any other version are
/// unusable (upstream reads the version through `SessionSnapshotSchema`).
pub const SCHEMA_VERSION: u32 = 1;

/// How much restorable layout a snapshot carries. Ordering mirrors
/// `SessionSnapshotRichness`: panels count first (a lost agent terminal
/// costs more than a lost empty workspace), workspaces break ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Richness {
    pub panels: u32,
    pub workspaces: u32,
}

/// One leaf pane of the split tree: the ordered tab list and selection.
/// Mirrors `SessionPaneLayoutSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PaneLayoutSnapshot {
    /// Panel identities in tab-strip order.
    #[serde(rename = "panelIds")]
    pub panel_ids: Vec<String>,
    /// Selected tab, or `None` for an empty pane.
    #[serde(rename = "selectedPanelId")]
    pub selected_panel_id: Option<String>,
    /// Whether the pane uses full-width tabs; `None` keeps older formats
    /// decoding (upstream adds this same optional field).
    #[serde(rename = "isFullWidthTabMode")]
    pub is_full_width_tab_mode: Option<bool>,
    /// M5.1: the URL a pane's WebView2 browser surface was parked on when
    /// saved. `None` (and absent on the wire) keeps the leaf a terminal.
    /// A missing WebView2 runtime at restore falls back to a terminal pane
    /// plus a log line (documented in ADR-0006).
    #[serde(rename = "browserUrl", skip_serializing_if = "Option::is_none")]
    pub browser_url: Option<String>,
}

/// One split node: axis, saved divider fraction, and the two children in
/// spatial order. Mirrors `SessionSplitLayoutSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitLayoutSnapshot {
    /// `"horizontal"` (left to right) or `"vertical"` (top to bottom),
    /// mirroring `SessionSplitOrientation` raw values.
    pub orientation: String,
    /// The saved fractional position of the divider.
    #[serde(rename = "dividerPosition")]
    pub divider_position: f64,
    pub first: Box<WorkspaceLayoutSnapshot>,
    pub second: Box<WorkspaceLayoutSnapshot>,
}

/// The recursive pane-tree node on the wire. Upstream hand-writes its
/// `Codable` as `{"type":"pane","pane":{…}}` / `{"type":"split","split":{…}}`
/// (distinct payload keys per variant), which serde's tagged enums cannot
/// derive, so the encoding is implemented here byte-compatibly.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceLayoutSnapshot {
    Pane(PaneLayoutSnapshot),
    Split(SplitLayoutSnapshot),
}

impl<'de> Deserialize<'de> for WorkspaceLayoutSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "camelCase")]
        enum Field {
            Type,
            Pane,
            Split,
            #[serde(other)]
            Unknown,
        }

        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = WorkspaceLayoutSnapshot;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(
                    "a layout node ({\"type\":\"pane\",...} or {\"type\":\"split\",...})",
                )
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut marker: Option<String> = None;
                // Payloads buffer as values so the type marker is always
                // processed first (mirroring upstream's decoder, which
                // checks `type` before reading either payload key).
                let mut pane: Option<serde_json::Value> = None;
                let mut split: Option<serde_json::Value> = None;
                while let Some(key) = map.next_key::<Field>()? {
                    match key {
                        Field::Type => {
                            marker = Some(map.next_value()?);
                        }
                        Field::Pane => pane = Some(map.next_value()?),
                        Field::Split => split = Some(map.next_value()?),
                        Field::Unknown => {
                            let _ignored = map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                match marker.as_deref() {
                    Some("pane") => {
                        let pane: PaneLayoutSnapshot = serde_json::from_value(
                            pane.ok_or_else(|| serde::de::Error::missing_field("pane"))?,
                        )
                        .map_err(serde::de::Error::custom)?;
                        Ok(WorkspaceLayoutSnapshot::Pane(pane))
                    }
                    Some("split") => {
                        let split: SplitLayoutSnapshot = serde_json::from_value(
                            split.ok_or_else(|| serde::de::Error::missing_field("split"))?,
                        )
                        .map_err(serde::de::Error::custom)?;
                        Ok(WorkspaceLayoutSnapshot::Split(split))
                    }
                    Some(other) => Err(serde::de::Error::custom(format!(
                        "Unsupported layout node type: {other}"
                    ))),
                    None => Err(serde::de::Error::missing_field("type")),
                }
            }
        }

        deserializer.deserialize_map(Visitor)
    }
}

impl Serialize for WorkspaceLayoutSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            WorkspaceLayoutSnapshot::Pane(pane) => {
                map.serialize_entry("type", "pane")?;
                map.serialize_entry("pane", pane)?;
            }
            WorkspaceLayoutSnapshot::Split(split) => {
                map.serialize_entry("type", "split")?;
                map.serialize_entry("split", split)?;
            }
        }
        map.end()
    }
}

/// One serialized panel: the port renders terminals only, so the subset
/// carries identity/metadata that survives round-trips. Other panel kinds
/// upstream (browser, markdown, agent sessions) are represented by their
/// `type` string without their content payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PanelSnapshot {
    pub id: String,
    /// `PanelType` raw value (`"terminal"`, `"browser"`, …).
    #[serde(rename = "type")]
    pub panel_type: String,
    pub title: Option<String>,
    #[serde(rename = "customTitle")]
    pub custom_title: Option<String>,
    pub directory: Option<String>,
}

/// One persisted workspace. Mirrors `SessionWorkspaceSnapshot`'s core;
/// fields the port does not model (notifications, todos, cloud bindings,
/// remote sessions) are omitted — decoding tolerates their absence because
/// they are optional on the wire upstream, and unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSnapshot {
    #[serde(rename = "workspaceId")]
    pub workspace_id: String,
    /// The auto/shell-derived name shown when the user has not renamed the
    /// workspace.
    #[serde(rename = "processTitle")]
    pub process_title: String,
    #[serde(rename = "customTitle")]
    pub custom_title: Option<String>,
    #[serde(rename = "customColor")]
    pub custom_color: Option<String>,
    #[serde(rename = "currentDirectory")]
    pub current_directory: String,
    #[serde(rename = "focusedPanelId")]
    pub focused_panel_id: Option<String>,
    pub layout: WorkspaceLayoutSnapshot,
    pub panels: Vec<PanelSnapshot>,
}

impl Default for WorkspaceSnapshot {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            process_title: String::new(),
            custom_title: None,
            custom_color: None,
            current_directory: String::new(),
            focused_panel_id: None,
            layout: WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot::default()),
            panels: Vec::new(),
        }
    }
}

/// The per-window tab store. Mirrors `SessionTabManagerSnapshot` (groups
/// are a future port feature).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TabManagerSnapshot {
    #[serde(rename = "selectedWorkspaceIndex")]
    pub selected_workspace_index: Option<u32>,
    pub workspaces: Vec<WorkspaceSnapshot>,
}

/// One persisted window. Mirrors `SessionWindowSnapshot`'s tab store; the
/// frame/display/sidebar fields are out of scope for the port slice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowSnapshot {
    #[serde(rename = "windowId")]
    pub window_id: Option<String>,
    #[serde(rename = "tabManager")]
    pub tab_manager: TabManagerSnapshot,
}

/// The snapshot root. Mirrors `AppSessionSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppSessionSnapshot {
    pub version: u32,
    /// Epoch seconds (upstream `TimeInterval`).
    #[serde(rename = "createdAt")]
    pub created_at: f64,
    pub windows: Vec<WindowSnapshot>,
}

impl AppSessionSnapshot {
    /// Builds a fresh root with the current schema version and timestamp.
    pub fn new_current(windows: Vec<WindowSnapshot>) -> Self {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        Self {
            version: SCHEMA_VERSION,
            created_at,
            windows,
        }
    }

    /// A persisted snapshot with an empty window list is anomalous (empty
    /// states remove the file instead of writing it) and loads as unusable —
    /// same as upstream.
    pub fn has_windows(&self) -> bool {
        !self.windows.is_empty()
    }

    /// Panels first, workspaces as the tie-break (upstream ordering).
    pub fn richness(&self) -> Richness {
        let mut panels = 0u32;
        for workspace in self.workspaces() {
            panels += workspace.panels.len() as u32;
        }
        Richness {
            panels,
            workspaces: self.workspaces().len() as u32,
        }
    }

    fn workspaces(&self) -> Vec<&WorkspaceSnapshot> {
        self.windows
            .iter()
            .flat_map(|window| window.tab_manager.workspaces.iter())
            .collect()
    }
}

pub fn user_session_paths() -> Result<SessionPaths, String> {
    match std::env::var_os("APPDATA") {
        Some(appdata) => {
            let dir = PathBuf::from(appdata).join("cmux");
            Ok(SessionPaths {
                primary: dir.join("session.json"),
                previous: dir.join("session-previous.json"),
            })
        }
        None => Err("no %APPDATA% directory resolved".to_owned()),
    }
}

/// Where the snapshot lives. `CMUX_SESSION_FILE` overrides the primary;
/// the backup keeps the same directory with a `-previous` name.
pub struct SessionPaths {
    pub primary: PathBuf,
    pub previous: PathBuf,
}

impl SessionPaths {
    /// Explicit override paths (tests and experiments).
    pub fn overriding(primary_local: &Path) -> Self {
        let file_name = primary_local
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session.json".to_owned());
        let previous = primary_local.with_file_name(format!("{file_name}-previous"));
        Self {
            primary: primary_local.to_owned(),
            previous,
        }
    }
}

/// Result of a load attempt (mirrors `SessionSnapshotLoadOutcome`).
#[derive(Debug, Clone, PartialEq)]
pub enum LoadOutcome {
    /// No file exists — as far as this session store knows the app never
    /// ran before.
    Missing,
    /// A file (primary or previous) could not be decoded into a usable
    /// snapshot: unreadable bytes, JSON errors, a different schema
    /// version, an empty window list.
    Unusable(String),
    Loaded(AppSessionSnapshot),
}

/// Reads the snapshot: the strongest store available wins, so an unusable
/// primary falls back to the `-previous` backup (upstream's
/// unusable-primary recovery path).
pub fn load(paths: &SessionPaths) -> LoadOutcome {
    match read_one(&paths.primary) {
        LoadOutcome::Loaded(snapshot) => LoadOutcome::Loaded(snapshot),
        other => match read_one(&paths.previous) {
            LoadOutcome::Loaded(snapshot) => LoadOutcome::Loaded(snapshot),
            LoadOutcome::Missing => other,
            LoadOutcome::Unusable(_) => other,
        },
    }
}

fn read_one(path: &Path) -> LoadOutcome {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadOutcome::Missing,
        Err(source) => {
            return LoadOutcome::Unusable(format!("{} is unreadable: {source}", path.display()))
        }
    };
    match decode_text(text) {
        Ok(snapshot) => LoadOutcome::Loaded(snapshot),
        Err(message) => LoadOutcome::Unusable(format!("{} is unusable: {message}", path.display())),
    }
}

fn decode_text(text: String) -> Result<AppSessionSnapshot, String> {
    let snapshot: AppSessionSnapshot =
        serde_json::from_str(&text).map_err(|error| format!("invalid snapshot: {error}"))?;
    if snapshot.version != SCHEMA_VERSION {
        return Err(format!(
            "schema version {} does not match this build ({SCHEMA_VERSION})",
            snapshot.version
        ));
    }
    if !snapshot.has_windows() {
        return Err("snapshot carries no windows".to_owned());
    }
    Ok(snapshot)
}

/// Persist errors carry the culprit path and cause, like `ConfigError`
/// does for `cmux.json`.
#[derive(Debug)]
pub enum SaveError {
    Write { path: PathBuf, source: io::Error },
    Encode { source: serde_json::Error },
    Rollback { path: PathBuf, source: io::Error },
}

impl fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Write { path, source } => {
                write!(
                    formatter,
                    "failed to write snapshot {}: {source}",
                    path.display()
                )
            }
            SaveError::Encode { source } => {
                write!(formatter, "failed to encode snapshot: {source}")
            }
            SaveError::Rollback { path, source } => {
                write!(
                    formatter,
                    "failed to roll back snapshot backup {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SaveError::Write { source, .. } | SaveError::Rollback { source, .. } => Some(source),
            SaveError::Encode { source } => Some(source),
        }
    }
}

/// Encodes and stores the snapshot: byte-equal content skips the write
/// entirely (upstream behavior), otherwise the current primary rotates
/// into `-previous` and the new content is published through a temporary
/// file (rename with a copy fallback on Windows).
pub fn save(snapshot: &AppSessionSnapshot, paths: &SessionPaths) -> Result<(), SaveError> {
    let body =
        serde_json::to_vec_pretty(snapshot).map_err(|source| SaveError::Encode { source })?;
    if !paths.primary.is_dir() {
        if let Some(parent) = paths.primary.parent() {
            let _ = fs::create_dir_all(parent);
        }
    }
    if let Ok(existing) = fs::read(&paths.primary) {
        if existing == body {
            return Ok(());
        }
    }
    // Rotate: primary → previous (Windows rename-over-existing can fail;
    // copy is the fallback, and an orphaned backup must not block saving).
    if paths.primary.is_file() {
        let rotated = match fs::rename(&paths.primary, &paths.previous) {
            Ok(()) => Ok(()),
            Err(_) => fs::copy(&paths.primary, &paths.previous).map(|_| ()),
        };
        if let Err(source) = rotated {
            return Err(SaveError::Rollback {
                path: paths.primary.clone(),
                source,
            });
        }
    }
    write_tmp_then_replace(&body, &paths.primary)
}

fn write_tmp_then_replace(body: &[u8], path: &Path) -> Result<(), SaveError> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, body).map_err(|source| SaveError::Write {
        path: path.to_owned(),
        source,
    })?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            let copied = fs::copy(&temporary, path).is_ok();
            let _ = fs::remove_file(&temporary);
            if copied {
                Ok(())
            } else {
                Err(SaveError::Write {
                    path: path.to_owned(),
                    source: io::Error::other("could not replace the snapshot file"),
                })
            }
        }
    }
}

// ———————————————————— restore-time repair ————————————————————

impl PaneLayoutSnapshot {
    /// A single-tab leaf (the port's current pane shape).
    pub fn single(panel_id: &str) -> Self {
        Self {
            panel_ids: vec![panel_id.to_owned()],
            selected_panel_id: Some(panel_id.to_owned()),
            is_full_width_tab_mode: None,
            browser_url: None,
        }
    }
}

impl WorkspaceLayoutSnapshot {
    /// The leaf payload, when this node is a leaf.
    pub fn as_leaf(&self) -> Option<&PaneLayoutSnapshot> {
        match self {
            WorkspaceLayoutSnapshot::Pane(pane) => Some(pane),
            WorkspaceLayoutSnapshot::Split(_) => None,
        }
    }

    /// The split geometry (orientation raw value, divider fraction, both
    /// children in spatial order), when this node is a split.
    pub fn as_split(&self) -> Option<(&str, f64, &Self, &Self)> {
        match self {
            WorkspaceLayoutSnapshot::Split(split) => Some((
                split.orientation.as_str(),
                split.divider_position,
                &split.first,
                &split.second,
            )),
            WorkspaceLayoutSnapshot::Pane(_) => None,
        }
    }

    /// Drops empty leaves (collapsing their splits), fixes selections, and
    /// deduplicates tab lists. Returns `None` when the whole branch is gone.
    fn repaired_tree(&self, issues: &mut Vec<String>) -> Option<Self> {
        match self {
            WorkspaceLayoutSnapshot::Pane(pane) => {
                let mut panel_ids: Vec<String> = Vec::new();
                for panel_id in &pane.panel_ids {
                    if !panel_ids.contains(panel_id) {
                        panel_ids.push(panel_id.clone());
                    }
                }
                if panel_ids.len() != pane.panel_ids.len() {
                    issues.push("pane tab list lost duplicate entries".to_owned());
                }
                if panel_ids.is_empty() {
                    issues.push("removed an empty pane leaf".to_owned());
                    return None;
                }
                let selected_panel_id = match &pane.selected_panel_id {
                    Some(selected) if panel_ids.contains(selected) => Some(selected.clone()),
                    other => {
                        if other.is_some() {
                            issues.push("pane selection referenced a missing tab; fell back to the first tab".to_owned());
                        }
                        Some(panel_ids[0].clone())
                    }
                };
                Some(WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                    panel_ids,
                    selected_panel_id,
                    is_full_width_tab_mode: pane.is_full_width_tab_mode,
                    // Browser identity survives repair (the URL was valid
                    // data already; the pane shape never changes).
                    browser_url: pane.browser_url.clone(),
                }))
            }
            WorkspaceLayoutSnapshot::Split(split) => {
                let first = split.first.repaired_tree(issues);
                let second = split.second.repaired_tree(issues);
                // Divider fractions outside the removable range (or absent
                // numbers) clamp; each branch collapse keeps its other side.
                let raw = split.divider_position;
                let divider_position = if raw.is_finite() {
                    raw.clamp(0.05, 0.95)
                } else {
                    0.5
                };
                if divider_position != raw {
                    issues.push("reset a divider position outside the supported range".to_owned());
                }
                match (first, second) {
                    (Some(first), Some(second)) => {
                        Some(WorkspaceLayoutSnapshot::Split(SplitLayoutSnapshot {
                            orientation: split.orientation.clone(),
                            divider_position,
                            first: Box::new(first),
                            second: Box::new(second),
                        }))
                    }
                    (Some(one), None) | (None, Some(one)) => {
                        issues.push("collapsed a split whose other pane leaf was empty".to_owned());
                        Some(one)
                    }
                    (None, None) => None,
                }
            }
        }
    }

    /// Every leaf-referenced panel id, deduplicated in encounter order.
    fn referenced_panels(&self, out: &mut Vec<String>) {
        match self {
            WorkspaceLayoutSnapshot::Pane(pane) => {
                for panel_id in &pane.panel_ids {
                    if !out.contains(panel_id) {
                        out.push(panel_id.clone());
                    }
                }
            }
            WorkspaceLayoutSnapshot::Split(split) => {
                split.first.referenced_panels(out);
                split.second.referenced_panels(out);
            }
        }
    }

    fn first_leaf_mut(&mut self) -> Option<&mut PaneLayoutSnapshot> {
        match self {
            WorkspaceLayoutSnapshot::Pane(pane) => Some(pane),
            WorkspaceLayoutSnapshot::Split(split) => split
                .first
                .first_leaf_mut()
                .or_else(|| split.second.first_leaf_mut()),
        }
    }

    /// Collects the leaf panel ids in spatial traversal order (the shell
    /// uses this to seed its id counter past restored ids).
    pub fn leaf_panel_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.referenced_panels(&mut out);
        out
    }
}

impl WorkspaceSnapshot {
    /// Applies the load-time repairs upstream's restore tolerances imply:
    /// empty leaves drop (their splits collapse), tab selections fall back
    /// to the first tab, panels referenced but absent from the panel list
    /// gain synthesized `"terminal"` entries, orphan panel entries re-attach
    /// to the first leaf, and a vanished tree rebuilds itself from the
    /// panel list. Returns the repaired workspace plus the issues that were
    /// fixed (the caller logs them).
    pub fn repaired(&self) -> Result<(Self, Vec<String>), String> {
        let mut issues: Vec<String> = Vec::new();

        // Panels: deduplicate entries by id (first wins).
        let mut panels: Vec<PanelSnapshot> = Vec::new();
        for panel in &self.panels {
            if panels.iter().any(|existing| existing.id == panel.id) {
                issues.push(format!("dropped a duplicate panel entry {}", panel.id));
            } else {
                panels.push(panel.clone());
            }
        }

        let tree = match self.layout.repaired_tree(&mut issues) {
            Some(tree) => tree,
            None => {
                if panels.is_empty() {
                    return Err("workspace has no restorable panes".to_owned());
                }
                issues.push("the layout tree vanished; rebuilding a single pane".to_owned());
                WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                    panel_ids: panels.iter().map(|panel| panel.id.clone()).collect(),
                    selected_panel_id: panels.first().map(|panel| panel.id.clone()),
                    is_full_width_tab_mode: None,
                    browser_url: None,
                })
            }
        };

        // References missing entries → synthesize terminal panels.
        let mut referenced: Vec<String> = Vec::new();
        tree.referenced_panels(&mut referenced);
        for panel_id in &referenced {
            if !panels.iter().any(|panel| &panel.id == panel_id) {
                issues.push(format!("synthesized a panel entry for {panel_id}"));
                panels.push(PanelSnapshot {
                    id: panel_id.clone(),
                    panel_type: "terminal".to_owned(),
                    title: None,
                    custom_title: None,
                    directory: None,
                });
            }
        }

        // Orphan entries (present but referenced by no leaf) attach to the
        // first leaf so a save never loses them.
        let mut orphan_ids: Vec<String> = Vec::new();
        for panel in &panels {
            if !referenced.contains(&panel.id) {
                orphan_ids.push(panel.id.clone());
            }
        }
        let mut tree = tree;
        if !orphan_ids.is_empty() {
            if let Some(first_leaf) = tree.first_leaf_mut() {
                issues.push(format!(
                    "re-attached orphan panel {} to the first pane",
                    orphan_ids.first().expect("non-empty by construction")
                ));
                first_leaf.panel_ids.extend(orphan_ids);
            }
        }

        // Focus: only repaired when it names an unknown panel.
        let mut repaired = Self {
            workspace_id: self.workspace_id.clone(),
            process_title: self.process_title.clone(),
            custom_title: self.custom_title.clone(),
            custom_color: self.custom_color.clone(),
            current_directory: self.current_directory.clone(),
            focused_panel_id: self.focused_panel_id.clone(),
            layout: tree,
            panels,
        };
        let known: Vec<String> = {
            let mut referenced: Vec<String> = Vec::new();
            repaired.layout.referenced_panels(&mut referenced);
            referenced
        };
        if let Some(focused) = &repaired.focused_panel_id {
            if !known.contains(focused) {
                if let Some(fallback) = known.first() {
                    issues.push(format!(
                        "focus {focused} named no pane; fell back to {fallback}"
                    ));
                    repaired.focused_panel_id = Some(fallback.clone());
                } else {
                    repaired.focused_panel_id = None;
                }
            }
        }

        Ok((repaired, issues))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_leaf(id: &str) -> WorkspaceLayoutSnapshot {
        WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot::single(id))
    }

    fn split(
        orientation: &str,
        divider: f64,
        first: WorkspaceLayoutSnapshot,
        second: WorkspaceLayoutSnapshot,
    ) -> WorkspaceLayoutSnapshot {
        WorkspaceLayoutSnapshot::Split(SplitLayoutSnapshot {
            orientation: orientation.to_owned(),
            divider_position: divider,
            first: Box::new(first),
            second: Box::new(second),
        })
    }

    fn workspace(layout: WorkspaceLayoutSnapshot, panel_ids: &[&str]) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            workspace_id: "w1".to_owned(),
            process_title: "cmux port — M2".to_owned(),
            custom_title: None,
            custom_color: None,
            current_directory: String::new(),
            focused_panel_id: Some("p1".to_owned()),
            layout,
            panels: panel_ids
                .iter()
                .map(|id| PanelSnapshot {
                    id: id.to_string(),
                    panel_type: "terminal".to_owned(),
                    title: None,
                    custom_title: None,
                    directory: None,
                })
                .collect(),
        }
    }

    #[test]
    fn layout_serializes_upstreams_wire_shape() {
        let leaf = pane_leaf("p1");
        assert_eq!(
            serde_json::to_string(&leaf).unwrap(),
            r#"{"type":"pane","pane":{"panelIds":["p1"],"selectedPanelId":"p1","isFullWidthTabMode":null}}"#
        );
        let split = split("horizontal", 0.5, pane_leaf("p1"), pane_leaf("p2"));
        assert_eq!(
            serde_json::to_string(&split).unwrap(),
            r#"{"type":"split","split":{"orientation":"horizontal","dividerPosition":0.5,"first":{"type":"pane","pane":{"panelIds":["p1"],"selectedPanelId":"p1","isFullWidthTabMode":null}},"second":{"type":"pane","pane":{"panelIds":["p2"],"selectedPanelId":"p2","isFullWidthTabMode":null}}}}"#
        );
    }

    fn snapshot(workspace: WorkspaceSnapshot) -> AppSessionSnapshot {
        AppSessionSnapshot::new_current(vec![WindowSnapshot {
            window_id: None,
            tab_manager: TabManagerSnapshot {
                selected_workspace_index: Some(0),
                workspaces: vec![workspace],
            },
        }])
    }

    #[test]
    fn layout_round_trips_through_decode() {
        let tree = split(
            "vertical",
            0.4,
            split("horizontal", 0.6, pane_leaf("p1"), pane_leaf("p2")),
            pane_leaf("p3"),
        );
        let snapshot = snapshot(workspace(tree.clone(), &["p1", "p2", "p3"]));
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: AppSessionSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
        assert_eq!(decoded.workspaces()[0].layout, tree);
    }

    #[test]
    fn decode_tolerates_upstream_fields_the_port_skips() {
        let text = r#"{ "version": 1, "createdAt": 123.0, "scrollbackCapturedAt": 9.9,
            "windows": [ { "windowId": null, "frame": null, "sidebar": null,
                "tabManager": { "selectedWorkspaceIndex": 0, "workspaceGroups": null,
                    "workspaces": [ { "workspaceId": "w1", "processTitle": "shell",
                        "customTitle": "agent", "notifications": [], "logEntries": [],
                        "statusEntries": [], "focusedPanelId": null, "gitBranch": null,
                        "layout": { "type": "pane", "pane": { "panelIds": ["p1"],
                            "selectedPanelId": null, "isFullWidthTabMode": true } },
                        "panels": [ { "id": "p1", "type": "terminal", "isPinned": false,
                            "listeningPorts": [3000], "title": null, "customTitle": null } ] } ] } } ] }"#;
        let decoded: AppSessionSnapshot = serde_json::from_str(text).unwrap();
        assert!(decoded.has_windows());
        assert_eq!(
            decoded.richness(),
            Richness {
                panels: 1,
                workspaces: 1
            }
        );
        assert_eq!(
            decoded.windows[0].tab_manager.workspaces[0]
                .custom_title
                .as_deref(),
            Some("agent")
        );
    }

    #[test]
    fn decode_rejects_unknown_layout_types() {
        let bad = r#"{ "version": 1, "createdAt": 1.0, "windows": [ { "windowId": null,
            "tabManager": { "selectedWorkspaceIndex": 0, "workspaces": [ { "workspaceId": "w",
            "processTitle": "x", "currentDirectory": "", "focusedPanelId": null,
            "layout": { "type": "cascade", "pane": null }, "panels": [] } ] } } ] }"#;
        let error = serde_json::from_str::<AppSessionSnapshot>(bad)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Unsupported layout node type"), "{error}");
    }

    #[test]
    fn load_rejects_wrong_version_and_empty_snapshots() {
        let good = snapshot(workspace(pane_leaf("p1"), &["p1"]));
        let mut wrong_version = good.clone();
        wrong_version.version = 99;

        let dir = std::env::temp_dir().join(format!("cmux-ws-load-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = SessionPaths::overriding(&dir.join("session.json"));
        let _ = fs::create_dir_all(&dir);

        // Missing file.
        assert_eq!(load(&paths), LoadOutcome::Missing);

        // Wrong schema version → unusable.
        fs::write(
            &paths.primary,
            serde_json::to_string(&wrong_version).unwrap(),
        )
        .unwrap();
        assert!(
            matches!(load(&paths), LoadOutcome::Unusable(reason) if reason.contains("schema version"))
        );

        // Empty windows → unusable (upstream: empty states remove the file).
        let empty_text =
            serde_json::to_string_pretty(&AppSessionSnapshot::new_current(Vec::new())).unwrap();
        fs::write(&paths.primary, empty_text).unwrap();
        assert!(
            matches!(load(&paths), LoadOutcome::Unusable(reason) if reason.contains("no windows"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_backup_when_primary_is_unusable() {
        let good = snapshot(workspace(pane_leaf("p1"), &["p1"]));
        let changed = snapshot(workspace(pane_leaf("p2"), &["p2"]));
        let dir = std::env::temp_dir().join(format!("cmux-ws-backup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = SessionPaths::overriding(&dir.join("session.json"));
        let _ = fs::create_dir_all(&dir);

        save(&good, &paths).unwrap();
        // A changed save rotates good into the backup; then clobbering the
        // primary must be served from `previous`.
        save(&changed, &paths).unwrap();
        fs::write(&paths.primary, "{ not a snapshot").unwrap();
        assert_eq!(load(&paths), LoadOutcome::Loaded(good));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_skips_identical_content_and_rotates_on_change() {
        let original = snapshot(workspace(
            split("vertical", 0.25, pane_leaf("p1"), pane_leaf("p2")),
            &["p1", "p2"],
        ));
        let dir = std::env::temp_dir().join(format!("cmux-ws-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = SessionPaths::overriding(&dir.join("session.json"));

        save(&original, &paths).unwrap();
        let first_body = fs::read(&paths.primary).unwrap();

        // Identical content skips everything — no backup rotation existed.
        save(&original, &paths).unwrap();
        assert!(!paths.previous.exists());

        // Changed layout: primary rotates into the backup, new bytes land.
        let changed = snapshot(workspace(
            split("horizontal", 0.75, pane_leaf("p1"), pane_leaf("p2")),
            &["p1", "p2"],
        ));
        save(&changed, &paths).unwrap();
        let backup_body = fs::read(&paths.previous).unwrap();
        assert_eq!(backup_body, first_body);
        assert_ne!(fs::read(&paths.primary).unwrap(), first_body);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_collapses_empty_split_leaves() {
        // The inner split's first leaf is empty, so that split collapses
        // into its surviving second child (p3) and the outer shape keeps
        // its geometry.
        let broken_root = split(
            "horizontal",
            0.5,
            pane_leaf("p1"),
            WorkspaceLayoutSnapshot::Split(SplitLayoutSnapshot {
                orientation: "vertical".to_owned(),
                divider_position: 0.7,
                first: Box::new(WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                    panel_ids: Vec::new(),
                    selected_panel_id: None,
                    is_full_width_tab_mode: None,
                    browser_url: None,
                })),
                second: Box::new(pane_leaf("p3")),
            }),
        );
        let expected = split("horizontal", 0.5, pane_leaf("p1"), pane_leaf("p3"));
        let (repaired, issues) = workspace(broken_root, &["p1", "p3"]).repaired().unwrap();
        assert_eq!(repaired.layout, expected);
        assert!(issues.iter().any(|issue| issue.contains("empty pane leaf")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("collapsed a split")));
    }

    #[test]
    fn repair_synthesizes_missing_panel_entries_and_attaches_orphans() {
        // p4 is referenced but has no panel entry; p9 is listed but never
        // referenced, so it re-attaches to the first leaf — which changes
        // what the repaired tree looks like.
        let tree = split("horizontal", 0.5, pane_leaf("p1"), pane_leaf("p4"));
        let workspace = WorkspaceSnapshot {
            panels: vec![
                PanelSnapshot {
                    id: "p1".to_owned(),
                    panel_type: "terminal".to_owned(),
                    title: None,
                    custom_title: None,
                    directory: None,
                },
                PanelSnapshot {
                    id: "p9".to_owned(),
                    panel_type: "terminal".to_owned(),
                    title: None,
                    custom_title: None,
                    directory: None,
                },
            ],
            ..workspace(tree.clone(), &[])
        };
        let (repaired, issues) = workspace.repaired().unwrap();
        let expected = split(
            "horizontal",
            0.5,
            WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                panel_ids: vec!["p1".to_owned(), "p9".to_owned()],
                selected_panel_id: Some("p1".to_owned()),
                is_full_width_tab_mode: None,
                browser_url: None,
            }),
            pane_leaf("p4"),
        );
        assert_eq!(repaired.layout, expected);
        assert!(issues
            .iter()
            .any(|issue| issue.contains("synthesized a panel entry for p4")));
        assert!(issues.iter().any(|issue| issue.contains("orphan panel p9")));
        let (_, _, first, _) = repaired.layout.as_split().unwrap();
        let first_leaf = first.as_leaf().unwrap();
        assert_eq!(first_leaf.panel_ids, vec!["p1".to_owned(), "p9".to_owned()]);
        assert!(repaired.panels.iter().any(|panel| panel.id == "p4"));
    }

    #[test]
    fn repair_rebuilds_a_vanished_tree_and_falls_back_focus() {
        let vanished = WorkspaceSnapshot {
            ..workspace(
                WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                    panel_ids: Vec::new(),
                    selected_panel_id: None,
                    is_full_width_tab_mode: None,
                    browser_url: None,
                }),
                &["p1", "p2"],
            )
        };
        let (repaired, issues) = vanished.repaired().unwrap();
        let leaf = repaired.layout.as_leaf().unwrap();
        assert_eq!(leaf.panel_ids.len(), 2);
        assert!(issues.iter().any(|issue| issue.contains("vanished")));

        // Focused panel unknown → first pane.
        let focused_wrong = WorkspaceSnapshot {
            focused_panel_id: Some("p99".to_owned()),
            ..repair_focus_fixture()
        };
        let (repaired, issues) = focused_wrong.repaired().unwrap();
        assert_eq!(repaired.focused_panel_id.as_deref(), Some("p1"));
        assert!(issues.iter().any(|issue| issue.contains("focus p99")));
    }

    #[test]
    fn repair_clamps_out_of_range_dividers() {
        let (repaired, issues) = workspace(
            split("horizontal", 3.0, pane_leaf("p1"), pane_leaf("p2")),
            &["p1", "p2"],
        )
        .repaired()
        .unwrap();
        let (_, divider, _, _) = repaired.layout.as_split().unwrap();
        assert_eq!(divider, 0.95);
        assert!(issues
            .iter()
            .any(|issue| issue.contains("divider position")));

        let (repaired, _) = workspace(
            split("horizontal", f64::NAN, pane_leaf("p1"), pane_leaf("p2")),
            &["p1", "p2"],
        )
        .repaired()
        .unwrap();
        let (_, divider, _, _) = repaired.layout.as_split().unwrap();
        assert_eq!(divider, 0.5);
    }

    fn repair_focus_fixture() -> WorkspaceSnapshot {
        workspace(pane_leaf("p1"), &["p1"])
    }
}
