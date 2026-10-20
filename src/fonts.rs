//! The fonts the reader can set a book in instead of its own, carried inside the reader and served
//! to the page from `epub://book/.slipstream/fonts/`, so nothing needs installing. Their OFL
//! licences are in `assets/fonts/`.

/// Where the page loads the reader's own fonts from, beside the book's files.
pub const PREFIX: &str = "/.slipstream/fonts/";

/// A font to read in.
pub struct Choice {
    /// What memory keeps.
    pub key: &'static str,
    /// What the menu shows.
    pub label: &'static str,
    /// The family it is used under on the page, apart from any the book names.
    family: &'static str,
    /// What the page falls back to while it loads, or if it can't.
    fallback: &'static str,
    faces: &'static [FontFace],
}

struct FontFace {
    file: &'static str,
    bytes: &'static [u8],
    style: &'static str,
    /// A range for a variable font.
    weight: &'static str,
}

macro_rules! face {
    ($file:literal, $style:literal, $weight:literal) => {
        FontFace {
            file: $file,
            bytes: include_bytes!(concat!("../assets/fonts/", $file)),
            style: $style,
            weight: $weight,
        }
    };
}

/// The fonts offered, after the book's own. Chosen for reading long text on a screen.
pub const CHOICES: &[Choice] = &[
    // Made for Google Play Books: a serif for reading on screens, sturdy at small sizes.
    Choice {
        key: "literata",
        label: "Literata",
        family: "Slipstream Literata",
        fallback: "serif",
        faces: &[
            face!("Literata.woff2", "normal", "200 900"),
            face!("Literata-Italic.woff2", "italic", "200 900"),
        ],
    },
    // Adobe's text serif, with optical sizes: calm and even over a long page.
    Choice {
        key: "source-serif",
        label: "Source Serif",
        family: "Slipstream Source Serif",
        fallback: "serif",
        faces: &[
            face!("SourceSerif4.woff2", "normal", "200 900"),
            face!("SourceSerif4-Italic.woff2", "italic", "200 900"),
        ],
    },
    // Garamond as the old books were set: classic, lighter on the page.
    Choice {
        key: "eb-garamond",
        label: "EB Garamond",
        family: "Slipstream EB Garamond",
        fallback: "serif",
        faces: &[
            face!("EBGaramond.woff2", "normal", "400 800"),
            face!("EBGaramond-Italic.woff2", "italic", "400 800"),
        ],
    },
    // A sans made for low vision, with letters that can't be taken for one another.
    Choice {
        key: "atkinson",
        label: "Atkinson Hyperlegible",
        family: "Slipstream Atkinson Hyperlegible",
        fallback: "sans-serif",
        faces: &[
            face!("AtkinsonHyperlegible-Regular.ttf", "normal", "400"),
            face!("AtkinsonHyperlegible-Italic.ttf", "italic", "400"),
            face!("AtkinsonHyperlegible-Bold.ttf", "normal", "700"),
            face!("AtkinsonHyperlegible-BoldItalic.ttf", "italic", "700"),
        ],
    },
];

/// What the menu calls following the book's own fonts.
pub const BOOKS_OWN: &str = "The book’s own";

pub fn choice(key: &str) -> Option<&'static Choice> {
    CHOICES.iter().find(|choice| choice.key == key)
}

/// One of the reader's fonts, asked for by the page at `path`.
pub fn read(path: &str) -> Option<(Vec<u8>, String)> {
    let file = path.strip_prefix(PREFIX)?;
    let face = CHOICES
        .iter()
        .flat_map(|choice| choice.faces)
        .find(|face| face.file == file)?;
    let media = if file.ends_with(".woff2") {
        "font/woff2"
    } else {
        "font/ttf"
    };
    Some((face.bytes.to_vec(), media.to_string()))
}

/// The page's text in `choice`, over whatever the book asks for. Code keeps its fixed width.
pub fn css(choice: &Choice) -> String {
    let mut css = String::new();
    for face in choice.faces {
        let format = if face.file.ends_with(".woff2") {
            "woff2"
        } else {
            "truetype"
        };
        css.push_str(&format!(
            "@font-face {{ font-family: \"{}\"; src: url(\"epub://book{PREFIX}{}\") format(\"{format}\"); \
             font-style: {}; font-weight: {}; font-display: block; }}\n",
            choice.family, face.file, face.style, face.weight
        ));
    }
    css.push_str(&format!(
        "body, body *:not(code):not(pre):not(kbd):not(samp):not(tt):not(var) {{ \
         font-family: \"{}\", {} !important; }}\n",
        choice.family, choice.fallback
    ));
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_is_served_with_its_type() {
        for choice in CHOICES {
            for face in choice.faces {
                let (bytes, media) = read(&format!("{PREFIX}{}", face.file)).unwrap();
                assert!(bytes.len() > 10_000, "{}", face.file);
                assert!(media.starts_with("font/"));
            }
        }
        assert!(read("/.slipstream/fonts/../../etc/passwd").is_none());
        assert!(read("/OEBPS/Literata.woff2").is_none());
    }

    #[test]
    fn the_stylesheet_loads_each_face_and_sets_the_text_in_it() {
        let css = css(choice("literata").unwrap());
        assert!(css.contains("epub://book/.slipstream/fonts/Literata-Italic.woff2"));
        assert!(css.contains("font-family: \"Slipstream Literata\", serif !important"));
        assert!(choice("comic-sans").is_none());
    }
}
