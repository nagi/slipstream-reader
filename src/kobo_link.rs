//! The reader's side of the Kobo sync: a book that the sync server delivers to the Kobo is read
//! here from the same KEPUB, with the Kobo's highlights, notes and bookmarks drawn on it, new ones
//! made here sent back, and the reading position kept in step.
//!
//! - Select text, then Ctrl+H (or right-click, Highlight) highlights it; Ctrl+Shift+H (Add note…)
//!   highlights it with a note. A click on a highlight shows its note, to change or remove.
//! - Ctrl+D bookmarks the top of the page, or takes the bookmark there away.
//! - Ctrl+I's Notes list has them all; one clicked goes there.
//! - The place you're reading is sent a few seconds after you stop; when the Kobo's is newer, the
//!   book opens there instead.
//!
//! Changes wait in `~/.local/state/slipstream/reader-outbox.json` until the server takes them, so
//! nothing is lost while it can't be reached.

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use serde::{Deserialize, Serialize};
use webkit6::prelude::*;

use crate::{
    READER, Reader, WORLD,
    annotations::{self, Annotation, Selection, Span},
    sync::{self, Bookmark, Client, ServerBook},
};

/// How long after the last move the place is sent.
const PUSH_AFTER: Duration = Duration::from_secs(4);
/// How often the open book's annotations are looked at again, for changes made on the Kobo.
const REFRESH_EVERY: u32 = 120;

/// A change the server hasn't taken yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Change {
    Put {
        book: String,
        note: Box<Annotation>,
    },
    Delete {
        book: String,
        id: String,
        at: String,
    },
}

fn outbox_path() -> PathBuf {
    crate::dirs::state_dir().join("reader-outbox.json")
}

fn read_outbox() -> Vec<Change> {
    std::fs::read_to_string(outbox_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_outbox(changes: &[Change]) {
    let path = outbox_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let new = path.with_extension("json.new");
    if let Ok(text) = serde_json::to_string_pretty(changes)
        && std::fs::write(&new, text).is_ok()
    {
        let _ = std::fs::rename(&new, &path);
    }
}

/// A fresh id for an annotation, in the Kobo's form.
fn new_id() -> String {
    glib::uuid_string_random().to_string()
}

/// What the note popover is showing.
enum Editing {
    Existing(String),
    New(Selection),
}

pub struct Link {
    client: Rc<Client>,
    /// The server's books, once fetched.
    books: RefCell<Option<Vec<ServerBook>>>,
    /// The open book's id on the server, when it was opened as the Kobo's KEPUB.
    pub book: RefCell<Option<String>>,
    /// Its annotations, deleted ones as tombstones.
    notes: RefCell<Vec<Annotation>>,
    outbox: RefCell<Vec<Change>>,
    flushing: Cell<bool>,
    /// The page was moved by you since the place was last sent.
    pub moved: Cell<bool>,
    pushing: RefCell<Option<glib::SourceId>>,
    /// The notes list's rows, by annotation id.
    rows: RefCell<Vec<String>>,
    editing: RefCell<Option<Editing>>,
    reachable: Cell<bool>,
}

/// The widgets the sync adds: the Notes list in the left panel, a line in the right one, and the
/// popover a note is shown and written in.
pub struct NotesUi {
    pub list: gtk::ListBox,
    pub scroll: gtk::ScrolledWindow,
    pub empty: gtk::Label,
    pub status: gtk::Label,
    pub popover: gtk::Popover,
    quote: gtk::Label,
    text: gtk::TextView,
    save: gtk::Button,
    remove: gtk::Button,
}

impl NotesUi {
    pub fn build(view: &webkit6::WebView) -> NotesUi {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .css_classes(["chapters", "notes"])
            .build();
        let empty = gtk::Label::builder()
            .label("No highlights, notes or bookmarks yet.\nSelect some text and press Ctrl+H.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .margin_top(32)
            .margin_start(24)
            .margin_end(24)
            .css_classes(["caption"])
            .build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&empty);
        column.append(&list);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&column)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption"])
            .visible(false)
            .build();

        let quote = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(40)
            .lines(4)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["quote"])
            .build();
        let text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .height_request(80)
            .width_request(320)
            .css_classes(["note-text"])
            .build();
        let save = gtk::Button::builder()
            .label("Save")
            .css_classes(["follow"])
            .build();
        let remove = gtk::Button::builder()
            .label("Remove")
            .css_classes(["follow"])
            .build();
        let buttons = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::End)
            .build();
        buttons.append(&remove);
        buttons.append(&save);
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(10)
            .margin_end(10)
            .build();
        body.append(&quote);
        body.append(&text);
        body.append(&buttons);
        let popover = gtk::Popover::builder().child(&body).autohide(true).build();
        popover.set_parent(view);
        NotesUi {
            list,
            scroll,
            empty,
            status,
            popover,
            quote,
            text,
            save,
            remove,
        }
    }
}

impl Link {
    pub fn new() -> Option<Rc<Link>> {
        let config = sync::Config::read()?;
        Some(Rc::new(Link {
            client: Rc::new(Client::new(config)),
            books: RefCell::new(None),
            book: RefCell::new(None),
            notes: RefCell::new(Vec::new()),
            outbox: RefCell::new(read_outbox()),
            flushing: Cell::new(false),
            moved: Cell::new(false),
            pushing: RefCell::new(None),
            rows: RefCell::new(Vec::new()),
            editing: RefCell::new(None),
            reachable: Cell::new(true),
        }))
    }
}

/// The open chapter's file as the Kobo names it: `OEBPS/part2.xhtml`, decoded.
fn kobo_chapter(path: &str) -> String {
    glib::Uri::unescape_string(path, None::<&str>)
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
        .trim_start_matches('/')
        .to_string()
}

/// The numbers in a span id (`kobo.12.3` → (12, 3)), for putting notes in reading order.
fn span_order(id: Option<String>) -> (u64, u64) {
    let id = id.unwrap_or_default();
    let mut parts = id
        .trim_start_matches("kobo.")
        .split('.')
        .map(|n| n.parse::<u64>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

impl Reader {
    // Opening.

    /// What to open for the book at `path`: its KEPUB when the server has it and it's here
    /// already, else the file itself (and the KEPUB is fetched meanwhile).
    pub(crate) fn file_to_open(&self, path: &Path) -> PathBuf {
        let Some(link) = &self.link else {
            return path.to_path_buf();
        };
        *link.book.borrow_mut() = None;
        let matched = link.books.borrow().as_ref().and_then(|books| {
            sync::match_book(books, &crate::library::dir(), path).map(|b| b.id.clone())
        });
        match matched {
            Some(id) if sync::kepub_path(&id).exists() => {
                let kepub = sync::kepub_path(&id);
                *link.book.borrow_mut() = Some(id);
                kepub
            }
            _ => path.to_path_buf(),
        }
    }

    /// Once a book is open: its annotations and the Kobo's place when it's the KEPUB, or else
    /// the KEPUB fetched, and the book opened again from it.
    pub(crate) fn after_open(self: &Rc<Self>, path: &Path) {
        let Some(link) = self.link.clone() else {
            return;
        };
        link.notes.borrow_mut().clear();
        self.fill_notes_list();
        let path = path.to_path_buf();
        let weak = Rc::downgrade(self);
        let opened_kepub = link.book.borrow().clone();
        glib::spawn_future_local(async move {
            if let Some(id) = opened_kepub {
                if let Some(reader) = weak.upgrade() {
                    reader.refresh_notes().await;
                    reader.follow_kobo_place(&id, &path).await;
                }
                return;
            }
            // Not the KEPUB yet: find the book on the server and fetch it.
            if link.books.borrow().is_none() {
                match link.client.books().await {
                    Ok(books) => *link.books.borrow_mut() = Some(books),
                    Err(err) => {
                        link.reachable.set(false);
                        if let Some(reader) = weak.upgrade() {
                            reader.show_sync_status();
                        }
                        eprintln!("slipstream-reader: kobo sync: {err}");
                        return;
                    }
                }
            }
            let id = link.books.borrow().as_ref().and_then(|books| {
                sync::match_book(books, &crate::library::dir(), &path).map(|b| b.id.clone())
            });
            let Some(id) = id else {
                return;
            };
            let kepub = sync::kepub_path(&id);
            if !kepub.exists()
                && let Err(err) = link.client.download(&id, &kepub).await
            {
                eprintln!("slipstream-reader: kobo sync: fetching the book: {err}");
                return;
            }
            // Still on that book: open it again, now from the KEPUB, at the same place.
            if let Some(reader) = weak.upgrade()
                && reader.path.borrow().as_deref() == Some(path.as_path())
            {
                reader.open(&path);
            }
        });
    }

    /// When the Kobo (or another reader) was somewhere later than this one was, go there.
    async fn follow_kobo_place(self: &Rc<Self>, id: &str, path: &Path) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let Ok(Some(theirs)) = link.client.position(id).await else {
            return;
        };
        if link.moved.get() || self.path.borrow().as_deref() != Some(path) {
            return; // You've moved since opening, or gone to another book: yours stands.
        }
        let ours = self
            .memory
            .borrow()
            .place(path)
            .and_then(|place| place.read_at);
        let later = ours.is_none_or(|ours| sync_time(&theirs.last_modified) > sync_time(&ours));
        if later && let Some(location) = theirs.location {
            self.go_to_span(&location.source, &location.value);
            // Taken as where you are, so it isn't sent straight back as newer.
            self.read_at.replace(Some(theirs.last_modified));
        }
    }

    // Notes.

    pub(crate) async fn refresh_notes(self: &Rc<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let Some(id) = link.book.borrow().clone() else {
            return;
        };
        self.flush_outbox().await;
        match link.client.annotations(&id).await {
            Ok(got) => {
                link.reachable.set(true);
                if link.book.borrow().as_deref() == Some(id.as_str()) {
                    // Changes still waiting to go keep the place they have here.
                    let waiting: Vec<Change> = link.outbox.borrow().clone();
                    let mut notes = got.annotations;
                    for change in waiting {
                        match change {
                            Change::Put { book, note } if book == id => {
                                notes.retain(|n| n.id != note.id);
                                notes.push(*note);
                            }
                            Change::Delete { book, id: gone, .. } if book == id => {
                                notes.retain(|n| n.id != gone);
                            }
                            _ => {}
                        }
                    }
                    *link.notes.borrow_mut() = notes;
                }
            }
            Err(err) => {
                link.reachable.set(false);
                eprintln!("slipstream-reader: kobo sync: {err}");
            }
        }
        self.redraw_marks();
        self.fill_notes_list();
        self.show_sync_status();
    }

    /// Sends what's waiting, oldest first, stopping at the first the server won't take.
    async fn flush_outbox(self: &Rc<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        if link.flushing.replace(true) {
            return;
        }
        loop {
            let Some(next) = link.outbox.borrow().first().cloned() else {
                break;
            };
            let sent = match &next {
                Change::Put { book, note } => link.client.put_annotation(book, note).await,
                Change::Delete { book, id, at } => {
                    link.client.delete_annotation(book, id, at).await
                }
            };
            if let Err(err) = sent {
                link.reachable.set(false);
                eprintln!("slipstream-reader: kobo sync: {err}");
                break;
            }
            link.reachable.set(true);
            let mut outbox = link.outbox.borrow_mut();
            outbox.remove(0);
            write_outbox(&outbox);
        }
        link.flushing.set(false);
        self.show_sync_status();
    }

    fn queue(self: &Rc<Self>, change: Change) {
        let Some(link) = self.link.clone() else {
            return;
        };
        {
            let mut outbox = link.outbox.borrow_mut();
            outbox.push(change);
            write_outbox(&outbox);
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if let Some(reader) = weak.upgrade() {
                reader.flush_outbox().await;
            }
        });
    }

    fn show_sync_status(&self) {
        let Some(link) = &self.link else {
            return;
        };
        let waiting = link.outbox.borrow().len();
        let text = match (link.book.borrow().is_some(), link.reachable.get(), waiting) {
            (_, false, 0) => "Kobo sync: can’t reach the server".to_string(),
            (_, false, n) => format!("Kobo sync: can’t reach the server; {n} change(s) waiting"),
            (true, true, 0) => "Kobo sync: up to date".to_string(),
            (true, true, n) => format!("Kobo sync: sending {n} change(s)"),
            (false, true, _) => "Kobo sync: this book isn’t on the Kobo".to_string(),
        };
        self.notes_ui.status.set_label(&text);
        self.notes_ui.status.set_visible(true);
    }

    /// The open chapter's highlights, drawn on the page.
    pub(crate) fn redraw_marks(&self) {
        let Some(link) = &self.link else {
            return;
        };
        let Some(book) = self.book.borrow().clone() else {
            return;
        };
        let Some(chapter) = book.chapters.get(self.chapter.get()) else {
            return;
        };
        let marks = annotations::marks_for(&link.notes.borrow(), chapter);
        let script = format!("window.slipstreamNotes && window.slipstreamNotes.draw({marks})");
        self.view
            .evaluate_javascript(&script, Some(WORLD), None, gio::Cancellable::NONE, |_| {});
    }

    /// Where the open chapter is in the book, for a new annotation.
    fn chapter_span(&self) -> Option<Span> {
        let book = self.book.borrow().clone()?;
        let index = self.chapter.get();
        let path = book.chapters.get(index)?;
        let title = book
            .entry_for_chapter(index)
            .and_then(|entry| book.contents.get(entry))
            .map(|entry| entry.label.clone())
            .unwrap_or_default();
        Some(Span {
            chapter: kobo_chapter(path),
            chapter_title: title,
            chapter_progress: index as f64 / book.chapters.len().max(1) as f64,
            ..Span::default()
        })
    }

    fn add_note(self: &Rc<Self>, note: Annotation) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let Some(book) = link.book.borrow().clone() else {
            return;
        };
        {
            let mut notes = link.notes.borrow_mut();
            notes.retain(|n| n.id != note.id);
            notes.push(note.clone());
        }
        self.queue(Change::Put {
            book,
            note: Box::new(note),
        });
        self.redraw_marks();
        self.fill_notes_list();
    }

    fn remove_note(self: &Rc<Self>, id: &str) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let Some(book) = link.book.borrow().clone() else {
            return;
        };
        link.notes.borrow_mut().retain(|n| n.id != id);
        self.queue(Change::Delete {
            book,
            id: id.to_string(),
            at: sync::now(),
        });
        self.redraw_marks();
        self.fill_notes_list();
    }

    /// Ctrl+H, or Highlight from the menu: the selection highlighted. With a note, the note is
    /// asked for first.
    pub(crate) fn highlight_selection(self: &Rc<Self>, with_note: bool) {
        if self
            .link
            .as_ref()
            .is_none_or(|link| link.book.borrow().is_none())
        {
            return;
        }
        let weak = Rc::downgrade(self);
        self.view.evaluate_javascript(
            "JSON.stringify(window.slipstreamNotes ? window.slipstreamNotes.selection() : null)",
            Some(WORLD),
            None,
            gio::Cancellable::NONE,
            move |result| {
                let Some(reader) = weak.upgrade() else { return };
                let selection = result
                    .ok()
                    .and_then(|value| value.to_str().parse::<String>().ok())
                    .and_then(|json| serde_json::from_str::<Option<Selection>>(&json).ok())
                    .flatten();
                let Some(selection) = selection else { return };
                if with_note {
                    reader.edit_note(Editing::New(selection), None);
                } else if reader.already_highlighted(&selection) {
                    reader.clear_selection();
                } else if let Some(chapter) = reader.chapter_span() {
                    reader.add_note(Annotation::new_highlight(
                        new_id(),
                        &selection,
                        chapter,
                        None,
                        sync::now(),
                    ));
                    reader.clear_selection();
                }
            },
        );
    }

    /// Whether exactly this text is highlighted already.
    fn already_highlighted(&self, selection: &Selection) -> bool {
        self.link.as_ref().is_some_and(|link| {
            link.notes.borrow().iter().any(|n| {
                let span = &n.location.span;
                !n.deleted
                    && !n.is_bookmark()
                    && span.start_path == selection.start
                    && span.start_char == selection.start_char
                    && span.end_path == selection.end
                    && span.end_char == selection.end_char
            })
        })
    }

    fn clear_selection(&self) {
        self.view.evaluate_javascript(
            "window.slipstreamNotes && window.slipstreamNotes.clear()",
            Some(WORLD),
            None,
            gio::Cancellable::NONE,
            |_| {},
        );
    }

    /// Ctrl+D: a bookmark at the top of the page, or the one there taken away.
    pub(crate) fn toggle_bookmark(self: &Rc<Self>) {
        if self
            .link
            .as_ref()
            .is_none_or(|link| link.book.borrow().is_none())
        {
            return;
        }
        let weak = Rc::downgrade(self);
        self.view.evaluate_javascript(
            "JSON.stringify(window.slipstreamNotes ? window.slipstreamNotes.topSpan() : null)",
            Some(WORLD),
            None,
            gio::Cancellable::NONE,
            move |result| {
                #[derive(Deserialize)]
                struct Top {
                    id: String,
                    text: String,
                }
                let Some(reader) = weak.upgrade() else { return };
                let Some(top) = result
                    .ok()
                    .and_then(|value| serde_json::from_str::<Option<Top>>(&value.to_str()).ok())
                    .flatten()
                else {
                    return;
                };
                let Some(chapter) = reader.chapter_span() else {
                    return;
                };
                let existing = reader.link.as_ref().and_then(|link| {
                    link.notes
                        .borrow()
                        .iter()
                        .find(|n| {
                            n.is_bookmark()
                                && n.in_chapter(&chapter.chapter)
                                && n.span_id().as_deref() == Some(top.id.as_str())
                        })
                        .map(|n| n.id.clone())
                });
                match existing {
                    Some(id) => reader.remove_note(&id),
                    None => reader.add_note(Annotation::new_bookmark(
                        new_id(),
                        &top.id,
                        top.text.trim(),
                        chapter,
                        sync::now(),
                    )),
                }
            },
        );
    }

    /// The note popover, for a highlight clicked (`Existing`) or a selection to be noted (`New`),
    /// pointing at `at` on the page, or its middle.
    fn edit_note(self: &Rc<Self>, editing: Editing, at: Option<(f64, f64)>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let ui = &self.notes_ui;
        let (quote, note, existing) = match &editing {
            Editing::Existing(id) => {
                let notes = link.notes.borrow();
                let Some(found) = notes.iter().find(|n| &n.id == id) else {
                    return;
                };
                (
                    found.text.clone(),
                    found.note.clone().unwrap_or_default(),
                    true,
                )
            }
            Editing::New(selection) => (selection.text.trim().to_string(), String::new(), false),
        };
        ui.quote.set_label(&format!("“{quote}”"));
        ui.text.buffer().set_text(&note);
        ui.remove.set_visible(existing);
        ui.save
            .set_label(if existing { "Save" } else { "Add note" });
        *link.editing.borrow_mut() = Some(editing);
        let (x, y) = at.unwrap_or_else(|| {
            (
                self.view.width() as f64 / 2.0,
                self.view.height() as f64 / 3.0,
            )
        });
        ui.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        ui.popover.popup();
        ui.text.grab_focus();
    }

    fn save_edited_note(self: &Rc<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let buffer = self.notes_ui.text.buffer();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .trim()
            .to_string();
        let note = (!text.is_empty()).then_some(text);
        let editing = link.editing.borrow_mut().take();
        match editing {
            Some(Editing::Existing(id)) => {
                let found = link.notes.borrow().iter().find(|n| n.id == id).cloned();
                if let Some(mut changed) = found {
                    changed.kind = if note.is_some() { "note" } else { "highlight" }.into();
                    changed.note = note;
                    changed.modified = sync::now();
                    self.add_note(changed);
                }
            }
            Some(Editing::New(selection)) => {
                if let Some(chapter) = self.chapter_span() {
                    self.add_note(Annotation::new_highlight(
                        new_id(),
                        &selection,
                        chapter,
                        note,
                        sync::now(),
                    ));
                    self.clear_selection();
                }
            }
            None => {}
        }
        self.notes_ui.popover.popdown();
        self.view.grab_focus();
    }

    fn remove_edited_note(self: &Rc<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        if let Some(Editing::Existing(id)) = link.editing.borrow_mut().take() {
            self.remove_note(&id);
        }
        self.notes_ui.popover.popdown();
        self.view.grab_focus();
    }

    /// The Notes list: bookmarks, highlights and notes, in reading order.
    fn fill_notes_list(&self) {
        let Some(link) = &self.link else {
            return;
        };
        let ui = &self.notes_ui;
        ui.list.remove_all();
        let book = self.book.borrow().clone();
        let chapter_of = |note: &Annotation| {
            book.as_ref()
                .and_then(|book| book.chapter_of(&format!("/{}", note.location.span.chapter)))
                .unwrap_or(usize::MAX)
        };
        let mut notes: Vec<Annotation> = link
            .notes
            .borrow()
            .iter()
            .filter(|n| !n.deleted)
            .cloned()
            .collect();
        notes.sort_by_key(|n| {
            (
                chapter_of(n),
                span_order(n.span_id()),
                n.location.span.start_char,
            )
        });
        let mut rows = Vec::new();
        for note in &notes {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .margin_top(10)
                .margin_bottom(10)
                .margin_start(20)
                .margin_end(20)
                .build();
            let kind = match note.kind.as_str() {
                "dogear" => "Bookmark",
                "note" => "Note",
                _ => "Highlight",
            };
            // The chapter as this book's contents name it, else as the Kobo did.
            let chapter = book
                .as_ref()
                .and_then(|book| {
                    book.entry_for_chapter(chapter_of(note))
                        .and_then(|entry| book.contents.get(entry))
                        .map(|entry| entry.label.clone())
                })
                .unwrap_or_else(|| note.location.span.chapter_title.clone());
            row.append(
                &gtk::Label::builder()
                    .label(if chapter.is_empty() {
                        kind.to_string()
                    } else {
                        format!("{kind} · {chapter}")
                    })
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["caption"])
                    .build(),
            );
            let quote = if note.is_bookmark() {
                note.context().unwrap_or_default().to_string()
            } else {
                note.text.clone()
            };
            if !quote.is_empty() {
                row.append(
                    &gtk::Label::builder()
                        .label(&quote)
                        .xalign(0.0)
                        .wrap(true)
                        .wrap_mode(gtk::pango::WrapMode::WordChar)
                        .lines(3)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .css_classes(["quote"])
                        .build(),
                );
            }
            if let Some(text) = note.note.as_deref().filter(|t| !t.is_empty()) {
                row.append(
                    &gtk::Label::builder()
                        .label(text)
                        .xalign(0.0)
                        .wrap(true)
                        .wrap_mode(gtk::pango::WrapMode::WordChar)
                        .css_classes(["note-said"])
                        .build(),
                );
            }
            ui.list.append(&row);
            rows.push(note.id.clone());
        }
        ui.empty.set_visible(rows.is_empty());
        *link.rows.borrow_mut() = rows;
    }

    /// A row of the Notes list clicked: there, in the book.
    fn go_to_note_row(self: &Rc<Self>, index: usize) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let Some(id) = link.rows.borrow().get(index).cloned() else {
            return;
        };
        let note = link.notes.borrow().iter().find(|n| n.id == id).cloned();
        if let Some(note) = note
            && let Some(span) = note.span_id()
        {
            self.go_to_span(&note.location.span.chapter, &span);
        }
    }

    /// The chapter file `chapter` (`OEBPS/part2.xhtml`), at the span `span`.
    pub(crate) fn go_to_span(&self, chapter: &str, span: &str) {
        let Some(book) = self.book.borrow().clone() else {
            return;
        };
        let Some(index) = book.chapter_of(&format!("/{}", chapter.trim_start_matches('/'))) else {
            return;
        };
        if index == self.chapter.get()
            && self.pending.get().is_none()
            && self.pending_span.borrow().is_none()
        {
            self.scroll_to_span(span);
        } else {
            *self.pending_span.borrow_mut() = Some(span.to_string());
            self.load_chapter(index, None);
        }
    }

    pub(crate) fn scroll_to_span(&self, span: &str) {
        let id = serde_json::to_string(span).unwrap_or_default();
        self.view.evaluate_javascript(
            &format!("window.slipstreamNotes && window.slipstreamNotes.scrollToSpan({id})"),
            Some(WORLD),
            None,
            gio::Cancellable::NONE,
            |_| {},
        );
    }

    // The reading place.

    /// You moved through the book: the place is sent once you've stopped for a moment.
    pub(crate) fn place_moved(&self) {
        let Some(link) = &self.link else {
            return;
        };
        link.moved.set(true);
        self.read_at.replace(Some(sync::now()));
        if link.book.borrow().is_none() {
            return;
        }
        if let Some(waiting) = link.pushing.borrow_mut().take() {
            waiting.remove();
        }
        let id = glib::timeout_add_local_once(PUSH_AFTER, || {
            if let Some(reader) = READER.with(|reader| reader.borrow().clone()) {
                if let Some(link) = &reader.link {
                    link.pushing.borrow_mut().take();
                }
                reader.push_place();
            }
        });
        *link.pushing.borrow_mut() = Some(id);
    }

    /// Where you are, as the Kobo says it, sent to the server.
    pub(crate) fn push_place(self: &Rc<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let Some(id) = link.book.borrow().clone() else {
            return;
        };
        let Some(book) = self.book.borrow().clone() else {
            return;
        };
        let index = self.chapter.get();
        let Some(chapter) = book.chapters.get(index).map(|path| kobo_chapter(path)) else {
            return;
        };
        let fraction = self.fraction.get();
        let progress = 100.0 * (index as f64 + fraction) / book.chapters.len().max(1) as f64;
        let stamp = self.read_at.borrow().clone().unwrap_or_else(sync::now);
        let client = link.client.clone();
        self.view.evaluate_javascript(
            "JSON.stringify(window.slipstreamNotes ? window.slipstreamNotes.topSpan() : null)",
            Some(WORLD),
            None,
            gio::Cancellable::NONE,
            move |result| {
                #[derive(Deserialize)]
                struct Top {
                    id: String,
                }
                let Some(top) = result
                    .ok()
                    .and_then(|value| serde_json::from_str::<Option<Top>>(&value.to_str()).ok())
                    .flatten()
                else {
                    return;
                };
                let mark = Bookmark {
                    last_modified: stamp,
                    progress_percent: progress.round(),
                    chapter_percent: (fraction * 100.0).round(),
                    location: Some(sync::Location {
                        source: chapter,
                        kind: "KoboSpan".into(),
                        value: top.id,
                    }),
                };
                glib::spawn_future_local(async move {
                    if let Err(err) = client.put_position(&id, &mark).await {
                        eprintln!("slipstream-reader: kobo sync: sending the place: {err}");
                    }
                });
            },
        );
    }

    // Wiring.

    pub(crate) fn connect_link(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        // A click on a highlight, from the page.
        let w = weak.clone();
        self.content
            .connect_script_message_received(Some("annotation"), move |_, value| {
                #[derive(Deserialize)]
                struct Clicked {
                    id: String,
                    x: f64,
                    y: f64,
                }
                let Some(reader) = w.upgrade() else { return };
                let Some(clicked) = value
                    .to_json(0)
                    .and_then(|json| serde_json::from_str::<Clicked>(&json).ok())
                else {
                    return;
                };
                let zoom = reader.view.zoom_level();
                reader.edit_note(
                    Editing::Existing(clicked.id),
                    Some((clicked.x * zoom, clicked.y * zoom)),
                );
            });

        let w = weak.clone();
        self.notes_ui.save.connect_clicked(move |_| {
            if let Some(reader) = w.upgrade() {
                reader.save_edited_note();
            }
        });
        let w = weak.clone();
        self.notes_ui.remove.connect_clicked(move |_| {
            if let Some(reader) = w.upgrade() {
                reader.remove_edited_note();
            }
        });
        let w = weak.clone();
        self.notes_ui.list.connect_row_activated(move |_, row| {
            if let Some(reader) = w.upgrade() {
                reader.go_to_note_row(row.index() as usize);
            }
        });
        // Ctrl+Enter saves the note being written.
        let keys = gtk::EventControllerKey::new();
        let w = weak.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && state.contains(gdk::ModifierType::CONTROL_MASK)
                && let Some(reader) = w.upgrade()
            {
                reader.save_edited_note();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.notes_ui.text.add_controller(keys);

        // Highlight and Add note… on a selection, Bookmark this page anywhere.
        let w = weak.clone();
        self.view.connect_context_menu(move |_, menu, hit| {
            let Some(reader) = w.upgrade() else {
                return false;
            };
            if reader
                .link
                .as_ref()
                .is_none_or(|link| link.book.borrow().is_none())
            {
                return false;
            }
            let item = |label: &str, run: fn(&Rc<Reader>)| {
                let action = gio::SimpleAction::new(&format!("slipstream-{}", label.len()), None);
                action.connect_activate(move |_, _| {
                    if let Some(reader) = READER.with(|reader| reader.borrow().clone()) {
                        run(&reader);
                    }
                });
                webkit6::ContextMenuItem::from_gaction(&action, label, None)
            };
            menu.append(&webkit6::ContextMenuItem::new_separator());
            if hit.context_is_selection() {
                menu.append(&item("Highlight", |reader| {
                    reader.highlight_selection(false)
                }));
                menu.append(&item("Add note…", |reader| {
                    reader.highlight_selection(true)
                }));
            }
            menu.append(&item("Bookmark this page", |reader| {
                reader.toggle_bookmark()
            }));
            false
        });

        // A move by you, not by the reader putting you somewhere: the wheel or a touchpad.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let w = weak.clone();
        wheel.connect_scroll(move |_, _, _| {
            if let Some(reader) = w.upgrade() {
                reader.user_moving.set(true);
            }
            glib::Propagation::Proceed
        });
        self.view.add_controller(wheel);

        // Changes made on the Kobo, now and then.
        glib::timeout_add_seconds_local(REFRESH_EVERY, || {
            if let Some(reader) = READER.with(|reader| reader.borrow().clone())
                && reader
                    .link
                    .as_ref()
                    .is_some_and(|link| link.book.borrow().is_some())
            {
                glib::spawn_future_local(async move { reader.refresh_notes().await });
            }
            glib::ControlFlow::Continue
        });

        // The server's books, fetched once at the start, so a book opened later is found at once.
        if let Some(link) = self.link.clone() {
            let w = weak.clone();
            glib::spawn_future_local(async move {
                match link.client.books().await {
                    Ok(books) => *link.books.borrow_mut() = Some(books),
                    Err(err) => {
                        link.reachable.set(false);
                        eprintln!("slipstream-reader: kobo sync: {err}");
                    }
                }
                if let Some(reader) = w.upgrade() {
                    reader.show_sync_status();
                    reader.flush_outbox().await;
                }
            });
        }
    }
}

/// A Kobo time, for comparing: `2026-10-20T16:33:17.2904345Z` and `…17Z` alike.
fn sync_time(text: &str) -> String {
    let text = text.trim().trim_end_matches('Z');
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    format!("{whole}.{:0<7}", &fraction[..fraction.len().min(7)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kobo_times_compare_whatever_their_precision() {
        assert!(sync_time("2026-10-20T16:33:17.2904345Z") > sync_time("2026-10-20T16:33:17Z"));
        assert!(sync_time("2026-10-20T16:33:18Z") > sync_time("2026-10-20T16:33:17.9Z"));
    }

    #[test]
    fn chapters_are_named_as_the_kobo_does() {
        assert_eq!(kobo_chapter("/OEBPS/part%201.xhtml"), "OEBPS/part 1.xhtml");
    }

    #[test]
    fn spans_sort_in_reading_order() {
        let mut ids = vec!["kobo.10.1", "kobo.2.3", "kobo.2.10"];
        ids.sort_by_key(|id| span_order(Some(id.to_string())));
        assert_eq!(ids, ["kobo.2.3", "kobo.2.10", "kobo.10.1"]);
    }

    #[test]
    fn changes_waiting_survive_a_restart() {
        let change = Change::Delete {
            book: "b".into(),
            id: "n".into(),
            at: "2026-10-21T09:00:00Z".into(),
        };
        let text = serde_json::to_string(&vec![change]).unwrap();
        assert!(text.contains(r#""op":"delete""#));
        let back: Vec<Change> = serde_json::from_str(&text).unwrap();
        assert!(matches!(&back[0], Change::Delete { id, .. } if id == "n"));
    }
}
