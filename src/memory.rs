//! What the reader remembers between runs: the last book, where you were in every book, how big
//! the text is, the two colours, and whether the side panels were out. Kept as JSON in
//! `~/.local/state/slipstream/reader.json`.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const ZOOM_MIN: f64 = 0.5;
pub const ZOOM_MAX: f64 = 4.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub last: Option<PathBuf>,
    pub zoom: f64,
    /// Where each slider was moved to along the run of colours (`palette::at`), or `None` while
    /// that colour follows the desktop's theme.
    pub page_choice: Option<f64>,
    pub text_choice: Option<f64>,
    pub panels: bool,
    /// Where you were in each book, by its path.
    pub places: BTreeMap<String, Place>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Place {
    pub chapter: usize,
    /// How far down the chapter, from 0 at its top to 1 at its end.
    pub fraction: f64,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            last: None,
            zoom: 1.0,
            page_choice: None,
            text_choice: None,
            panels: false,
            places: BTreeMap::new(),
        }
    }
}

impl Memory {
    /// The file's memory, or a fresh one when it is missing or can't be read.
    pub fn read(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Memory>(&text).ok())
            .map(Memory::sane)
            .unwrap_or_default()
    }

    /// Written beside the file, then moved over it, so a crash never leaves half of one.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let new = path.with_extension("json.new");
        std::fs::write(&new, text)?;
        std::fs::rename(&new, path)
    }

    pub fn place(&self, book: &Path) -> Option<Place> {
        self.places.get(&key(book)).copied()
    }

    pub fn set_place(&mut self, book: &Path, place: Place) {
        self.places.insert(key(book), place);
    }

    /// Values from a hand-edited or older file brought back into range.
    fn sane(mut self) -> Self {
        let unit = |value: f64, default: f64| {
            if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                default
            }
        };
        let choice = |value: Option<f64>| value.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
        self.zoom = if self.zoom.is_finite() {
            self.zoom.clamp(ZOOM_MIN, ZOOM_MAX)
        } else {
            1.0
        };
        self.page_choice = choice(self.page_choice);
        self.text_choice = choice(self.text_choice);
        for place in self.places.values_mut() {
            place.fraction = unit(place.fraction, 0.0);
        }
        self
    }
}

pub fn path() -> PathBuf {
    slipstream_config::screens::state_dir().join("reader.json")
}

fn key(book: &Path) -> String {
    std::fs::canonicalize(book)
        .unwrap_or_else(|_| book.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("slipstream-reader-memory-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_missing_file_is_a_fresh_memory() {
        assert_eq!(Memory::read(&scratch("missing.json")), Memory::default());
    }

    #[test]
    fn what_is_written_is_read_back() {
        let path = scratch("round.json");
        let mut memory = Memory {
            zoom: 1.5,
            panels: true,
            ..Memory::default()
        };
        memory.set_place(
            Path::new("/books/a.epub"),
            Place {
                chapter: 3,
                fraction: 0.25,
            },
        );
        memory.write(&path).unwrap();
        let back = Memory::read(&path);
        assert_eq!(back, memory);
        assert_eq!(
            back.place(Path::new("/books/a.epub")).map(|p| p.chapter),
            Some(3)
        );
    }

    #[test]
    fn values_out_of_range_are_brought_back() {
        let path = scratch("wild.json");
        std::fs::write(
            &path,
            r#"{"zoom": 40, "page_choice": -2, "places": {"/b.epub": {"chapter": 1, "fraction": 9}}}"#,
        )
        .unwrap();
        let memory = Memory::read(&path);
        assert_eq!(memory.zoom, ZOOM_MAX);
        assert_eq!(memory.page_choice, Some(0.0));
        assert_eq!(memory.text_choice, None);
        assert_eq!(memory.places["/b.epub"].fraction, 1.0);
    }

    #[test]
    fn colours_from_before_the_theme_was_followed_are_let_go() {
        let path = scratch("old.json");
        std::fs::write(&path, r#"{"page": 0.85, "text": 0.12}"#).unwrap();
        let memory = Memory::read(&path);
        assert_eq!((memory.page_choice, memory.text_choice), (None, None));
    }

    #[test]
    fn a_broken_file_is_a_fresh_memory() {
        let path = scratch("broken.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Memory::read(&path), Memory::default());
    }
}
