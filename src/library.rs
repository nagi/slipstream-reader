//! The books on the shelf: every EPUB in `~/Dropbox/Books` and the folders inside it, each with
//! its title, its author and a small picture of its cover.
//!
//! Covers are read out of the books once, shrunk, and kept as PNGs in the cache folder, named for
//! the book and when it last changed, so the shelf fills in at once the next time.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use gtk4::{gdk_pixbuf::Pixbuf, gio, glib};
use rbook::Epub;

/// How tall a cover is kept, in pixels: twice what the shelf shows, for screens at 2×.
const COVER_PX: i32 = 540;

/// Where the books are.
pub fn dir() -> PathBuf {
    glib::home_dir().join("Dropbox/Books")
}

/// Where the shrunk covers are kept.
pub fn cache_dir() -> PathBuf {
    glib::user_cache_dir().join("slipstream-reader/covers")
}

/// One book on the shelf.
#[derive(Debug, Clone, PartialEq)]
pub struct Shelved {
    pub path: PathBuf,
    pub title: String,
    pub author: Option<String>,
    /// The shrunk cover, when the book has one.
    pub cover: Option<PathBuf>,
}

/// Every EPUB under `dir`, however deep, in order of their names. Hidden files and folders, and
/// anything that can't be read, are passed over.
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(path),
                Ok(_) if is_epub(&path) => found.push(path),
                _ => {}
            }
        }
    }
    found.sort_by_key(|path| sort_key(path));
    found
}

fn is_epub(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("epub"))
}

/// A book's name as the shelf sorts it: its file name, without a leading "The" or "A".
fn sort_key(path: &Path) -> String {
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = name.strip_prefix(article) {
            return rest.to_string();
        }
    }
    name
}

/// The book's title, author and cover. Slow: it opens the book, so it is called off the main
/// thread.
pub fn describe(path: &Path) -> Shelved {
    let fallback = || Shelved {
        path: path.to_path_buf(),
        title: tidy_name(path),
        author: None,
        cover: None,
    };
    let Ok(epub) = Epub::open(path) else {
        return fallback();
    };
    let metadata = epub.metadata();
    let title = metadata
        .title()
        .map(|title| title.value().trim().to_string())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| tidy_name(path));
    let author = metadata
        .creators()
        .map(|creator| creator.value().trim().to_string())
        .find(|name| !name.is_empty());
    let cover = cached_cover(path, &epub);
    Shelved {
        path: path.to_path_buf(),
        title,
        author,
        cover,
    }
}

/// The book's shrunk cover in the cache, made now if it isn't there yet.
fn cached_cover(path: &Path, epub: &Epub) -> Option<PathBuf> {
    let kept = cache_dir().join(format!("{:016x}.png", cover_key(path)));
    if kept.is_file() {
        return Some(kept);
    }
    let bytes = cover_bytes(epub)?;
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
    let pixbuf =
        Pixbuf::from_stream_at_scale(&stream, -1, COVER_PX, true, gio::Cancellable::NONE).ok()?;
    std::fs::create_dir_all(cache_dir()).ok()?;
    // Written beside it and moved into place, so a half-written cover is never shown.
    let part = kept.with_extension("part");
    pixbuf.savev(&part, "png", &[]).ok()?;
    std::fs::rename(&part, &kept).ok()?;
    Some(kept)
}

/// The cover's picture: the one the book names, or failing that the first picture it calls a
/// cover.
fn cover_bytes(epub: &Epub) -> Option<Vec<u8>> {
    let manifest = epub.manifest();
    if let Some(bytes) = manifest
        .cover_image()
        .and_then(|cover| cover.read_bytes().ok())
    {
        return Some(bytes);
    }
    manifest
        .images()
        .find(|image| {
            let href = image.href().path().as_str().to_lowercase();
            href.contains("cover") || image.id().to_lowercase().contains("cover")
        })
        .and_then(|image| image.read_bytes().ok())
}

/// The book and when it last changed, so a book replaced with a new copy gets a new cover.
fn cover_key(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut hasher);
        if let Ok(changed) = meta.modified()
            && let Ok(since) = changed.duration_since(UNIX_EPOCH)
        {
            since.as_secs().hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// A file name as a title: `Broken Money (Lyn Alden).epub` is "Broken Money (Lyn Alden)", and
/// a download site's name in brackets at the end is left off.
pub fn tidy_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut name = stem.trim().to_string();
    while let Some(open) = name.rfind(" (")
        && name.ends_with(')')
        && name[open..].to_lowercase().contains("lib")
    {
        name.truncate(open);
    }
    name
}

/// Whether a book matches what's typed into the library's filter: every word of `query` is
/// somewhere in its title, author or file name, ignoring case.
pub fn matches(query: &str, book: &[&str]) -> bool {
    let book = book.join(" ").to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| book.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "slipstream-reader-library-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_epub_in_the_folders_is_found_and_nothing_else() {
        let dir = scratch("scan");
        std::fs::create_dir_all(dir.join("fiction/deeper")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        for file in [
            "The Zebra.epub",
            "fiction/Apple.EPUB",
            "fiction/deeper/Mango.epub",
            "fiction/notes.txt",
            "song.mp3",
            ".hidden/Secret.epub",
        ] {
            std::fs::write(dir.join(file), b"").unwrap();
        }
        let names: Vec<String> = scan(&dir)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Apple.EPUB", "Mango.epub", "The Zebra.epub"]);
    }

    #[test]
    fn a_missing_folder_is_an_empty_shelf() {
        assert!(scan(Path::new("/nowhere/at/all")).is_empty());
    }

    #[test]
    fn a_book_that_wont_open_is_shelved_by_its_file_name() {
        let dir = scratch("broken");
        let path = dir.join("Broken Money (Lyn Alden) (Z-Library).epub");
        std::fs::write(&path, b"not a zip").unwrap();
        let shelved = describe(&path);
        assert_eq!(shelved.title, "Broken Money (Lyn Alden)");
        assert_eq!((shelved.author, shelved.cover), (None, None));
    }

    #[test]
    fn every_typed_word_is_somewhere_in_the_title_author_or_file() {
        let book = ["The Colour of Magic", "Terry Pratchett", "Discworld 01"];
        assert!(matches("", &book), "nothing typed shows every book");
        assert!(matches("colour", &book));
        assert!(matches("PRATCH magic", &book), "any case, any order");
        assert!(matches("discworld", &book), "the file name too");
        assert!(!matches("magic feist", &book));
    }

    #[test]
    fn download_sites_are_left_off_a_name() {
        let name = |text: &str| tidy_name(Path::new(text));
        assert_eq!(
            name("Riftwar (Raymond E. Feist) (Z-Library).epub"),
            "Riftwar (Raymond E. Feist)"
        );
        assert_eq!(
            name("Discworld (Terry Pratchett) (z-library.sk, 1lib.sk, z-lib.sk).epub"),
            "Discworld (Terry Pratchett)"
        );
        assert_eq!(name("Plain.epub"), "Plain");
    }
}
