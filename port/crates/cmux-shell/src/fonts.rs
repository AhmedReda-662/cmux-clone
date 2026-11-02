//! Windows system fonts for the shell (M4.3, ADR-0012).
//!
//! egui's built-in fonts (epaint's `default_fonts`: Hack, Ubuntu-Light,
//! NotoEmoji, emoji-icon-font) cover the Latin world and emojis, but carry
//! **no CJK glyphs**, so terminal text produced through an IME or chosen by
//! the shell (CJK filenames, localized prompt output) renders as tofu. The
//! terminal IME path exists in the widget (`egui_tty`'s text-without-key
//! handling), so the missing piece is display coverage — on Windows that
//! means loading system faces out of `%WINDIR%\Fonts`:
//!
//! - The **terminal primary**: a well-hinted monospace face. Cascadia when
//!   the machine has it (Windows Terminal's font), Consolas otherwise —
//!   both are guaranteed to keep a uniform advance for the grid.
//! - The **CJK fallbacks** in script-coverage order: Microsoft YaHei
//!   (simplified Chinese), Microsoft JhengHei (traditional Chinese),
//!   MS Gothic (Japanese), Malgun Gothic (Korean). They join the
//!   *Monospace* family (terminal grid) and the *Proportional* family
//!   (sidebar and UI chrome) after the Latin faces, before the emoji
//!   fonts, so a CJK glyph resolves to a text face rather than an icon.
//!
//! Everything here degrades to egui's defaults: a machine without any of
//! the faces keeps today's rendering unchanged and logs why. The face
//! *selection* (Windows-cascading identity fonts instead of upstream's
//! settings-driven font stack) is a recorded port divergence until the
//! settings milestone adds custom fonts.
//!
//! The planner and the family composition are pure: unit tests drive them
//! with a fake filesystem and fake font bytes. Touching the real
//! filesystem happens only in [`install`].

use std::path::{Path, PathBuf};

/// Terminal primary candidates, most preferred first. The label doubles as
/// the egui font-registry name and the startup-log name.
const MONO_PRIMARY: &[(&str, &str)] = &[
    ("CascadiaCode.ttf", "Cascadia Code"),
    ("CascadiaMono.ttf", "Cascadia Mono"),
    ("consola.ttf", "Consolas"),
];

/// CJK fallback candidates in coverage order (simplified Chinese,
/// traditional Chinese, Japanese, Korean). `msgothic.ttc` and the YaHei/
/// JhengHei files are TrueType collections; index 0 is the plain face.
const CJK_FALLBACK: &[(&str, &str)] = &[
    ("msyh.ttc", "Microsoft YaHei"),
    ("msjh.ttc", "Microsoft JhengHei"),
    ("msgothic.ttc", "MS Gothic"),
    ("malgun.ttf", "Malgun Gothic"),
];

/// Bold-face candidates for the design's strong chip inks (lane 3's diff
/// chip shows `+12 -3` in Liberation Serif Bold; the Windows
/// identity-companions carry that weight). Registered as a **separate**
/// `Bold` family — the Proportional family's order must never prefer a
/// bold face, or every chrome line turns heavy.
const BOLD_PRIMARY: &[(&str, &str)] = &[
    ("segoeuib.ttf", "Segoe UI Bold"),
    ("arialbd.ttf", "Arial Bold"),
];

/// The egui family name the bold faces install under (see [`BOLD_PRIMARY`]).
pub const BOLD_FAMILY: &str = "Bold";

/// The family id for the design's strong text (the chip numbers). Falls
/// back to the Proportional faces when the machine has no identity bold
/// (the painter keeps its size and ink either way).
pub fn bold_family_id() -> egui::FontFamily {
    egui::FontFamily::Name(BOLD_FAMILY.into())
}

/// The Windows fonts directory: `%WINDIR%\Fonts`, with the classic
/// location standing in when the variable is absent (tests always pass a
/// directory of their own).
pub fn system_fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    windir.join("Fonts")
}

/// One font file the plan wants loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FontSource {
    /// File name inside the Windows fonts directory.
    file: &'static str,
    /// Face name — the egui font-registry key and the log name.
    label: &'static str,
    /// Index into a TrueType collection (.ttc); 0 for single-face files.
    index: u32,
}

impl FontSource {
    fn new(file: &'static str, label: &'static str) -> Self {
        Self {
            file,
            label,
            index: 0,
        }
    }
}

/// What the machine's fonts directory supports: one terminal primary (or
/// none — egui's Hack stays), every CJK fallback found in the coverage
/// order above, and one identity bold face for the design's strong inks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct FontPlan {
    mono: Option<FontSource>,
    cjk: Vec<FontSource>,
    bold: Option<FontSource>,
}

/// Decides which fonts to load given a directory that may contain the
/// candidates. Pure: `existing` stands in for the filesystem lookup so
/// tests can shape any machine.
pub(crate) fn plan_from(existing: &dyn Fn(&Path) -> bool, dir: &Path) -> FontPlan {
    let mono = MONO_PRIMARY
        .iter()
        .find(|(file, _)| existing(&dir.join(file)))
        .map(|(file, label)| FontSource::new(file, label));
    let cjk = CJK_FALLBACK
        .iter()
        .filter(|(file, _)| existing(&dir.join(file)))
        .map(|(file, label)| FontSource::new(file, label))
        .collect();
    let bold = BOLD_PRIMARY
        .iter()
        .find(|(file, _)| existing(&dir.join(file)))
        .map(|(file, label)| FontSource::new(file, label));
    FontPlan { mono, cjk, bold }
}

/// Scans the real fonts directory.
fn resolve_plan(dir: &Path) -> FontPlan {
    plan_from(&|path| path.is_file(), dir)
}

/// Reads one font file on the real machine.
fn read_font(dir: &Path, source: &FontSource) -> std::io::Result<Vec<u8>> {
    std::fs::read(dir.join(source.file))
}

/// Loads the plan into egui's font definitions and returns the face names
/// that made it in. Families are only rebuilt when something loaded — a
/// machine with no candidates keeps egui's defaults byte-for-byte.
///
/// Family orders mirror epaint 0.36.2's defaults (verified in
/// `epaint-0.36.2/src/text/fonts.rs`): Monospace
/// `Hack, Ubuntu-Light, NotoEmoji-Regular, emoji-icon-font`; Proportional
/// `Ubuntu-Light, NotoEmoji-Regular, emoji-icon-font`. The port's faces go
/// ahead of the defaults in Monospace and between the Latin face and the
/// emoji fonts in Proportional, so CJK text resolves to a text face
/// instead of an icon font's approximation.
pub(crate) fn compose(
    definitions: &mut egui::FontDefinitions,
    plan: &FontPlan,
    read: &dyn Fn(&FontSource) -> std::io::Result<Vec<u8>>,
) -> Vec<String> {
    const DEFAULT_MONO: &[&str] = &[
        "Hack",
        "Ubuntu-Light",
        "NotoEmoji-Regular",
        "emoji-icon-font",
    ];
    const DEFAULT_PROP: &[&str] = &["Ubuntu-Light", "NotoEmoji-Regular", "emoji-icon-font"];

    let mut loaded: Vec<String> = Vec::new();
    // Returns false (and records nothing) when the file cannot be read.
    let mut insert = |source: &FontSource| -> bool {
        match read(source) {
            Ok(bytes) => {
                let mut data = egui::FontData::from_owned(bytes);
                data.index = source.index;
                definitions
                    .font_data
                    .insert(source.label.to_owned(), std::sync::Arc::new(data));
                loaded.push(source.label.to_owned());
                true
            }
            Err(_) => false,
        }
    };

    let mut mono_family: Vec<String> = Vec::new();
    let mut prop_family: Vec<String> = vec!["Ubuntu-Light".to_owned()];
    let mut bold_family: Option<Vec<String>> = None;

    if let Some(primary) = &plan.mono {
        if insert(primary) {
            mono_family.push(primary.label.to_owned());
        }
    }
    let mut prop_fallbacks: Vec<String> = Vec::new();
    for fallback in &plan.cjk {
        if insert(fallback) {
            mono_family.push(fallback.label.to_owned());
            prop_fallbacks.push(fallback.label.to_owned());
        }
    }
    // The bold lane is a family of its own (see BOLD_PRIMARY). When no
    // identity bold exists the family key still registers — pointing at
    // the regular faces — so a painter using it degrades in weight, not
    // coverage.
    if let Some(bold) = &plan.bold {
        if insert(bold) {
            bold_family = Some(vec![bold.label.to_owned()]);
        }
    }
    if loaded.is_empty() {
        return loaded;
    }

    mono_family.extend(DEFAULT_MONO.iter().map(|name| name.to_string()));
    prop_family.extend(prop_fallbacks);
    prop_family.extend(DEFAULT_PROP[1..].iter().map(|name| name.to_string()));
    let bold_family = match bold_family {
        Some(mut faces) => {
            faces.extend(DEFAULT_PROP.iter().map(|name| name.to_string()));
            faces
        }
        None => prop_family.clone(),
    };

    definitions
        .families
        .insert(egui::FontFamily::Monospace, mono_family);
    definitions
        .families
        .insert(egui::FontFamily::Proportional, prop_family);
    definitions
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY.into()), bold_family);
    loaded
}

/// Resolves the machine's faces and installs them into the context.
/// Returns the face names actually loaded (empty when the machine has
/// none of the candidates and the context keeps egui's defaults).
pub fn install(ctx: &egui::Context) -> Vec<String> {
    let dir = system_fonts_dir();
    let plan = resolve_plan(&dir);
    if plan.mono.is_none() && plan.cjk.is_empty() && plan.bold.is_none() {
        return Vec::new();
    }
    let mut definitions = egui::FontDefinitions::default();
    let loaded = compose(&mut definitions, &plan, &|source| read_font(&dir, source));
    if loaded.is_empty() {
        return loaded;
    }
    ctx.set_fonts(definitions);
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn dir_with<'a>(files: &'a [&'a str]) -> (PathBuf, impl Fn(&Path) -> bool + 'a) {
        let set: BTreeSet<String> = files.iter().map(|name| name.to_string()).collect();
        let dir = PathBuf::from("\\\\fake\\fonts");
        let existing = move |path: &Path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| set.contains(name))
        };
        (dir, existing)
    }

    fn always_ok(_source: &FontSource) -> std::io::Result<Vec<u8>> {
        Ok(vec![0x00, 0x01, 0x00, 0x00])
    }

    #[test]
    fn primary_prefers_cascadia_when_both_exist() {
        let (dir, existing) = dir_with(&["CascadiaCode.ttf", "consola.ttf", "msyh.ttc"]);
        let plan = plan_from(&existing, &dir);
        assert_eq!(
            plan.mono,
            Some(FontSource {
                file: "CascadiaCode.ttf",
                label: "Cascadia Code",
                index: 0,
            })
        );
        assert_eq!(plan.cjk.len(), 1);
        assert_eq!(plan.cjk[0].label, "Microsoft YaHei");
    }

    #[test]
    fn primary_falls_back_to_consolas() {
        let (dir, existing) = dir_with(&["consola.ttf"]);
        let plan = plan_from(&existing, &dir);
        assert_eq!(plan.mono.map(|source| source.label), Some("Consolas"));
        assert!(plan.cjk.is_empty());
    }

    #[test]
    fn nothing_on_disk_keeps_the_defaults() {
        let (dir, existing) = dir_with(&[]);
        let plan = plan_from(&existing, &dir);
        assert_eq!(plan, FontPlan::default());
    }

    #[test]
    fn cjk_fallbacks_keep_coverage_order_despite_gaps() {
        let (dir, existing) = dir_with(&["malgun.ttf", "msyh.ttc", "msgothic.ttc"]);
        let plan = plan_from(&existing, &dir);
        let labels: Vec<&str> = plan.cjk.iter().map(|s| s.label).collect();
        assert_eq!(
            labels,
            vec!["Microsoft YaHei", "MS Gothic", "Malgun Gothic"]
        );
    }

    #[test]
    fn compose_puts_the_primary_first_and_cjk_before_the_emoji_fonts() {
        let (dir, existing) = dir_with(&["consola.ttf", "msyh.ttc"]);
        let plan = plan_from(&existing, &dir);
        let mut definitions = egui::FontDefinitions::default();
        let loaded = compose(&mut definitions, &plan, &always_ok);
        assert_eq!(loaded, vec!["Consolas", "Microsoft YaHei"]);
        let mono = &definitions.families[&egui::FontFamily::Monospace];
        assert_eq!(
            mono,
            &vec![
                "Consolas".to_owned(),
                "Microsoft YaHei".to_owned(),
                "Hack".to_owned(),
                "Ubuntu-Light".to_owned(),
                "NotoEmoji-Regular".to_owned(),
                "emoji-icon-font".to_owned(),
            ]
        );
        let prop = &definitions.families[&egui::FontFamily::Proportional];
        assert_eq!(
            prop,
            &vec![
                "Ubuntu-Light".to_owned(),
                "Microsoft YaHei".to_owned(),
                "NotoEmoji-Regular".to_owned(),
                "emoji-icon-font".to_owned(),
            ]
        );
        // The loaded face lands in the registry too (labels match keys).
        for label in &loaded {
            assert!(definitions.font_data.contains_key(label));
        }
    }

    #[test]
    fn an_unreadable_font_is_skipped_without_failing_the_rest() {
        let (dir, existing) = dir_with(&["consola.ttf", "msyh.ttc"]);
        let plan = plan_from(&existing, &dir);
        let read = |source: &FontSource| -> std::io::Result<Vec<u8>> {
            if source.file == "consola.ttf" {
                Err(std::io::Error::other("locked by another process"))
            } else {
                always_ok(source)
            }
        };
        let mut definitions = egui::FontDefinitions::default();
        let loaded = compose(&mut definitions, &plan, &read);
        // Consolas failed; YaHei still lands (and takes the front of the
        // grid family — a degraded but readable terminal).
        assert_eq!(loaded, vec!["Microsoft YaHei"]);
        assert_eq!(
            definitions.families[&egui::FontFamily::Monospace][0],
            "Microsoft YaHei"
        );
    }

    #[test]
    fn everything_unreadable_leaves_the_defaults_untouched() {
        let mut definitions = egui::FontDefinitions::default();
        let before = definitions.clone();
        let plan = FontPlan {
            mono: Some(FontSource::new("consola.ttf", "Consolas")),
            cjk: vec![FontSource::new("msyh.ttc", "Microsoft YaHei")],
            ..FontPlan::default()
        };
        let read = |_source: &FontSource| -> std::io::Result<Vec<u8>> {
            Err(std::io::Error::other("unreadable"))
        };
        let loaded = compose(&mut definitions, &plan, &read);
        assert!(loaded.is_empty());
        assert_eq!(definitions, before);
    }

    #[test]
    fn bold_face_registers_as_its_own_family_with_a_safe_fallback() {
        let (dir, existing) = dir_with(&["segoeuib.ttf"]);
        let plan = plan_from(&existing, &dir);
        let mut definitions = egui::FontDefinitions::default();
        let loaded = compose(&mut definitions, &plan, &always_ok);
        assert_eq!(loaded, vec!["Segoe UI Bold"]);
        let bold_key = egui::FontFamily::Name(BOLD_FAMILY.into());
        let bold = &definitions.families[&bold_key];
        assert_eq!(
            bold,
            &vec![
                "Segoe UI Bold".to_owned(),
                "Ubuntu-Light".to_owned(),
                "NotoEmoji-Regular".to_owned(),
                "emoji-icon-font".to_owned(),
            ]
        );
        // The regular families never prefer the bold face.
        assert!(!definitions.families[&egui::FontFamily::Proportional]
            .contains(&"Segoe UI Bold".to_owned()));

        // Without any bold face the family still registers, pointing at
        // the regular faces (painters degrade in weight, not coverage).
        let (dir, existing) = dir_with(&["consola.ttf"]);
        let plan = plan_from(&existing, &dir);
        let mut definitions = egui::FontDefinitions::default();
        compose(&mut definitions, &plan, &always_ok);
        let bold = &definitions.families[&egui::FontFamily::Name(BOLD_FAMILY.into())];
        let prop = &definitions.families[&egui::FontFamily::Proportional];
        assert_eq!(bold, prop);
    }
}
