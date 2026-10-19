//! Slipstream's reading mode (Super+R): an EPUB book, full screen, with nothing else on it.
//!
//! The page is drawn by WebKit, the engine Foliate uses, from the book's own files, served from
//! inside the book under `epub://book/`. The book's scripts never run and nothing is fetched from
//! the network: a link out of the book opens in the browser.
//!
//! - Ctrl+= and Ctrl+-, Ctrl and the wheel, or a pinch make the text bigger or smaller; Ctrl+0
//!   puts it back.
//! - Super+I brings out the chapters on the left and the two colour sliders on the right, and
//!   puts them away again.
//! - ← and → go a chapter back or on; reading past the end of one goes on into the next.
//! - Ctrl+O opens another book.
//!
//! It is always full screen. The page and the text are in the desktop's theme, and change with
//! it; a slider moved takes that colour off the theme until "Follow theme" puts it back.
//!
//! `slipstream-reader BOOK.epub` opens that book, in the window already open if there is one.
//! With no book it opens the last one, where you left it.

mod book;
mod memory;
mod palette;
mod theme;

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use book::Book;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use memory::{Memory, Place, ZOOM_MAX, ZOOM_MIN};
use webkit6::{prelude::*, soup};

const APP_ID: &str = "io.github.peterwalker78.SlipstreamReader";
/// The script world the reader's own scripts run in, apart from the page's.
const WORLD: &str = "slipstream-reader";
/// Only the book's own files, its pictures and fonts, and its own inline styles. No scripts.
const POLICY: &str = "default-src epub:; img-src epub: data:; style-src epub: 'unsafe-inline'; \
                      font-src epub: data:; media-src epub:; script-src 'none'; object-src 'none'";
/// How much each zoom step multiplies the text by.
const ZOOM_STEP: f64 = 1.1;
/// How long the pointer stays on screen once it stops over the page.
const POINTER_REST: Duration = Duration::from_secs(2);

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_startup(|app| {
        app.set_accels_for_action("window.close", &["<Control>q", "<Control>w"]);
    });
    app.connect_activate(|app| {
        let reader = Reader::for_app(app);
        reader.window.present();
        if reader.book.borrow().is_none() {
            reader.open_last_or_ask();
        }
    });
    app.connect_open(|app, files, _| {
        let reader = Reader::for_app(app);
        reader.window.present();
        if let Some(path) = files.first().and_then(|file| file.path()) {
            reader.open(&path);
        }
    });
    app.run()
}

thread_local! {
    static READER: RefCell<Option<Rc<Reader>>> = const { RefCell::new(None) };
}

struct Reader {
    window: gtk::ApplicationWindow,
    view: webkit6::WebView,
    content: webkit6::UserContentManager,
    chapters_panel: gtk::Revealer,
    chapters_scroll: gtk::ScrolledWindow,
    colours_panel: gtk::Revealer,
    list: gtk::ListBox,
    zoom_label: gtk::Label,
    page_scale: gtk::Scale,
    text_scale: gtk::Scale,
    follow_theme: gtk::Button,
    chrome: gtk::CssProvider,
    /// The desktop's colours, as last read.
    theme: Cell<theme::Theme>,
    /// The sliders are being moved to match the theme, not by hand.
    syncing: Cell<bool>,
    theme_pending: Cell<bool>,
    watching: RefCell<Vec<gio::FileMonitor>>,
    /// Shared with the scheme handler, which serves the page from it.
    book: Rc<RefCell<Option<Rc<Book>>>>,
    path: RefCell<Option<PathBuf>>,
    chapter: Cell<usize>,
    /// Where the page is scrolled to, as the page last said.
    fraction: Cell<f64>,
    at_top: Cell<bool>,
    at_bottom: Cell<bool>,
    /// Where to scroll to once the chapter being loaded has finished.
    pending: Cell<Option<f64>>,
    memory: RefCell<Memory>,
    saving: RefCell<Option<glib::SourceId>>,
    pointer_rest: RefCell<Option<glib::SourceId>>,
    zoom_start: Cell<f64>,
}

impl Reader {
    /// The reader's one window, built the first time it's asked for.
    fn for_app(app: &gtk::Application) -> Rc<Reader> {
        if let Some(reader) = READER.with(|reader| reader.borrow().clone()) {
            return reader;
        }
        let reader = Reader::build(app);
        READER.with(|slot| *slot.borrow_mut() = Some(reader.clone()));
        reader
    }

    fn build(app: &gtk::Application) -> Rc<Reader> {
        let memory = Memory::read(&memory::path());
        let book: Rc<RefCell<Option<Rc<Book>>>> = Rc::default();

        let context = webkit6::WebContext::new();
        let serving = book.clone();
        context.register_uri_scheme(book::SCHEME, move |request| serve(&serving, request));

        let content = webkit6::UserContentManager::new();
        content.register_script_message_handler("reader", Some(WORLD));
        content.add_script(&webkit6::UserScript::for_world(
            SCROLL_SCRIPT,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::End,
            WORLD,
            &[],
            &[],
        ));

        let settings = webkit6::Settings::new();
        // Scripts the reader runs itself still work; the book's own never do.
        settings.set_enable_javascript_markup(false);
        settings.set_enable_developer_extras(false);
        settings.set_enable_back_forward_navigation_gestures(false);
        settings.set_enable_smooth_scrolling(true);

        let view = webkit6::WebView::builder()
            .web_context(&context)
            .network_session(&webkit6::NetworkSession::new_ephemeral())
            .user_content_manager(&content)
            .settings(&settings)
            .zoom_level(memory.zoom)
            .hexpand(true)
            .vexpand(true)
            .build();

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(true)
            .css_classes(["chapters"])
            .build();
        let chapters_scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(320)
            .vexpand(true)
            .build();
        let chapters_panel = gtk::Revealer::builder()
            .child(&chapters_scroll)
            .transition_type(gtk::RevealerTransitionType::SlideRight)
            .reveal_child(memory.panels)
            .css_classes(["panel", "left"])
            .build();

        let page_scale = colour_scale("page");
        let text_scale = colour_scale("text");
        let zoom_label = gtk::Label::builder().css_classes(["zoom"]).build();
        let follow_theme = gtk::Button::builder()
            .label("Follow theme")
            .css_classes(["follow"])
            .halign(gtk::Align::Center)
            .build();
        let sliders = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(18)
            .halign(gtk::Align::Center)
            .vexpand(true)
            .build();
        sliders.append(&labelled(&page_scale, "Page"));
        sliders.append(&labelled(&text_scale, "Text"));
        let colours = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(32)
            .margin_bottom(24)
            .margin_start(20)
            .margin_end(20)
            .build();
        colours.append(&sliders);
        colours.append(&follow_theme);
        colours.append(&zoom_label);
        let colours_panel = gtk::Revealer::builder()
            .child(&colours)
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .reveal_child(memory.panels)
            .css_classes(["panel", "right"])
            .build();

        let layout = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        layout.append(&chapters_panel);
        layout.append(&view);
        layout.append(&colours_panel);

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Reading")
            .default_width(1200)
            .default_height(900)
            .child(&layout)
            .build();
        window.fullscreen();

        let chrome = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &chrome,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let reader = Rc::new(Reader {
            window,
            view,
            content,
            chapters_panel,
            chapters_scroll,
            colours_panel,
            list,
            zoom_label,
            page_scale,
            text_scale,
            follow_theme,
            chrome,
            theme: Cell::new(theme::current()),
            syncing: Cell::new(false),
            theme_pending: Cell::new(false),
            watching: RefCell::new(Vec::new()),
            book,
            path: RefCell::new(None),
            chapter: Cell::new(0),
            fraction: Cell::new(0.0),
            at_top: Cell::new(true),
            at_bottom: Cell::new(false),
            pending: Cell::new(None),
            memory: RefCell::new(memory),
            saving: RefCell::new(None),
            pointer_rest: RefCell::new(None),
            zoom_start: Cell::new(1.0),
        });
        reader.connect();
        reader.sync_sliders();
        reader.recolour();
        let weak = Rc::downgrade(&reader);
        *reader.watching.borrow_mut() = theme::watch(move || {
            if let Some(reader) = weak.upgrade() {
                reader.theme_changed_soon();
            }
        });
        reader.show_zoom();
        reader
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);

        let w = weak.clone();
        self.page_scale.connect_value_changed(move |scale| {
            if let Some(reader) = w.upgrade()
                && !reader.syncing.get()
            {
                reader.memory.borrow_mut().page_choice = Some(scale.value());
                reader.chosen();
            }
        });
        let w = weak.clone();
        self.text_scale.connect_value_changed(move |scale| {
            if let Some(reader) = w.upgrade()
                && !reader.syncing.get()
            {
                reader.memory.borrow_mut().text_choice = Some(scale.value());
                reader.chosen();
            }
        });
        let w = weak.clone();
        self.follow_theme.connect_clicked(move |_| {
            if let Some(reader) = w.upgrade() {
                {
                    let mut memory = reader.memory.borrow_mut();
                    memory.page_choice = None;
                    memory.text_choice = None;
                }
                reader.sync_sliders();
                reader.chosen();
            }
        });

        let w = weak.clone();
        self.list.connect_row_activated(move |_, row| {
            if let Some(reader) = w.upgrade() {
                reader.go_to_entry(row.index() as usize);
            }
        });

        // Where the page is scrolled to, from the reader's own script in the page.
        let w = weak.clone();
        self.content
            .connect_script_message_received(Some("reader"), move |_, value| {
                let said = value
                    .to_json(0)
                    .and_then(|json| serde_json::from_str::<(f64, bool, bool)>(&json).ok());
                if let (Some(reader), Some((fraction, top, bottom))) = (w.upgrade(), said) {
                    reader.scrolled(fraction, top, bottom);
                }
            });

        let w = weak.clone();
        self.view.connect_load_changed(move |view, event| {
            if let Some(reader) = w.upgrade() {
                reader.load_changed(view, event);
            }
        });

        let w = weak.clone();
        self.view.connect_decide_policy(move |_, decision, kind| {
            w.upgrade()
                .is_some_and(|reader| reader.decide(decision, kind))
        });

        // Keys are looked at before the page gets them.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = weak.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            if w.upgrade().is_some_and(|reader| reader.key(key, state)) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.window.add_controller(keys);

        // Ctrl and the wheel zooms, as it does in a browser.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = weak.clone();
        wheel.connect_scroll(move |wheel, _, dy| {
            if !wheel
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                return glib::Propagation::Proceed;
            }
            if let Some(reader) = w.upgrade() {
                if dy < 0.0 {
                    reader.zoom_by(ZOOM_STEP);
                } else if dy > 0.0 {
                    reader.zoom_by(1.0 / ZOOM_STEP);
                }
            }
            glib::Propagation::Stop
        });
        self.view.add_controller(wheel);

        // A pinch on a touchpad or a touchscreen.
        let pinch = gtk::GestureZoom::new();
        let w = weak.clone();
        pinch.connect_begin(move |_, _| {
            if let Some(reader) = w.upgrade() {
                reader.zoom_start.set(reader.view.zoom_level());
            }
        });
        let w = weak.clone();
        pinch.connect_scale_changed(move |_, scale| {
            if let Some(reader) = w.upgrade() {
                reader.zoom_to(reader.zoom_start.get() * scale);
            }
        });
        self.view.add_controller(pinch);

        // The pointer keeps out of the way of the text once it stops.
        let motion = gtk::EventControllerMotion::new();
        let w = weak.clone();
        motion.connect_motion(move |_, _, _| {
            if let Some(reader) = w.upgrade() {
                reader.pointer_moved();
            }
        });
        self.view.add_controller(motion);

        // A book's `title` attributes would otherwise pop up over the text.
        self.view.connect_tooltip_text_notify(|view| {
            if view.tooltip_text().is_some() {
                view.set_tooltip_text(None);
            }
        });

        let w = weak.clone();
        self.window.connect_close_request(move |_| {
            if let Some(reader) = w.upgrade() {
                reader.save_now();
            }
            glib::Propagation::Proceed
        });
    }

    // Opening books.

    /// The last book, where it was left, or a file chooser when there isn't one.
    fn open_last_or_ask(self: &Rc<Self>) {
        let last = self.memory.borrow().last.clone();
        match last.filter(|path| path.exists()) {
            Some(path) => self.open(&path),
            None => {
                self.show_note("No book open", "Ctrl+O opens one.");
                self.ask_for_book();
            }
        }
    }

    fn ask_for_book(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("EPUB books"));
        filter.add_mime_type("application/epub+zip");
        filter.add_suffix("epub");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Open a book")
            .filters(&filters)
            .default_filter(&filter)
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |chosen| {
            if let (Some(reader), Ok(file)) = (weak.upgrade(), chosen)
                && let Some(path) = file.path()
            {
                reader.open(&path);
            }
        });
    }

    fn open(self: &Rc<Self>, path: &Path) {
        self.save_now();
        let opened = match Book::open(path) {
            Ok(book) => Rc::new(book),
            Err(err) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                self.show_note(&format!("Couldn’t open {name}"), &err);
                return;
            }
        };
        self.window.set_title(Some(&opened.title));
        self.fill_chapters(&opened);
        *self.book.borrow_mut() = Some(opened.clone());
        *self.path.borrow_mut() = Some(path.to_path_buf());
        let place = {
            let mut memory = self.memory.borrow_mut();
            memory.last = Some(path.to_path_buf());
            memory.place(path).unwrap_or_default()
        };
        let chapter = place.chapter.min(opened.chapters.len() - 1);
        self.load_chapter(chapter, Some(place.fraction));
        self.save_soon();
    }

    fn fill_chapters(&self, book: &Book) {
        self.list.remove_all();
        for entry in &book.contents {
            let label = gtk::Label::builder()
                .label(&entry.label)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .margin_start(20 + 18 * entry.depth.min(4) as i32)
                .margin_end(20)
                .margin_top(7)
                .margin_bottom(7)
                .build();
            if entry.depth == 0 {
                label.add_css_class("top");
            }
            self.list.append(&label);
        }
    }

    // Moving through the book.

    /// Loads chapter `index`, and once it's in, scrolls to `fraction` of the way down it.
    fn load_chapter(&self, index: usize, fraction: Option<f64>) {
        let Some(book) = self.book.borrow().clone() else {
            return;
        };
        let Some(path) = book.chapters.get(index) else {
            return;
        };
        self.chapter.set(index);
        self.pending.set(fraction);
        self.view.load_uri(&book::uri(path, None));
        self.mark_chapter();
    }

    fn go_to_entry(&self, index: usize) {
        let Some(book) = self.book.borrow().clone() else {
            return;
        };
        let Some(entry) = book.contents.get(index) else {
            return;
        };
        if let Some(chapter) = entry.chapter {
            self.chapter.set(chapter);
        }
        self.pending.set(None);
        self.view
            .load_uri(&book::uri(&entry.path, entry.fragment.as_deref()));
    }

    /// The next chapter from its top, or the one before from its end.
    fn step_chapter(&self, forward: bool) {
        let Some(count) = self.book.borrow().as_ref().map(|book| book.chapters.len()) else {
            return;
        };
        let now = self.chapter.get();
        match forward {
            true if now + 1 < count => self.load_chapter(now + 1, Some(0.0)),
            false if now > 0 => self.load_chapter(now - 1, Some(1.0)),
            _ => {}
        }
    }

    /// Highlights the current chapter's entry in the list, and keeps it in view.
    fn mark_chapter(&self) {
        let Some(book) = self.book.borrow().clone() else {
            return;
        };
        let row = book
            .entry_for_chapter(self.chapter.get())
            .and_then(|index| self.list.row_at_index(index as i32));
        self.list.select_row(row.as_ref());
        self.scroll_list_to_chapter();
    }

    /// Scrolls the chapter list so the current chapter is a third of the way down it, once the
    /// list has been laid out.
    fn scroll_list_to_chapter(&self) {
        let list = self.list.clone();
        let scroll = self.chapters_scroll.clone();
        glib::idle_add_local_once(move || {
            let Some(row) = list.selected_row() else {
                return;
            };
            let Some(bounds) = row.compute_bounds(&list) else {
                return;
            };
            let adjustment = scroll.vadjustment();
            let top = bounds.y() as f64 - adjustment.page_size() / 3.0;
            adjustment.set_value(top.clamp(adjustment.lower(), adjustment.upper()));
        });
    }

    fn load_changed(&self, view: &webkit6::WebView, event: webkit6::LoadEvent) {
        match event {
            // A link inside the book can lead to another chapter.
            webkit6::LoadEvent::Committed => {
                let chapter = view.uri().and_then(|uri| {
                    let path = book::path_of(&uri)?.to_string();
                    self.book.borrow().as_ref()?.chapter_of(&path)
                });
                if let Some(chapter) = chapter
                    && chapter != self.chapter.get()
                {
                    self.chapter.set(chapter);
                    self.mark_chapter();
                }
                self.at_top.set(true);
                self.at_bottom.set(false);
            }
            webkit6::LoadEvent::Finished => {
                if let Some(fraction) = self.pending.take() {
                    self.scroll_to(fraction);
                }
            }
            _ => {}
        }
    }

    fn scroll_to(&self, fraction: f64) {
        let script = format!(
            "(() => {{ const el = document.scrollingElement || document.documentElement; \
             window.scrollTo(0, {fraction} * Math.max(0, el.scrollHeight - window.innerHeight)); }})()"
        );
        self.view
            .evaluate_javascript(&script, Some(WORLD), None, gio::Cancellable::NONE, |_| {});
    }

    fn scrolled(&self, fraction: f64, top: bool, bottom: bool) {
        self.fraction.set(fraction.clamp(0.0, 1.0));
        self.at_top.set(top);
        self.at_bottom.set(bottom);
        self.save_soon();
    }

    /// Links: one inside the book is followed, one out of it opens in the browser, and nothing
    /// else is loaded at all.
    fn decide(
        &self,
        decision: &webkit6::PolicyDecision,
        kind: webkit6::PolicyDecisionType,
    ) -> bool {
        if kind == webkit6::PolicyDecisionType::Response {
            return false;
        }
        let Some(navigation) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() else {
            return false;
        };
        let uri = navigation
            .navigation_action()
            .and_then(|action| action.request())
            .and_then(|request| request.uri())
            .map(|uri| uri.to_string())
            .unwrap_or_default();
        let scheme = uri
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match scheme.as_str() {
            book::SCHEME if kind == webkit6::PolicyDecisionType::NavigationAction => {
                decision.use_();
            }
            "http" | "https" | "mailto" => {
                decision.ignore();
                gtk::UriLauncher::new(&uri).launch(
                    Some(&self.window),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
            // Notes shown by the reader itself.
            "about" if uri == "about:blank" => decision.use_(),
            _ => decision.ignore(),
        }
        true
    }

    // Keys.

    /// A key pressed anywhere in the window. Whether the reader took it.
    fn key(self: &Rc<Self>, key: gdk::Key, state: gdk::ModifierType) -> bool {
        let state = state & gtk::accelerator_get_default_mod_mask();
        let ctrl = state == gdk::ModifierType::CONTROL_MASK;
        let plain = state.is_empty();
        let shift = state == gdk::ModifierType::SHIFT_MASK;
        if state.contains(gdk::ModifierType::SUPER_MASK) && key.to_lower() == gdk::Key::i {
            self.toggle_panels();
            return true;
        }
        if ctrl {
            match key {
                gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => self.zoom_by(ZOOM_STEP),
                gdk::Key::minus | gdk::Key::underscore | gdk::Key::KP_Subtract => {
                    self.zoom_by(1.0 / ZOOM_STEP)
                }
                gdk::Key::_0 | gdk::Key::KP_0 => self.zoom_to(1.0),
                gdk::Key::o => self.ask_for_book(),
                gdk::Key::Page_Down => self.step_chapter(true),
                gdk::Key::Page_Up => self.step_chapter(false),
                _ => return false,
            }
            return true;
        }
        if plain && key == gdk::Key::Escape && self.chapters_panel.reveals_child() {
            self.toggle_panels();
            return true;
        }
        // The rest move through the book, and only while the page has the keyboard: in the
        // chapter list and on the sliders the arrows are theirs.
        let on_page = gtk::prelude::GtkWindowExt::focus(&self.window).is_none_or(|focus| {
            focus == *self.view.upcast_ref::<gtk::Widget>() || focus.is_ancestor(&self.view)
        });
        if !on_page {
            return false;
        }
        match key {
            gdk::Key::Left if plain => self.step_chapter(false),
            gdk::Key::Right if plain => self.step_chapter(true),
            // Reading on past the end of a chapter, or back past its start.
            gdk::Key::space | gdk::Key::Page_Down | gdk::Key::Down | gdk::Key::KP_Page_Down
                if plain && self.at_bottom.get() =>
            {
                self.step_chapter(true)
            }
            gdk::Key::space if shift && self.at_top.get() => self.step_chapter(false),
            gdk::Key::Page_Up | gdk::Key::Up | gdk::Key::KP_Page_Up
                if plain && self.at_top.get() && self.chapter.get() > 0 =>
            {
                self.step_chapter(false)
            }
            _ => return false,
        }
        true
    }

    fn toggle_panels(&self) {
        let shown = !self.chapters_panel.reveals_child();
        self.chapters_panel.set_reveal_child(shown);
        self.colours_panel.set_reveal_child(shown);
        if shown {
            self.scroll_list_to_chapter();
        } else {
            self.view.grab_focus();
        }
        self.memory.borrow_mut().panels = shown;
        self.save_soon();
    }

    // Zoom.

    fn zoom_by(&self, factor: f64) {
        self.zoom_to(self.view.zoom_level() * factor);
    }

    /// The text at `zoom` times its size, keeping the same place in the chapter in view.
    fn zoom_to(&self, zoom: f64) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if (zoom - self.view.zoom_level()).abs() < 0.001 {
            return;
        }
        let fraction = self.fraction.get();
        self.view.set_zoom_level(zoom);
        self.memory.borrow_mut().zoom = zoom;
        self.show_zoom();
        let view = self.view.clone();
        glib::timeout_add_local_once(Duration::from_millis(60), move || {
            if let Some(reader) = READER.with(|reader| reader.borrow().clone())
                && reader.view == view
            {
                reader.scroll_to(fraction);
            }
        });
        self.save_soon();
    }

    fn show_zoom(&self) {
        let percent = (self.memory.borrow().zoom * 100.0).round();
        self.zoom_label.set_label(&format!("{percent}%"));
    }

    // Colours.

    /// A slider moved by hand, or the theme followed again.
    fn chosen(&self) {
        self.recolour();
        self.save_soon();
    }

    /// The theme's files changed: read again once they've settled, since a theme being set
    /// writes several.
    fn theme_changed_soon(&self) {
        if self.theme_pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(Duration::from_millis(300), || {
            if let Some(reader) = READER.with(|reader| reader.borrow().clone()) {
                reader.theme_pending.set(false);
                let theme = theme::current();
                if theme != reader.theme.get() {
                    reader.theme.set(theme);
                    reader.sync_sliders();
                    reader.recolour();
                }
            }
        });
    }

    /// Each slider where its colour is: the one chosen, or the nearest to the theme's.
    fn sync_sliders(&self) {
        let theme = self.theme.get();
        let (page, text) = {
            let memory = self.memory.borrow();
            (memory.page_choice, memory.text_choice)
        };
        self.syncing.set(true);
        self.page_scale
            .set_value(page.unwrap_or_else(|| palette::nearest(theme.page)));
        self.text_scale
            .set_value(text.unwrap_or_else(|| palette::nearest(theme.text)));
        self.syncing.set(false);
    }

    /// The page's and the text's colours: the theme's, or the sliders' where they were moved.
    fn colours(&self) -> (palette::Rgb, palette::Rgb) {
        let theme = self.theme.get();
        let memory = self.memory.borrow();
        (
            memory.page_choice.map_or(theme.page, palette::at),
            memory.text_choice.map_or(theme.text, palette::at),
        )
    }

    /// The page, its text and the panels in the two colours.
    fn recolour(&self) {
        let (page, text) = self.colours();
        {
            let memory = self.memory.borrow();
            self.follow_theme
                .set_visible(memory.page_choice.is_some() || memory.text_choice.is_some());
        }
        self.content.remove_all_style_sheets();
        self.content.add_style_sheet(&webkit6::UserStyleSheet::new(
            &page_css(page, text),
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserStyleLevel::User,
            &[],
            &[],
        ));
        let rgba = |c: palette::Rgb| {
            gdk::RGBA::new(
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
                1.0,
            )
        };
        self.view.set_background_color(&rgba(page));
        self.chrome
            .load_from_string(&chrome_css(page, text, self.theme.get().accent));
    }

    /// Shown in place of a book: why there isn't one, and what to do.
    fn show_note(&self, title: &str, body: &str) {
        let escape = |text: &str| {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let html = format!(
            "<!doctype html><html><body style=\"font-family: sans-serif; text-align: center; \
             padding-top: 30vh\"><h1 style=\"font-weight: 300\">{}</h1><p>{}</p></body></html>",
            escape(title),
            escape(body)
        );
        self.view.load_html(&html, None);
    }

    // The pointer.

    fn pointer_moved(&self) {
        self.view.set_cursor(None);
        if let Some(waiting) = self.pointer_rest.borrow_mut().take() {
            waiting.remove();
        }
        let view = self.view.clone();
        let id = glib::timeout_add_local_once(POINTER_REST, move || {
            if let Some(reader) = READER.with(|reader| reader.borrow().clone()) {
                reader.pointer_rest.borrow_mut().take();
            }
            view.set_cursor_from_name(Some("none"));
        });
        *self.pointer_rest.borrow_mut() = Some(id);
    }

    // Remembering.

    /// Saved a moment after the last change, so scrolling doesn't write the file on every step.
    fn save_soon(&self) {
        if self.saving.borrow().is_some() {
            return;
        }
        let id = glib::timeout_add_local_once(Duration::from_millis(800), || {
            if let Some(reader) = READER.with(|reader| reader.borrow().clone()) {
                reader.saving.borrow_mut().take();
                reader.save_now();
            }
        });
        *self.saving.borrow_mut() = Some(id);
    }

    fn save_now(&self) {
        if let Some(waiting) = self.saving.borrow_mut().take() {
            waiting.remove();
        }
        let mut memory = self.memory.borrow_mut();
        if let Some(path) = self.path.borrow().as_ref() {
            memory.set_place(
                path,
                Place {
                    chapter: self.chapter.get(),
                    fraction: self.fraction.get(),
                },
            );
        }
        if let Err(err) = memory.write(&memory::path()) {
            eprintln!("slipstream-reader: couldn't save where you were: {err}");
        }
    }
}

/// A file of the book, for the page.
fn serve(book: &Rc<RefCell<Option<Rc<Book>>>>, request: &webkit6::URISchemeRequest) {
    let path = request
        .path()
        .map(|path| path.to_string())
        .unwrap_or_default();
    let found = book.borrow().as_ref().and_then(|book| book.read(&path));
    let Some((bytes, mut media)) = found else {
        let mut err = glib::Error::new(gio::IOErrorEnum::NotFound, "not in this book");
        request.finish_error(&mut err);
        return;
    };
    // XHTML that isn't well-formed XML is shown as HTML rather than as WebKit's parse error.
    if media == "application/xhtml+xml" && !well_formed(&bytes) {
        media = "text/html".to_string();
    }
    let length = bytes.len() as i64;
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
    let response = webkit6::URISchemeResponse::new(&stream, length);
    response.set_content_type(&media);
    let headers = soup::MessageHeaders::new(soup::MessageHeadersType::Response);
    headers.append("Content-Security-Policy", POLICY);
    response.set_http_headers(headers);
    request.finish_with_response(&response);
}

fn well_formed(bytes: &[u8]) -> bool {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Eof) => return true,
            Ok(_) => buffer.clear(),
            Err(_) => return false,
        }
    }
}

/// A vertical slider along the run of colours, light at the top.
fn colour_scale(name: &str) -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 1.0, 0.01);
    scale.set_inverted(true);
    scale.set_draw_value(false);
    scale.set_vexpand(true);
    scale.add_css_class("colour");
    scale.add_css_class(name);
    scale
}

fn labelled(scale: &gtk::Scale, label: &str) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 10);
    column.append(scale);
    column.append(
        &gtk::Label::builder()
            .label(label)
            .css_classes(["caption"])
            .build(),
    );
    column
}

/// The book in the two colours, set as the reader's own stylesheet, which wins over the book's.
/// The text keeps to a column of a comfortable width, which grows with the zoom.
fn page_css(page: palette::Rgb, text: palette::Rgb) -> String {
    let page_hex = palette::hex(page);
    let text_hex = palette::hex(text);
    let faint = palette::rgba(text, 0.35);
    let selection = palette::rgba(text, 0.22);
    format!(
        "html {{ background: {page_hex} !important; scrollbar-width: none !important; }}
::-webkit-scrollbar {{ display: none !important; }}
body {{ background: {page_hex} !important; color: {text_hex} !important;
  max-width: 38em !important; margin: 0 auto !important; padding: 4em 2.5em 8em !important;
  line-height: 1.6 !important; }}
body * {{ color: inherit !important; background-color: transparent !important;
  border-color: {faint} !important; }}
a, a * {{ color: {text_hex} !important; text-decoration-color: {faint} !important; }}
img, svg, video {{ max-width: 100% !important; height: auto !important; }}
::selection {{ background: {selection} !important; }}"
    )
}

/// The panels a shade off the page, in the same text colour, so the screen stays one surface.
/// The chapter you're in is marked in the theme's accent.
fn chrome_css(page: palette::Rgb, text: palette::Rgb, accent: palette::Rgb) -> String {
    let panel = palette::hex(palette::mix(page, text, 0.05));
    let edge = palette::hex(palette::mix(page, text, 0.14));
    let chosen = palette::hex(palette::mix(page, accent, 0.16));
    let accent_hex = palette::hex(accent);
    let hover = palette::hex(palette::mix(page, text, 0.08));
    let page_hex = palette::hex(page);
    let text_hex = palette::hex(text);
    let quiet = palette::rgba(text, 0.65);
    let gradient = palette::gradient();
    format!(
        "window {{ background: {page_hex}; color: {text_hex}; }}
.panel > * {{ background: {panel}; color: {text_hex}; }}
.panel.left > * {{ border-right: 1px solid {edge}; }}
.panel.right > * {{ border-left: 1px solid {edge}; }}
.chapters {{ background: transparent; padding: 24px 0; }}
.chapters row {{ color: {quiet}; border-radius: 0; }}
.chapters row:hover {{ background: {hover}; }}
.chapters row:selected {{ background: {chosen}; color: {text_hex};
  box-shadow: inset 3px 0 {accent_hex}; }}
button.follow {{ background: {chosen}; color: {text_hex}; border: 1px solid {edge};
  box-shadow: none; padding: 4px 12px; }}
.chapters label.top {{ font-weight: 600; }}
scale.colour trough {{ min-width: 18px; border-radius: 9px; background-image: {gradient};
  border: 1px solid {edge}; }}
scale.colour highlight, scale.colour fill {{ background: none; border: none; box-shadow: none;
  min-width: 0; }}
scale.colour slider {{ min-width: 26px; min-height: 26px; border-radius: 13px;
  border: 2px solid {text_hex}; box-shadow: 0 0 0 2px {page_hex}; }}
scale.page slider {{ background: {page_hex}; }}
scale.text slider {{ background: {text_hex}; border-color: {page_hex}; box-shadow: 0 0 0 2px {text_hex}; }}
.caption, .zoom {{ color: {quiet}; font-size: 0.9em; }}"
    )
}

/// Tells the reader where the page is scrolled to: how far down, and whether at the top or end.
const SCROLL_SCRIPT: &str = r#"(() => {
  let queued = false;
  const report = () => {
    queued = false;
    const el = document.scrollingElement || document.documentElement;
    const room = el.scrollHeight - window.innerHeight;
    const y = window.scrollY;
    window.webkit.messageHandlers.reader.postMessage(
      [room > 0 ? y / room : 0, y <= 2, room - y <= 2]);
  };
  const soon = () => { if (!queued) { queued = true; requestAnimationFrame(report); } };
  addEventListener("scroll", soon, { passive: true });
  addEventListener("resize", soon);
  addEventListener("load", soon);
  report();
})();"#;
