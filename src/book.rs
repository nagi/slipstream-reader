//! An EPUB opened for reading: its title, its chapters in reading order, its table of contents,
//! and the files inside it, served to the page by path.
//!
//! Paths are the book's own absolute ones, as the EPUB's manifest gives them (`/EPUB/c1.xhtml`),
//! still percent-encoded. The page asks for them as `epub://book/EPUB/c1.xhtml`.

use std::path::Path;

use rbook::Epub;

/// The scheme the page's files are served under, and the host every book is at.
pub const SCHEME: &str = "epub";
const ORIGIN: &str = "epub://book";

pub struct Book {
    epub: Epub,
    pub title: String,
    /// Each chapter's path, in reading order.
    pub chapters: Vec<String>,
    /// The table of contents, flattened: what the chapter list shows.
    pub contents: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub label: String,
    /// 0 for a top-level entry, 1 for one under it, and so on.
    pub depth: usize,
    pub path: String,
    pub fragment: Option<String>,
    /// The chapter the entry is in, when it is one of the book's chapters.
    pub chapter: Option<usize>,
}

impl Book {
    pub fn open(path: &Path) -> Result<Book, String> {
        let epub = Epub::open(path).map_err(|err| err.to_string())?;
        let title = epub
            .metadata()
            .title()
            .map(|title| title.value().trim().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let chapters: Vec<String> = epub
            .spine()
            .iter()
            .filter_map(|item| item.manifest_entry())
            .map(|entry| entry.href().path().as_str().to_string())
            .collect();
        if chapters.is_empty() {
            return Err("it has no chapters".to_string());
        }
        let mut contents: Vec<Entry> = epub
            .toc()
            .contents()
            .map(|root| {
                root.flatten()
                    .filter_map(|item| {
                        let href = item.href()?;
                        let path = href.path().as_str().to_string();
                        Some(Entry {
                            label: tidy(item.label()),
                            depth: item.depth().saturating_sub(1),
                            chapter: chapters.iter().position(|c| *c == path),
                            fragment: href.fragment().map(str::to_string),
                            path,
                        })
                    })
                    .filter(|entry| !entry.label.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // A book without a table of contents still has its chapters to go to.
        if contents.is_empty() {
            contents = chapters
                .iter()
                .enumerate()
                .map(|(index, path)| Entry {
                    label: format!("Section {}", index + 1),
                    depth: 0,
                    path: path.clone(),
                    fragment: None,
                    chapter: Some(index),
                })
                .collect();
        }
        Ok(Book {
            epub,
            title,
            chapters,
            contents,
        })
    }

    /// The file at `path`, and its media type, as the page asked for it.
    pub fn read(&self, path: &str) -> Option<(Vec<u8>, String)> {
        let decoded = percent_decoded(path);
        let bytes = self
            .epub
            .read_resource_bytes(path)
            .or_else(|_| self.epub.read_resource_bytes(decoded.as_str()))
            .ok()?;
        let media = self
            .epub
            .manifest()
            .by_href(path)
            .or_else(|| self.epub.manifest().by_href(&decoded))
            .map(|entry| entry.media_type().to_string())
            .unwrap_or_else(|| guess_media_type(path).to_string());
        Some((bytes, media))
    }

    /// Which chapter `path` is, if it is one.
    pub fn chapter_of(&self, path: &str) -> Option<usize> {
        let decoded = percent_decoded(path);
        self.chapters
            .iter()
            .position(|chapter| chapter == path || percent_decoded(chapter) == decoded)
    }

    /// The table of contents' entry for being in `chapter`: the last one at or before it, so a
    /// part split over several files shows its heading all the way through.
    pub fn entry_for_chapter(&self, chapter: usize) -> Option<usize> {
        self.contents
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.chapter.is_some_and(|at| at <= chapter))
            .max_by_key(|(index, entry)| (entry.chapter, std::cmp::Reverse(*index)))
            .map(|(index, _)| index)
    }
}

/// The address the page loads `path` from.
pub fn uri(path: &str, fragment: Option<&str>) -> String {
    let slash = if path.starts_with('/') { "" } else { "/" };
    match fragment {
        Some(fragment) => format!("{ORIGIN}{slash}{path}#{fragment}"),
        None => format!("{ORIGIN}{slash}{path}"),
    }
}

/// The book's path in an address the page loaded, without its fragment or query.
pub fn path_of(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix(ORIGIN)?;
    let end = rest.find(['#', '?']).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Whitespace in a label run together, as a table of contents often breaks its lines.
fn tidy(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(digits) = bytes.get(i + 1..i + 3)
            && let Ok(digits) = std::str::from_utf8(digits)
            && let Ok(byte) = u8::from_str_radix(digits, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

fn guess_media_type(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match extension.as_str() {
        "xhtml" | "xht" => "application/xhtml+xml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A small unpacked EPUB 3: two parts, the first in two files, and a picture.
    fn sample() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "slipstream-reader-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("mimetype", "application/epub+zip");
        write(
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        );
        write(
            "OEBPS/content.opf",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">test</dc:identifier>
    <dc:title>  A Short Book </dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="one" href="text/one.xhtml" media-type="application/xhtml+xml"/>
    <item id="one-b" href="text/one%20b.xhtml" media-type="application/xhtml+xml"/>
    <item id="two" href="text/two.xhtml" media-type="application/xhtml+xml"/>
    <item id="pic" href="pic.png" media-type="image/png"/>
  </manifest>
  <spine><itemref idref="one"/><itemref idref="one-b"/><itemref idref="two"/></spine>
</package>"#,
        );
        write(
            "OEBPS/nav.xhtml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<body><nav epub:type="toc"><ol>
  <li><a href="text/one.xhtml">Part
      One</a>
    <ol><li><a href="text/one.xhtml#start">Beginning</a></li></ol>
  </li>
  <li><a href="text/two.xhtml">Part Two</a></li>
</ol></nav></body></html>"#,
        );
        let page = |title: &str| {
            format!(
                r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p id="start">{title}</p></body></html>"#
            )
        };
        write("OEBPS/text/one.xhtml", &page("one"));
        write("OEBPS/text/one b.xhtml", &page("one b"));
        write("OEBPS/text/two.xhtml", &page("two"));
        write("OEBPS/pic.png", "not really a png");
        dir
    }

    #[test]
    fn reads_the_title_and_the_chapters_in_order() {
        let book = Book::open(&sample()).unwrap();
        assert_eq!(book.title, "A Short Book");
        assert_eq!(book.chapters.len(), 3);
        assert!(book.chapters[0].ends_with("/text/one.xhtml"));
        assert!(book.chapters[2].ends_with("/text/two.xhtml"));
    }

    #[test]
    fn the_contents_keep_their_depth_and_anchors() {
        let book = Book::open(&sample()).unwrap();
        let labels: Vec<(&str, usize)> = book
            .contents
            .iter()
            .map(|entry| (entry.label.as_str(), entry.depth))
            .collect();
        assert_eq!(
            labels,
            [("Part One", 0), ("Beginning", 1), ("Part Two", 0)]
        );
        assert_eq!(book.contents[1].fragment.as_deref(), Some("start"));
        assert_eq!(book.contents[2].chapter, Some(2));
    }

    #[test]
    fn a_chapter_without_an_entry_shows_the_one_before_it() {
        let book = Book::open(&sample()).unwrap();
        // The second file of part one has no entry of its own.
        assert_eq!(book.entry_for_chapter(1), Some(0));
        assert_eq!(book.entry_for_chapter(0), Some(0));
        assert_eq!(book.entry_for_chapter(2), Some(2));
    }

    #[test]
    fn files_are_found_with_their_media_types() {
        let book = Book::open(&sample()).unwrap();
        let (bytes, media) = book.read(&book.chapters[0]).unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("one"));
        assert_eq!(media, "application/xhtml+xml");
        let picture = book.chapters[0].replace("text/one.xhtml", "pic.png");
        assert_eq!(book.read(&picture).unwrap().1, "image/png");
        assert!(book.read("/OEBPS/missing.css").is_none());
    }

    #[test]
    fn an_encoded_name_is_found_either_way() {
        let book = Book::open(&sample()).unwrap();
        let encoded = book.chapters[1].clone();
        assert!(encoded.contains("%20"), "{encoded}");
        assert!(book.read(&encoded).is_some());
        assert_eq!(book.chapter_of(&encoded.replace("%20", " ")), Some(1));
    }

    #[test]
    fn addresses_go_there_and_back() {
        let uri = uri("/OEBPS/text/one.xhtml", Some("start"));
        assert_eq!(uri, "epub://book/OEBPS/text/one.xhtml#start");
        assert_eq!(path_of(&uri), Some("/OEBPS/text/one.xhtml"));
        assert_eq!(path_of("https://example.com/a"), None);
    }

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(percent_decoded("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decoded("100%"), "100%");
        assert_eq!(percent_decoded("%zz"), "%zz");
    }
}
