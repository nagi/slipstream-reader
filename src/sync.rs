//! Talking to the Kobo sync server (Kobo-Sync, books.bitcointop.club): the books it delivers to
//! the Kobo, each as a KEPUB, with its highlights, notes, bookmarks and reading position. The
//! reader opens the same KEPUB the Kobo has, so their positions mean the same place.
//!
//! The server and its token are in `~/.config/slipstream/reader-sync.json`:
//! `{"server": "https://books.bitcointop.club", "token": "…"}`. Without that file there is no
//! sync, and the reader works as before. Everything here runs on the main loop without blocking.

use std::path::{Path, PathBuf};

use gtk4::glib;
use serde::{Deserialize, Serialize};
use webkit6::soup::{self, prelude::*};

use crate::annotations::Annotation;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: String,
    pub token: String,
}

pub fn config_path() -> PathBuf {
    crate::dirs::config_dir().join("reader-sync.json")
}

impl Config {
    pub fn read() -> Option<Config> {
        let text = std::fs::read_to_string(config_path()).ok()?;
        let config: Config = serde_json::from_str(&text).ok()?;
        (!config.server.is_empty() && !config.token.is_empty()).then_some(config)
    }
}

/// A book the server delivers.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerBook {
    pub id: String,
    /// Its path in the library (`~/Dropbox/Books`), which is how a book here is matched to it.
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Annotations {
    pub annotations: Vec<Annotation>,
}

/// Where the reader is, as the Kobo says it: the chapter file and the KEPUB sentence span.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Location {
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "Type", default = "kobo_span")]
    pub kind: String,
    #[serde(rename = "Value")]
    pub value: String,
}

/// Percentages go to the Kobo as whole numbers, as it sends them.
fn whole<S: serde::Serializer>(value: &f64, out: S) -> Result<S::Ok, S::Error> {
    out.serialize_i64(value.round() as i64)
}

fn kobo_span() -> String {
    "KoboSpan".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bookmark {
    #[serde(rename = "LastModified")]
    pub last_modified: String,
    #[serde(rename = "ProgressPercent", default, serialize_with = "whole")]
    pub progress_percent: f64,
    #[serde(
        rename = "ContentSourceProgressPercent",
        default,
        serialize_with = "whole"
    )]
    pub chapter_percent: f64,
    #[serde(rename = "Location", default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadingState {
    #[serde(rename = "CurrentBookmark")]
    pub bookmark: Option<Bookmark>,
}

pub struct Client {
    config: Config,
    session: soup::Session,
}

impl Client {
    pub fn new(config: Config) -> Client {
        let session = soup::Session::new();
        session.set_timeout(30);
        session.set_user_agent("slipstream-reader");
        Client { config, session }
    }

    fn url(&self, path: &str) -> String {
        let token: String = glib::Uri::escape_string(&self.config.token, None, false).into();
        format!(
            "{}/reader/{token}/{path}",
            self.config.server.trim_end_matches('/')
        )
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<glib::Bytes, String> {
        let message = soup::Message::new(method, &self.url(path)).map_err(|err| err.to_string())?;
        if let Some(body) = body {
            message.set_request_body_from_bytes(
                Some("application/json"),
                Some(&glib::Bytes::from_owned(body)),
            );
        }
        let bytes = self
            .session
            .send_and_read_future(&message, glib::Priority::DEFAULT)
            .await
            .map_err(|err| err.to_string())?;
        match message.status_code() {
            200..=299 => Ok(bytes),
            code => Err(format!("{method} {path}: the server answered {code}")),
        }
    }

    async fn json<T: for<'a> Deserialize<'a>>(&self, path: &str) -> Result<T, String> {
        let bytes = self.send("GET", path, None).await?;
        serde_json::from_slice(&bytes).map_err(|err| format!("{path}: {err}"))
    }

    pub async fn books(&self) -> Result<Vec<ServerBook>, String> {
        self.json("books").await
    }

    /// The book's KEPUB, written to `to` (beside it first, then moved over it).
    pub async fn download(&self, id: &str, to: &Path) -> Result<(), String> {
        let bytes = self.send("GET", &format!("books/{id}/file"), None).await?;
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        let new = to.with_extension("epub.new");
        std::fs::write(&new, &bytes).map_err(|err| err.to_string())?;
        std::fs::rename(&new, to).map_err(|err| err.to_string())
    }

    pub async fn annotations(&self, id: &str) -> Result<Annotations, String> {
        self.json(&format!("books/{id}/annotations")).await
    }

    pub async fn put_annotation(&self, book: &str, note: &Annotation) -> Result<(), String> {
        let body = serde_json::to_vec(note).map_err(|err| err.to_string())?;
        let id: String = glib::Uri::escape_string(&note.id, None, false).into();
        self.send("PUT", &format!("books/{book}/annotations/{id}"), Some(body))
            .await
            .map(|_| ())
    }

    pub async fn delete_annotation(
        &self,
        book: &str,
        note_id: &str,
        at: &str,
    ) -> Result<(), String> {
        let id: String = glib::Uri::escape_string(note_id, None, false).into();
        let at: String = glib::Uri::escape_string(at, None, false).into();
        self.send(
            "DELETE",
            &format!("books/{book}/annotations/{id}?at={at}"),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn position(&self, id: &str) -> Result<Option<Bookmark>, String> {
        let state: Option<ReadingState> = self.json(&format!("books/{id}/position")).await?;
        Ok(state
            .and_then(|state| state.bookmark)
            .filter(|b| b.location.is_some()))
    }

    pub async fn put_position(&self, id: &str, bookmark: &Bookmark) -> Result<(), String> {
        let body = serde_json::to_vec(&serde_json::json!({ "CurrentBookmark": bookmark }))
            .map_err(|err| err.to_string())?;
        self.send("PUT", &format!("books/{id}/position"), Some(body))
            .await
            .map(|_| ())
    }
}

/// Where a book's KEPUB is kept here.
pub fn kepub_path(id: &str) -> PathBuf {
    crate::dirs::cache_dir()
        .join("reader")
        .join(format!("{id}.kepub.epub"))
}

/// The server's book for a file in the library, by its path under the library folder.
pub fn match_book<'a>(
    books: &'a [ServerBook],
    library: &Path,
    file: &Path,
) -> Option<&'a ServerBook> {
    let rel = file
        .strip_prefix(library)
        .ok()?
        .to_string_lossy()
        .into_owned();
    books.iter().find(|book| book.path == rel)
}

/// The time now, as the Kobo writes it.
pub fn now() -> String {
    glib::DateTime::now_utc()
        .and_then(|t| t.format("%Y-%m-%dT%H:%M:%SZ"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_book_is_matched_by_its_path_in_the_library() {
        let books = vec![
            ServerBook {
                id: "a".into(),
                path: "For Kobo/fiction/A.epub".into(),
            },
            ServerBook {
                id: "b".into(),
                path: "For Kobo/fiction/B.epub".into(),
            },
        ];
        let library = Path::new("/home/me/Dropbox/Books");
        let found = match_book(
            &books,
            library,
            Path::new("/home/me/Dropbox/Books/For Kobo/fiction/B.epub"),
        );
        assert_eq!(found.map(|b| b.id.as_str()), Some("b"));
        assert!(match_book(&books, library, Path::new("/elsewhere/B.epub")).is_none());
    }

    #[test]
    fn a_position_reads_as_the_server_writes_it() {
        let state: ReadingState = serde_json::from_str(
            r#"{"CurrentBookmark": {"LastModified": "2026-10-20T15:48:04Z", "ProgressPercent": 60,
                "ContentSourceProgressPercent": 7,
                "Location": {"Source": "OEBPS/part50.xhtml", "Type": "KoboSpan", "Value": "kobo.9.3"}}}"#,
        )
        .unwrap();
        let mark = state.bookmark.unwrap();
        assert_eq!(mark.location.as_ref().unwrap().value, "kobo.9.3");
        assert_eq!(mark.progress_percent, 60.0);
        let sent = serde_json::to_string(&mark).unwrap();
        assert!(sent.contains(r#""ProgressPercent":60,"#), "{sent}");
    }
}
