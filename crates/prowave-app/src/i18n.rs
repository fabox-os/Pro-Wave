//! Interface language. English is built in; the other languages come from
//! `locales/<code>.txt`, one `English text = Translation` line per text, compiled into the
//! program. A text without a translation shows in English.
//!
//! Pro Wave starts in English. Picking a language in Help > Language switches the interface
//! at once and saves the choice (`language.txt`, next to the recent projects), so the next start
//! is in that language too. The `PROWAVE_LANG` environment variable (a code such as `de` or
//! `pt-BR`) overrides it for one run, for testing. Text in Chinese, Japanese and Hindi is drawn
//! with the system's fonts (eframe's `system_fonts`), shaped by egui.
//!
//! Texts are looked up with [`tr`] (or [`trf`] for texts with `{}` placeholders) where they are
//! drawn, so changing the language takes effect on the next frame.
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Lang {
    En,
    ZhHans,
    Ja,
    Fr,
    De,
    Es,
    PtPt,
    PtBr,
    Fil,
    Id,
    Hi,
    Uk,
    Nb,
    Sv,
    Fi,
    Pl,
    Lt,
}

impl Lang {
    pub const ALL: [Lang; 17] = [
        Lang::En,
        Lang::ZhHans,
        Lang::Ja,
        Lang::Fr,
        Lang::De,
        Lang::Es,
        Lang::PtPt,
        Lang::PtBr,
        Lang::Fil,
        Lang::Id,
        Lang::Hi,
        Lang::Uk,
        Lang::Nb,
        Lang::Sv,
        Lang::Fi,
        Lang::Pl,
        Lang::Lt,
    ];

    /// BCP 47 code, as saved and as accepted by `PROWAVE_LANG`.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhHans => "zh-Hans",
            Lang::Ja => "ja",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Es => "es",
            Lang::PtPt => "pt-PT",
            Lang::PtBr => "pt-BR",
            Lang::Fil => "fil",
            Lang::Id => "id",
            Lang::Hi => "hi",
            Lang::Uk => "uk",
            Lang::Nb => "nb",
            Lang::Sv => "sv",
            Lang::Fi => "fi",
            Lang::Pl => "pl",
            Lang::Lt => "lt",
        }
    }

    /// The language's name in that language, as listed in Help > Language.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English (Default)",
            Lang::ZhHans => "简体中文",
            Lang::Ja => "日本語",
            Lang::Fr => "Français",
            Lang::De => "Deutsch",
            Lang::Es => "Español",
            Lang::PtPt => "Português (Portugal)",
            Lang::PtBr => "Português (Brasil)",
            Lang::Fil => "Filipino",
            Lang::Id => "Bahasa Indonesia",
            Lang::Hi => "हिन्दी",
            Lang::Uk => "Українська",
            Lang::Nb => "Norsk",
            Lang::Sv => "Svenska",
            Lang::Fi => "Suomi",
            Lang::Pl => "Polski",
            Lang::Lt => "Lietuvių",
        }
    }

    pub fn from_native_name(name: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.native_name() == name)
    }

    /// The translation file, or `None` for English.
    fn source(self) -> Option<&'static str> {
        Some(match self {
            Lang::En => return None,
            Lang::ZhHans => include_str!("../locales/zh-Hans.txt"),
            Lang::Ja => include_str!("../locales/ja.txt"),
            Lang::Fr => include_str!("../locales/fr.txt"),
            Lang::De => include_str!("../locales/de.txt"),
            Lang::Es => include_str!("../locales/es.txt"),
            Lang::PtPt => include_str!("../locales/pt-PT.txt"),
            Lang::PtBr => include_str!("../locales/pt-BR.txt"),
            Lang::Fil => include_str!("../locales/fil.txt"),
            Lang::Id => include_str!("../locales/id.txt"),
            Lang::Hi => include_str!("../locales/hi.txt"),
            Lang::Uk => include_str!("../locales/uk.txt"),
            Lang::Nb => include_str!("../locales/nb.txt"),
            Lang::Sv => include_str!("../locales/sv.txt"),
            Lang::Fi => include_str!("../locales/fi.txt"),
            Lang::Pl => include_str!("../locales/pl.txt"),
            Lang::Lt => include_str!("../locales/lt.txt"),
        })
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL
            .into_iter()
            .find(|l| l.code().eq_ignore_ascii_case(code.trim()))
    }
}

/// `English text = Translation` lines; `#` starts a comment line.
fn parse(src: &'static str) -> HashMap<&'static str, &'static str> {
    src.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once(" = "))
        .map(|(k, v)| (k.trim(), v.trim()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

fn table(lang: Lang) -> Option<&'static HashMap<&'static str, &'static str>> {
    static TABLES: [OnceLock<HashMap<&'static str, &'static str>>; 17] =
        [const { OnceLock::new() }; 17];
    let i = Lang::ALL.iter().position(|l| *l == lang)?;
    let src = lang.source()?;
    Some(TABLES[i].get_or_init(|| parse(src)))
}

#[cfg(not(test))]
mod state {
    use super::Lang;
    use std::sync::atomic::{AtomicU8, Ordering};

    static CURRENT: AtomicU8 = AtomicU8::new(0);

    pub fn get() -> Lang {
        Lang::ALL[CURRENT.load(Ordering::Relaxed) as usize % Lang::ALL.len()]
    }

    pub fn set(lang: Lang) {
        let i = Lang::ALL.iter().position(|l| *l == lang).unwrap_or(0);
        CURRENT.store(i as u8, Ordering::Relaxed);
    }
}

// Tests run in parallel threads: each test has its own language, so one test switching
// language never changes the texts another test checks.
#[cfg(test)]
mod state {
    use super::Lang;
    use std::cell::Cell;

    thread_local!(static CURRENT: Cell<Lang> = const { Cell::new(Lang::En) });

    pub fn get() -> Lang {
        CURRENT.with(|c| c.get())
    }

    pub fn set(lang: Lang) {
        CURRENT.with(|c| c.set(lang));
    }
}

/// The language the interface is in now.
pub fn current() -> Lang {
    state::get()
}

pub fn set(lang: Lang) {
    state::set(lang);
}

/// `text` in the current language (English when it has no translation).
pub fn tr(text: &str) -> &str {
    tr_in(current(), text)
}

pub fn tr_in(lang: Lang, text: &str) -> &str {
    table(lang)
        .and_then(|t| t.get(text).copied())
        .unwrap_or(text)
}

/// [`tr`] for a text with `{}` placeholders, filled in order with `args`.
pub fn trf(text: &str, args: &[&dyn Display]) -> String {
    fill(tr(text), args)
}

fn fill(pattern: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut args = args.iter();
    let mut rest = pattern;
    while let Some(at) = rest.find("{}") {
        out.push_str(&rest[..at]);
        if let Some(a) = args.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out
}

/// The language picked in Help > Language and saved, or `None` if none was ever picked.
pub fn chosen() -> Option<Lang> {
    choice::load()
}

/// Help > Language: switches to `lang` now and saves it for the next start.
pub fn choose(lang: Lang) {
    choice::save(lang);
    set(lang);
}

#[cfg(not(test))]
mod choice {
    use super::Lang;
    use std::path::PathBuf;

    fn file() -> Option<PathBuf> {
        Some(crate::recent_files::config_dir()?.join("language.txt"))
    }

    pub fn load() -> Option<Lang> {
        file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| Lang::from_code(&t))
    }

    pub fn save(lang: Lang) {
        let Some(f) = file() else { return };
        let written = f
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&f, format!("{}\n", lang.code())));
        if let Err(e) = written {
            tracing::warn!("cannot save the language to {}: {e}", f.display());
        }
    }
}

// Tests must not change the user's settings: the choice lives in the test's thread only.
#[cfg(test)]
mod choice {
    use super::Lang;
    use std::cell::Cell;

    thread_local!(static CHOSEN: Cell<Option<Lang>> = const { Cell::new(None) });

    pub fn load() -> Option<Lang> {
        CHOSEN.with(|c| c.get())
    }

    pub fn save(lang: Lang) {
        CHOSEN.with(|c| c.set(Some(lang)));
    }
}

/// Picks the language at startup: `PROWAVE_LANG`, else the saved choice, else English.
pub fn init() {
    let from_env = std::env::var("PROWAVE_LANG")
        .ok()
        .and_then(|v| Lang::from_code(&v));
    let lang = from_env.or_else(chosen).unwrap_or(Lang::En);
    tracing::info!("interface language: {}", lang.code());
    set(lang);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn codes_and_native_names_round_trip() {
        for l in Lang::ALL {
            assert_eq!(Lang::from_code(l.code()), Some(l), "{l:?}");
            assert_eq!(Lang::from_native_name(l.native_name()), Some(l), "{l:?}");
        }
        assert_eq!(Lang::from_code("PT-br\n"), Some(Lang::PtBr));
        assert_eq!(Lang::from_code("ru"), None, "not translated");
    }

    #[test]
    fn english_until_a_language_is_picked_then_that_one_is_kept() {
        assert_eq!(chosen(), None);
        assert_eq!(current(), Lang::En);
        choose(Lang::Uk);
        assert_eq!((chosen(), current()), (Some(Lang::Uk), Lang::Uk));
        // The next start reads the saved choice.
        set(Lang::En);
        init();
        assert_eq!(current(), Lang::Uk);
        set(Lang::En);
    }

    #[test]
    fn placeholders_are_filled_in_order_and_missing_texts_stay_english() {
        assert_eq!(fill("a {} b {}", &[&1, &"x"]), "a 1 b x");
        assert_eq!(fill("no args {}", &[]), "no args ");
        set(Lang::De);
        assert_eq!(tr("No such text, really"), "No such text, really");
        set(Lang::En);
    }

    /// Texts that are the same in every language and need no translation line.
    const UNTRANSLATED: [&str; 3] = ["MIDI", "EQ", "Pro Wave"];

    /// The Rust string literal starting at `s` (just after its opening quote), unescaped.
    fn literal(s: &str) -> Option<String> {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    }

    /// Every English text the interface shows: `tr("…")` / `trf("…")` in the code (outside
    /// tests), plus the tables translated where they are drawn.
    pub(crate) fn interface_texts() -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        let mut add = |k: &str| {
            if !UNTRANSLATED.contains(&k) && !keys.iter().any(|x| x == k) {
                keys.push(k.to_string());
            }
        };
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<_> = std::fs::read_dir(&src)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .filter(|p| !p.ends_with("i18n.rs"))
            .collect();
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            let code = [
                "\n#[cfg(test)]\nmod tests",
                "\n#[cfg(test)]\npub(crate) mod tests",
            ]
            .iter()
            .filter_map(|m| text.find(m))
            .min()
            .map_or(text.as_str(), |end| &text[..end]);
            for call in ["tr(", "trf("] {
                for (at, _) in code.match_indices(call) {
                    let ident_before = code[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_');
                    let rest = code[at + call.len()..].trim_start();
                    if ident_before || !rest.starts_with('"') {
                        continue;
                    }
                    if let Some(k) = literal(&rest[1..]) {
                        add(&k);
                    }
                }
            }
        }
        for (title, items) in crate::menus::MENUS {
            add(title);
            for it in items.iter().filter(|i| **i != "-") {
                add(it);
            }
        }
        for it in crate::menus::all_entries() {
            if Lang::from_native_name(it).is_none() {
                add(it);
            }
        }
        for t in crate::tracks_area::TOOLS {
            add(t);
        }
        for (_, t, _, _) in crate::inspector::PIANO_ROWS {
            add(t);
        }
        for (t, _) in crate::inspector::QUANTIZE {
            add(t);
        }
        for (t, _) in crate::inspector::FADE_CURVES {
            add(t);
        }
        for (t, _) in crate::note_edit::GRIDS {
            add(t);
        }
        for (t, _) in crate::note_edit::MODES {
            add(t);
        }
        for t in crate::mixer::FILTERS {
            add(t);
        }
        for c in crate::library::CATEGORIES {
            add(c.name);
        }
        for t in crate::start_screen::Template::ALL {
            add(t.name());
            add(t.about());
        }
        keys
    }

    fn placeholders(s: &str) -> usize {
        s.matches("{}").count()
    }

    #[test]
    fn every_language_translates_every_text() {
        let keys = interface_texts();
        assert!(keys.len() > 300, "found {} texts", keys.len());
        let mut problems = Vec::new();
        for lang in Lang::ALL.into_iter().filter(|l| *l != Lang::En) {
            let table = table(lang).unwrap();
            for k in &keys {
                match table.get(k.as_str()) {
                    None => problems.push(format!("{}: missing \"{k}\"", lang.code())),
                    Some(v) if placeholders(v) != placeholders(k) => {
                        problems.push(format!("{}: placeholders differ in \"{k}\"", lang.code()))
                    }
                    Some(_) => {}
                }
            }
            for k in table.keys() {
                if !keys.iter().any(|x| x == k) {
                    problems.push(format!("{}: not used any more: \"{k}\"", lang.code()));
                }
            }
            // Every line is `text = translation`; a mistyped line would be silently dropped.
            let src = lang.source().unwrap();
            for l in src.lines() {
                if !l.trim().is_empty() && !l.starts_with('#') && !l.contains(" = ") {
                    problems.push(format!("{}: malformed line \"{l}\"", lang.code()));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "{} problems:\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    #[test]
    fn every_language_switches_the_menus() {
        for lang in Lang::ALL.into_iter().filter(|l| *l != Lang::En) {
            set(lang);
            for k in ["Language", "Save", "Cancel", "Open Project", "New Project"] {
                assert_ne!(tr(k), k, "{}: {k}", lang.code());
            }
            assert_eq!(
                trf("{} Notes selected", &[&3]).matches('3').count(),
                1,
                "{}",
                lang.code()
            );
        }
        set(Lang::En);
        assert_eq!(tr("File"), "File");
    }

    /// Latin and Cyrillic languages are drawn with egui's built-in font; Chinese, Japanese
    /// and Hindi come from the system's fonts (checked in the layout pictures instead).
    #[test]
    fn built_in_fonts_have_every_letter_of_the_latin_and_cyrillic_languages() {
        let ctx = egui::Context::default();
        let mut missing = Vec::new();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let font = egui::FontId::proportional(13.0);
            for lang in Lang::ALL
                .into_iter()
                .filter(|l| ![Lang::En, Lang::ZhHans, Lang::Ja, Lang::Hi].contains(l))
            {
                let text: String = table(lang).unwrap().values().copied().collect();
                let name = lang.native_name();
                ui.fonts_mut(|f| {
                    for c in text.chars().chain(name.chars()) {
                        if !f.has_glyph(&font, c) && !missing.contains(&(lang.code(), c)) {
                            missing.push((lang.code(), c));
                        }
                    }
                });
            }
        });
        out.textures_delta.clear();
        assert!(missing.is_empty(), "letters without a glyph: {missing:?}");
    }
}
