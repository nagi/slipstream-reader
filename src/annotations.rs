//! Highlights, notes and bookmarks, in the shape the Kobo sends them (see Kobo-Sync's PROTOCOL.md).
//!
//! In a KEPUB every sentence is a `<span class="koboSpan" id="kobo.N.M">`. A highlight runs from
//! `startChar` in its start span (`startPath`, a CSS selector like `span#kobo\.3\.1`) to `endChar`
//! in its end span. A bookmark (`dogear`) names a span as `OEBPS/x.xhtml#kobo.11.5`. The page
//! script here finds those spans, draws highlights over the text without changing it, and turns a
//! selection back into the same shape.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Span {
    #[serde(rename = "chapterFilename", default)]
    pub chapter: String,
    #[serde(rename = "chapterTitle", default)]
    pub chapter_title: String,
    #[serde(rename = "chapterProgress", default)]
    pub chapter_progress: f64,
    #[serde(rename = "startPath", default)]
    pub start_path: String,
    #[serde(rename = "startChar", default)]
    pub start_char: i64,
    #[serde(rename = "endPath", default)]
    pub end_path: String,
    #[serde(rename = "endChar", default)]
    pub end_char: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Location {
    #[serde(default)]
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Annotation {
    pub id: String,
    /// `highlight`, `note` (a highlight with a note) or `dogear` (a bookmark).
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "highlightedText", default)]
    pub text: String,
    #[serde(rename = "noteText", default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "clientLastModifiedUtc", default)]
    pub modified: String,
    #[serde(default)]
    pub location: Location,
    /// Only from the server: a tombstone for one deleted, here or on the Kobo.
    #[serde(default, skip_serializing)]
    pub deleted: bool,
    #[serde(rename = "modified", default, skip_serializing)]
    pub server_modified: Option<String>,
    /// Everything else the Kobo sent (`context`, `highlightColor`, …), kept as it was.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl Annotation {
    pub fn is_bookmark(&self) -> bool {
        self.kind == "dogear"
    }

    pub fn new_highlight(
        id: String,
        selection: &Selection,
        chapter: Span,
        note: Option<String>,
        now: String,
    ) -> Annotation {
        let mut rest = Map::new();
        rest.insert("highlightColor".into(), Value::String("#A0A0A0".into()));
        rest.insert("context".into(), Value::Null);
        rest.insert("attachments".into(), Value::Object(Map::new()));
        Annotation {
            id,
            kind: if note.is_some() { "note" } else { "highlight" }.into(),
            text: selection.text.trim().to_string(),
            note,
            modified: now,
            location: Location {
                span: Span {
                    start_path: selection.start.clone(),
                    start_char: selection.start_char,
                    end_path: selection.end.clone(),
                    end_char: selection.end_char,
                    ..chapter
                },
            },
            deleted: false,
            server_modified: None,
            rest,
        }
    }

    pub fn new_bookmark(
        id: String,
        span_id: &str,
        context: &str,
        chapter: Span,
        now: String,
    ) -> Annotation {
        let path = format!("{}#{span_id}", chapter.chapter);
        let mut rest = Map::new();
        rest.insert(
            "context".into(),
            Value::String(context.chars().take(150).collect()),
        );
        rest.insert("attachments".into(), Value::Object(Map::new()));
        Annotation {
            id,
            kind: "dogear".into(),
            text: String::new(),
            note: None,
            modified: now,
            location: Location {
                span: Span {
                    start_path: path.clone(),
                    end_path: path,
                    ..chapter
                },
            },
            deleted: false,
            server_modified: None,
            rest,
        }
    }

    /// Whether it is in the chapter whose path in the book is `chapter` (`/OEBPS/part2.xhtml`,
    /// perhaps percent-encoded).
    pub fn in_chapter(&self, chapter: &str) -> bool {
        same_file(&self.location.span.chapter, chapter)
    }

    /// The span a bookmark is at (`kobo.11.5`), or a highlight starts in.
    pub fn span_id(&self) -> Option<String> {
        let path = &self.location.span.start_path;
        if let Some((_, id)) = path.split_once('#') {
            // `OEBPS/x.xhtml#kobo.11.5`, or `span#kobo\.3\.1`.
            return Some(id.replace('\\', "")).filter(|id| !id.is_empty());
        }
        None
    }

    /// The text around a bookmark, as the Kobo saved it.
    pub fn context(&self) -> Option<&str> {
        self.rest.get("context").and_then(Value::as_str)
    }
}

/// Two paths of the same file in the book, however each was written.
pub fn same_file(a: &str, b: &str) -> bool {
    let clean = |p: &str| {
        let decoded = glib_unescape(p);
        decoded.trim_start_matches('/').to_string()
    };
    !a.is_empty() && clean(a) == clean(b)
}

fn glib_unescape(text: &str) -> String {
    gtk4::glib::Uri::unescape_string(text, None::<&str>)
        .map(|s| s.to_string())
        .unwrap_or_else(|| text.to_string())
}

/// What the page says is selected, in the Kobo's terms.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Selection {
    pub start: String,
    #[serde(rename = "startChar")]
    pub start_char: i64,
    pub end: String,
    #[serde(rename = "endChar")]
    pub end_char: i64,
    pub text: String,
}

/// What the page is given to draw: each highlight's spans and characters, and whether it has a note.
pub fn marks_for(notes: &[Annotation], chapter: &str) -> Value {
    Value::Array(
        notes
            .iter()
            .filter(|n| !n.deleted && !n.is_bookmark() && n.in_chapter(chapter))
            .map(|n| {
                let span = &n.location.span;
                serde_json::json!({
                    "id": n.id, "start": span.start_path, "startChar": span.start_char,
                    "end": span.end_path, "endChar": span.end_char,
                    "note": n.note.as_deref().is_some_and(|t| !t.is_empty()),
                })
            })
            .collect(),
    )
}

/// The reader's page script for annotations, in its own script world. `window.slipstreamNotes`:
/// - `draw(marks)` draws highlights (and notes, underlined) with the CSS Custom Highlight API;
/// - `selection()` is the selection as `{start, startChar, end, endChar, text}`, or null;
/// - `topSpan()` is the first sentence span showing at the top: `{id, text}`;
/// - `scrollToSpan(id)` brings a span to the top;
/// - `clear()` takes the selection away.
///
/// A click on a highlight is posted to the `annotation` handler as `{id, x, y}`.
pub const SCRIPT: &str = r#"(() => {
  let ranges = new Map();
  const textNodes = (root) => {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const out = [];
    while (walker.nextNode()) out.push(walker.currentNode);
    return out;
  };
  // The text node and offset `ch` characters into an element's text.
  const point = (element, ch) => {
    let left = Math.max(0, ch);
    const nodes = textNodes(element);
    for (const node of nodes) {
      if (left <= node.data.length) return [node, left];
      left -= node.data.length;
    }
    const last = nodes[nodes.length - 1];
    return last ? [last, last.data.length] : null;
  };
  // How many characters into `span`'s text the point (node, offset) is.
  const offsetIn = (span, node, offset) => {
    const range = document.createRange();
    range.setStart(span, 0);
    try { range.setEnd(node, offset); } catch (e) { return 0; }
    return range.toString().length;
  };
  const find = (selector) => {
    try { return document.querySelector(selector); } catch (e) { return null; }
  };
  window.slipstreamNotes = {
    draw(marks) {
      ranges = new Map();
      const plain = [], noted = [];
      for (const mark of marks) {
        const start = find(mark.start), end = find(mark.end);
        if (!start || !end) continue;
        const a = point(start, mark.startChar), b = point(end, mark.endChar);
        if (!a || !b) continue;
        const range = document.createRange();
        try { range.setStart(a[0], a[1]); range.setEnd(b[0], b[1]); } catch (e) { continue; }
        ranges.set(mark.id, range);
        (mark.note ? noted : plain).push(range);
      }
      if (window.CSS && CSS.highlights) {
        CSS.highlights.set("slipstream-mark", new Highlight(...plain));
        CSS.highlights.set("slipstream-note", new Highlight(...noted));
      }
      return ranges.size;
    },
    selection() {
      const selection = getSelection();
      if (!selection || selection.isCollapsed || !selection.rangeCount) return null;
      const range = selection.getRangeAt(0);
      const spans = [...document.querySelectorAll("span.koboSpan")].filter(s => range.intersectsNode(s));
      if (!spans.length) return null;
      const first = spans[0], last = spans[spans.length - 1];
      let startChar = first.contains(range.startContainer) ? offsetIn(first, range.startContainer, range.startOffset) : 0;
      let endChar = last.contains(range.endContainer) ? offsetIn(last, range.endContainer, range.endOffset) : last.textContent.length;
      // Whole words, as the Kobo makes them.
      const word = /[\p{L}\p{N}'’-]/u;
      const head = first.textContent, tail = last.textContent;
      while (startChar > 0 && word.test(head[startChar - 1]) && word.test(head[startChar] || "")) startChar--;
      while (endChar < tail.length && word.test(tail[endChar]) && word.test(tail[endChar - 1] || "")) endChar++;
      const a = point(first, startChar), b = point(last, endChar);
      const snapped = document.createRange();
      try { snapped.setStart(a[0], a[1]); snapped.setEnd(b[0], b[1]); } catch (e) { return null; }
      return {
        start: "span#" + CSS.escape(first.id), startChar,
        end: "span#" + CSS.escape(last.id), endChar,
        text: snapped.toString(),
      };
    },
    topSpan() {
      for (const span of document.querySelectorAll("span.koboSpan")) {
        const rect = span.getBoundingClientRect();
        if (rect.bottom > 4 && rect.height > 0) return { id: span.id, text: span.textContent };
      }
      return null;
    },
    scrollToSpan(id) {
      const span = document.getElementById(id);
      if (!span) return false;
      span.scrollIntoView({ block: "start" });
      return true;
    },
    clear() { getSelection().removeAllRanges(); },
  };
  addEventListener("click", (event) => {
    const selection = getSelection();
    if (selection && !selection.isCollapsed) return;
    const caret = document.caretRangeFromPoint(event.clientX, event.clientY);
    if (!caret) return;
    for (const [id, range] of ranges) {
      if (range.isPointInRange(caret.startContainer, caret.startOffset)) {
        window.webkit.messageHandlers.annotation.postMessage({ id, x: event.clientX, y: event.clientY });
        event.preventDefault();
        return;
      }
    }
  });
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    const FROM_KOBO: &str = r##"{
      "clientLastModifiedUtc": "2026-10-20T16:35:34Z", "highlightColor": "#A0A0A0",
      "highlightedText": "Sometimes", "id": "7854ec42", "noteText": "Test", "type": "note",
      "location": {"span": {"chapterFilename": "OEBPS/part2.xhtml", "chapterProgress": 0.11,
        "chapterTitle": "PROLOGUE", "endChar": 9, "endPath": "span#kobo\\.3\\.2",
        "startChar": 0, "startPath": "span#kobo\\.3\\.2"}},
      "deleted": false, "modified": "2026-10-20T16:35:51Z"}"##;

    #[test]
    fn what_the_kobo_sent_goes_back_as_it_came() {
        let note: Annotation = serde_json::from_str(FROM_KOBO).unwrap();
        assert_eq!(note.note.as_deref(), Some("Test"));
        assert_eq!(note.location.span.end_char, 9);
        assert!(note.in_chapter("/OEBPS/part2.xhtml"));
        let back = serde_json::to_value(&note).unwrap();
        assert_eq!(back["highlightColor"], "#A0A0A0");
        assert!(back.get("deleted").is_none() && back.get("modified").is_none());
        assert_eq!(back["location"]["span"]["startPath"], "span#kobo\\.3\\.2");
    }

    #[test]
    fn spans_are_found_in_either_kind_of_path() {
        let mut note: Annotation = serde_json::from_str(FROM_KOBO).unwrap();
        assert_eq!(note.span_id().as_deref(), Some("kobo.3.2"));
        note.location.span.start_path = "OEBPS/part2.xhtml#kobo.11.5".into();
        assert_eq!(note.span_id().as_deref(), Some("kobo.11.5"));
    }

    #[test]
    fn only_this_chapters_highlights_are_drawn() {
        let note: Annotation = serde_json::from_str(FROM_KOBO).unwrap();
        let mut gone = note.clone();
        gone.id = "gone".into();
        gone.deleted = true;
        let marks = marks_for(&[note.clone(), gone], "/OEBPS/part2.xhtml");
        assert_eq!(marks.as_array().unwrap().len(), 1);
        assert_eq!(marks[0]["note"], true);
        assert!(
            marks_for(&[note], "/OEBPS/part3.xhtml")
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_new_highlight_has_the_kobos_shape() {
        let selection = Selection {
            start: "span#kobo\\.4\\.1".into(),
            start_char: 0,
            end: "span#kobo\\.4\\.1".into(),
            end_char: 36,
            text: " A man has to have a purpose in life. ".into(),
        };
        let chapter = Span {
            chapter: "OEBPS/part2.xhtml".into(),
            chapter_title: "PROLOGUE".into(),
            ..Span::default()
        };
        let note = Annotation::new_highlight(
            "n1".into(),
            &selection,
            chapter,
            None,
            "2026-10-21T09:00:00Z".into(),
        );
        let value = serde_json::to_value(&note).unwrap();
        assert_eq!(value["type"], "highlight");
        assert_eq!(
            value["highlightedText"],
            "A man has to have a purpose in life."
        );
        assert_eq!(
            value["location"]["span"]["chapterFilename"],
            "OEBPS/part2.xhtml"
        );
        assert!(value.get("noteText").is_none());
    }
}
