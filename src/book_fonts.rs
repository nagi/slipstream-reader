//! What a book says about its fonts, from its stylesheets: the font its text is set in, every
//! family it names, and which of them it carries inside itself, in what format.

use crate::obfuscation::Scheme;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookFonts {
    /// The family the body text asks for first, as the book spells it. It may be a generic one,
    /// like `serif`.
    pub body: Option<String>,
    /// Each named family, the body's first, then those the book carries, then the rest.
    pub families: Vec<Family>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Family {
    pub name: String,
    /// The faces the book carries for it (regular, italic, …). None when it isn't embedded.
    pub faces: Vec<Face>,
    /// `@font-face` rules for it whose file isn't in the book.
    pub missing: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub format: Format,
    pub obfuscated: Option<Scheme>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Format {
    TrueType,
    OpenType,
    Woff,
    Woff2,
    Collection,
    Svg,
    Unknown,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::TrueType => "TrueType",
            Format::OpenType => "OpenType",
            Format::Woff => "WOFF",
            Format::Woff2 => "WOFF2",
            Format::Collection => "TrueType collection",
            Format::Svg => "SVG font",
            Format::Unknown => "unrecognised format",
        }
    }

    /// From the file's first bytes, or its name when they say nothing.
    pub fn of(bytes: &[u8], path: &str) -> Format {
        match bytes.get(..4) {
            Some(b"wOF2") => return Format::Woff2,
            Some(b"wOFF") => return Format::Woff,
            Some(b"OTTO") => return Format::OpenType,
            Some(b"ttcf") => return Format::Collection,
            Some([0, 1, 0, 0]) | Some(b"true") => return Format::TrueType,
            _ => {}
        }
        let extension = path
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "ttf" => Format::TrueType,
            "otf" => Format::OpenType,
            "woff" => Format::Woff,
            "woff2" => Format::Woff2,
            "ttc" => Format::Collection,
            "svg" => Format::Svg,
            _ => Format::Unknown,
        }
    }
}

impl Family {
    pub fn embedded(&self) -> bool {
        !self.faces.is_empty()
    }

    /// The faces' formats, each once.
    pub fn formats(&self) -> Vec<Format> {
        let mut formats: Vec<Format> = self.faces.iter().map(|face| face.format).collect();
        formats.sort();
        formats.dedup();
        formats
    }

    pub fn obfuscation(&self) -> Option<Scheme> {
        self.faces.iter().find_map(|face| face.obfuscated)
    }
}

/// Families every browser has, which name a kind of font rather than one.
const GENERIC: [&str; 13] = [
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "math",
    "emoji",
    "fangsong",
];

pub fn is_generic(family: &str) -> bool {
    GENERIC.iter().any(|g| g.eq_ignore_ascii_case(family))
}

/// The book's fonts from its stylesheets, each with the path it was read from. `read` gives a
/// file of the book by its absolute decoded path, put back as it was, and whether it had been
/// obfuscated.
pub fn survey(
    sheets: &[(String, String)],
    read: impl Fn(&str) -> Option<(Vec<u8>, Option<Scheme>)>,
) -> BookFonts {
    // Each family named, by its lower-case name: as spelled first, how often it leads a
    // `font-family`, and its embedded faces.
    let mut families: Vec<(String, Family, usize)> = Vec::new();
    // The best guess so far at the body text's font, and how sure: 3 for `body`, 2 for `html`
    // and 1 for `p`. A later rule as sure wins, as it would in the cascade.
    let mut body: Option<(u8, String)> = None;

    for (path, css) in sheets {
        for (prelude, declarations) in rules(css) {
            let prelude = prelude.trim();
            if prelude.eq_ignore_ascii_case("@font-face") {
                let mut name = None;
                let mut sources = Vec::new();
                for (property, value) in declarations_of(&declarations) {
                    match property.as_str() {
                        "font-family" => name = families_of(&value).into_iter().next(),
                        "src" => sources = urls(&value),
                        _ => {}
                    }
                }
                // Word's `@font-face`s only describe a font's metrics, with no file to load.
                let Some(name) = name.filter(|_| !sources.is_empty()) else {
                    continue;
                };
                let at = entry(&mut families, &name);
                let found = sources.iter().find_map(|url| {
                    let file = resolve(path, url)?;
                    let (bytes, obfuscated) = read(&file)?;
                    Some(Face {
                        format: Format::of(&bytes, &file),
                        obfuscated,
                    })
                });
                match found {
                    Some(face) => families[at].1.faces.push(face),
                    None => families[at].1.missing += 1,
                }
                continue;
            }
            if prelude.starts_with('@') {
                continue;
            }
            for (property, value) in declarations_of(&declarations) {
                let named = match property.as_str() {
                    "font-family" => families_of(&value),
                    "font" => families_of(&shorthand_families(&value)),
                    _ => continue,
                };
                let Some(lead) = named.into_iter().next() else {
                    continue;
                };
                if !is_generic(&lead) {
                    let at = entry(&mut families, &lead);
                    families[at].2 += 1;
                }
                let sure = body_selector(prelude);
                if sure > 0 && body.as_ref().is_none_or(|(was, _)| sure >= *was) {
                    body = Some((sure, lead));
                }
            }
        }
    }

    // With no rule for the body, the font used most is the best guess.
    let body = body.map(|(_, name)| name).or_else(|| {
        families
            .iter()
            .filter(|(_, _, uses)| *uses > 0)
            .max_by_key(|(_, _, uses)| *uses)
            .map(|(_, family, _)| family.name.clone())
    });
    let body_key = body.as_ref().map(|name| name.to_lowercase());
    // A family only declared, never used and not in the book, says nothing about it.
    let mut listed: Vec<(String, Family, usize)> = families
        .into_iter()
        .filter(|(_, family, uses)| *uses > 0 || family.embedded())
        .collect();
    listed.sort_by_key(|(key, family, uses)| {
        (
            Some(key) != body_key.as_ref(),
            !family.embedded(),
            std::cmp::Reverse(*uses),
        )
    });
    BookFonts {
        body,
        families: listed.into_iter().map(|(_, family, _)| family).collect(),
    }
}

/// Where `name` is in `families`, added if it isn't yet. Family names ignore case.
fn entry(families: &mut Vec<(String, Family, usize)>, name: &str) -> usize {
    let key = name.to_lowercase();
    if let Some(at) = families.iter().position(|(k, _, _)| *k == key) {
        return at;
    }
    families.push((
        key,
        Family {
            name: name.to_string(),
            faces: Vec::new(),
            missing: 0,
        },
        0,
    ));
    families.len() - 1
}

/// How surely a selector is the body text's: 3 for `body`, 2 for `html` or `:root`, 1 for `p`,
/// 0 for anything else.
fn body_selector(prelude: &str) -> u8 {
    prelude
        .split(',')
        .map(|selector| {
            let last = selector
                .split([' ', '>', '+', '~'])
                .rfind(|part| !part.is_empty())
                .unwrap_or_default()
                .to_ascii_lowercase();
            // `body.calibre` and the like are still the body.
            let element = last
                .split(['.', '#', ':', '['])
                .next()
                .unwrap_or_default()
                .to_string();
            match element.as_str() {
                "body" => 3,
                "html" => 2,
                "" if last == ":root" => 2,
                "p" => 1,
                _ => 0,
            }
        })
        .max()
        .unwrap_or(0)
}

/// Each innermost block in a stylesheet, as its prelude and its declarations, with comments taken
/// out. Blocks inside `@media` and the like are returned as if they weren't.
fn rules(css: &str) -> Vec<(String, String)> {
    let css = without_comments(css);
    let mut out = Vec::new();
    // Each open block: its prelude, where its body starts, and whether a block opened inside it.
    let mut open: Vec<(String, usize, bool)> = Vec::new();
    let mut prelude_start = 0;
    let mut quote: Option<char> = None;
    for (at, ch) in css.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '{' => {
                if let Some(parent) = open.last_mut() {
                    parent.2 = true;
                }
                let prelude = css[prelude_start..at].trim();
                // A prelude starts after the last `;` or `}` before it (for `@import …;`).
                let prelude = prelude.rsplit(';').next().unwrap_or_default().trim();
                open.push((prelude.to_string(), at + 1, false));
                prelude_start = at + 1;
            }
            '}' => {
                if let Some((prelude, start, nested)) = open.pop()
                    && !nested
                {
                    out.push((prelude, css[start..at].to_string()));
                }
                prelude_start = at + 1;
            }
            ';' if open.is_empty() => prelude_start = at + 1,
            _ => {}
        }
    }
    out
}

fn without_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The declarations in a block, as lower-case property and value, `!important` taken off.
fn declarations_of(block: &str) -> Vec<(String, String)> {
    split_outside(block, ';')
        .into_iter()
        .filter_map(|declaration| {
            let (property, value) = declaration.split_once(':')?;
            let value = value.trim();
            let value = value
                .strip_suffix("!important")
                .map(str::trim)
                .unwrap_or(value);
            Some((property.trim().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

/// `text` split at `separator`, but not inside quotes or brackets (a `data:` URL has `;` in it).
fn split_outside(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (at, ch) in text.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == separator && depth <= 0 => {
                parts.push(&text[start..at]);
                start = at + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The families in a `font-family` value, in order, unquoted. CSS-wide keywords aren't families.
fn families_of(value: &str) -> Vec<String> {
    split_outside(value, ',')
        .into_iter()
        .map(|family| {
            let family = family.trim();
            let unquoted = family
                .strip_prefix('"')
                .and_then(|f| f.strip_suffix('"'))
                .or_else(|| family.strip_prefix('\'').and_then(|f| f.strip_suffix('\'')));
            match unquoted {
                Some(name) => name.trim().to_string(),
                // Unquoted names are their words with single spaces.
                None => family.split_whitespace().collect::<Vec<_>>().join(" "),
            }
        })
        .filter(|family| {
            !family.is_empty()
                && ![
                    "inherit",
                    "initial",
                    "unset",
                    "revert",
                    "revert-layer",
                    "default",
                ]
                .iter()
                .any(|keyword| keyword.eq_ignore_ascii_case(family))
        })
        .collect()
}

/// The families at the end of a `font` shorthand: everything after its size (and line height).
fn shorthand_families(value: &str) -> String {
    const SIZES: [&str; 9] = [
        "xx-small", "x-small", "small", "medium", "large", "x-large", "xx-large", "larger",
        "smaller",
    ];
    let words = split_outside(value, ' ');
    let size = words.iter().rposition(|word| {
        let word = word.trim().to_ascii_lowercase();
        word.starts_with(|c: char| c.is_ascii_digit() || c == '.')
            || word.contains('/')
            || SIZES.contains(&word.as_str())
    });
    match size {
        Some(at) => words[at + 1..].join(" "),
        // `font: caption` and the like name a system font, not a family.
        None => String::new(),
    }
}

/// The `url(…)`s in an `src` value, in order. `local(…)` fonts aren't in the book.
fn urls(value: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.to_ascii_lowercase().find("url(") {
        let after = &rest[start + 4..];
        let Some(end) = after.find(')') else { break };
        let url = after[..end].trim().trim_matches(['"', '\'']).trim();
        if !url.is_empty() && !url.starts_with("data:") {
            found.push(url.to_string());
        }
        rest = &after[end + 1..];
    }
    found
}

/// `url` from the stylesheet at `sheet`, as an absolute decoded path in the book.
fn resolve(sheet: &str, url: &str) -> Option<String> {
    let url = url.split(['#', '?']).next()?;
    if url.contains("://") {
        return None;
    }
    let url = crate::book::percent_decoded(url);
    let sheet = crate::book::percent_decoded(sheet);
    let mut parts: Vec<&str> = if url.starts_with('/') {
        Vec::new()
    } else {
        let mut dir: Vec<&str> = sheet.split('/').filter(|p| !p.is_empty()).collect();
        dir.pop();
        dir
    };
    for part in url.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(css: &str) -> Vec<(String, String)> {
        vec![("/OEBPS/Styles/book.css".to_string(), css.to_string())]
    }

    fn no_files(_: &str) -> Option<(Vec<u8>, Option<Scheme>)> {
        None
    }

    #[test]
    fn the_body_font_and_its_embedded_faces_are_found() {
        let css = r#"
            /* The book's own font. */
            @font-face { font-family: "Charis SIL"; src: url(../Fonts/CharisSILR.ttf); }
            @font-face { font-family: "Charis SIL"; font-style: italic;
                         src: url("../Fonts/CharisSILI.ttf") format("truetype"); }
            body { font-family: "Charis SIL", Georgia, serif; }
            h1 { font-family: 'Trajan Pro', serif; }
            .small { font: italic 0.8em/1.2 Georgia, serif; }
        "#;
        let read = |path: &str| match path {
            "/OEBPS/Fonts/CharisSILR.ttf" => Some((vec![0, 1, 0, 0, 9], None)),
            "/OEBPS/Fonts/CharisSILI.ttf" => Some((b"wOF2..".to_vec(), Some(Scheme::Idpf))),
            _ => None,
        };
        let fonts = survey(&sheet(css), read);
        assert_eq!(fonts.body.as_deref(), Some("Charis SIL"));
        let names: Vec<&str> = fonts.families.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names[0], "Charis SIL");
        assert!(names.contains(&"Trajan Pro") && names.contains(&"Georgia"));
        let charis = &fonts.families[0];
        assert_eq!(charis.formats(), [Format::TrueType, Format::Woff2]);
        assert_eq!(charis.obfuscation(), Some(Scheme::Idpf));
        assert!(!fonts.families[1].embedded());
    }

    #[test]
    fn a_font_face_whose_file_is_missing_is_not_embedded() {
        let css = "@font-face { font-family: Gone; src: url(gone.otf) }  p { font-family: Gone } \
                   @font-face { font-family: Metrics; panose-1: 2 4 5 3 5 4 6 3 2 4 }";
        let fonts = survey(&sheet(css), no_files);
        assert_eq!(fonts.body.as_deref(), Some("Gone"));
        assert!(!fonts.families[0].embedded());
        assert_eq!(fonts.families[0].missing, 1);
        // Declared for its metrics only, and never used.
        assert_eq!(fonts.families.len(), 1);
    }

    #[test]
    fn body_beats_a_paragraph_and_a_generic_family_is_kept_as_the_body() {
        let css = "body.calibre { font-family: serif } p.text { font-family: Palatino }";
        let fonts = survey(&sheet(css), no_files);
        assert_eq!(fonts.body.as_deref(), Some("serif"));
        assert_eq!(fonts.families.len(), 1);
        assert_eq!(fonts.families[0].name, "Palatino");
    }

    #[test]
    fn without_a_body_rule_the_most_used_font_is_the_body() {
        let css = ".a { font-family: Minion Pro } .b { font-family: 'minion pro' } \
                   .c { font-family: Futura }";
        let fonts = survey(&sheet(css), no_files);
        assert_eq!(fonts.body.as_deref(), Some("Minion Pro"));
    }

    #[test]
    fn rules_inside_media_queries_count_and_data_urls_do_not_split() {
        let css = "@media screen { body { font-family: Bookerly; } } \
                   @font-face { font-family: Inline; src: url(data:font/woff2;base64,AAAA) }";
        let fonts = survey(&sheet(css), no_files);
        assert_eq!(fonts.body.as_deref(), Some("Bookerly"));
        assert!(!fonts.families.iter().any(|f| f.name == "Inline"));
    }

    #[test]
    fn paths_resolve_from_the_stylesheet() {
        let sheet = "/OEBPS/Styles/book.css";
        assert_eq!(
            resolve(sheet, "../Fonts/A%20B.otf").as_deref(),
            Some("/OEBPS/Fonts/A B.otf")
        );
        assert_eq!(
            resolve(sheet, "fonts/a.ttf").as_deref(),
            Some("/OEBPS/Styles/fonts/a.ttf")
        );
        assert_eq!(
            resolve("/stylesheet.css", "fonts/00001.ttf").as_deref(),
            Some("/fonts/00001.ttf")
        );
        assert_eq!(resolve(sheet, "https://example.com/a.ttf"), None);
    }

    #[test]
    fn formats_come_from_the_bytes_before_the_name() {
        assert_eq!(Format::of(b"OTTO....", "a.ttf"), Format::OpenType);
        assert_eq!(Format::of(&[0, 1, 0, 0], "a.otf"), Format::TrueType);
        assert_eq!(Format::of(b"????", "a.woff"), Format::Woff);
    }
}
