//! A reading mode for EPUB books: one book, full screen, with nothing else on it. It began as
//! Slipstream's reading mode, and on a Slipstream desktop Super+R still opens it.
//!
//! The page is drawn by WebKit, the engine Foliate uses, from the book's own files, served from
//! inside the book under `epub://book/`. The book's scripts never run and nothing is fetched from
//! the network: a link out of the book opens in the browser.
//!
//! - Ctrl+= and Ctrl+-, Ctrl and the wheel, or a pinch make the text bigger or smaller; Ctrl+0
//!   puts it back.
//! - Ctrl+I brings out the chapters on the left, and on the right the two colour sliders, the
//!   font to read in and what the book says of its own fonts; and puts them away again.
//! - ← and → go a chapter back or on; reading past the end of one goes on into the next.
//! - h, j, k and l do what ←, ↓, ↑ and → do, on the page and in the panels; in the library,
//!   where letters find a book, Ctrl+h, j, k and l move between the books.
//! - Ctrl+O shows the library: every book in `~/Dropbox/Books`, by its cover. Typing narrows it
//!   down by title, author or file name. Enter or a click
//!   opens one; Esc goes back to the book you were reading.
//! - With the Kobo sync set up (`kobo_link`), a book the Kobo has opens as the Kobo's copy, with
//!   its highlights, notes and bookmarks: Ctrl+H highlights the selection, Ctrl+Shift+H adds a
//!   note, Ctrl+D bookmarks the page, and Ctrl+I's Notes list has them all.
//!
//! It is always full screen. The page and the text are in the desktop's theme, and change with
//! it; a slider moved takes that colour off the theme until "Follow theme" puts it back.
//!
//! `slipstream-reader BOOK.epub` opens that book, in the window already open if there is one.
//! With no book it opens the last one, where you left it, or the library when there isn't one.

mod annotations;
mod book;
mod book_fonts;
mod dirs;
mod fonts;
mod kobo_link;
mod library;
mod memory;
mod obfuscation;
mod palette;
mod sync;
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
/// How far j and k scroll the page, in CSS pixels: what an arrow key scrolls in WebKit.
const ARROW_STEP: i32 = 40;
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
    /// The library or the book.
    stack: gtk::Stack,
    shelf: gtk::FlowBox,
    shelf_count: gtk::Label,
    shelf_empty: gtk::Label,
    /// What's typed to narrow the shelf down: typing anywhere in the library starts it.
    filter: gtk::SearchEntry,
    library_button: gtk::Button,
    /// The books on the shelf as last shown, in order, and their cards.
    cards: RefCell<Vec<Card>>,
    /// Counts each filling of the shelf, so covers still coming for an old one are dropped.
    filling: Cell<u64>,
    view: webkit6::WebView,
    content: webkit6::UserContentManager,
    chapters_panel: gtk::Revealer,
    chapters_scroll: gtk::ScrolledWindow,
    colours_panel: gtk::Revealer,
    list: gtk::ListBox,
    zoom_label: gtk::Label,
    /// The font to read in: the book's own, or one of the reader's.
    font_menu: gtk::DropDown,
    /// What the book says of its fonts.
    book_fonts: gtk::Box,
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
    /// The Kobo sync, when it's set up.
    link: Option<Rc<kobo_link::Link>>,
    notes_ui: kobo_link::NotesUi,
    /// A sentence span to go to once the chapter being loaded has finished.
    pending_span: RefCell<Option<String>>,
    /// When you were last moving through this book, as the Kobo writes times.
    read_at: RefCell<Option<String>>,
    /// The page is being moved by you, not put somewhere by the reader.
    user_moving: Cell<bool>,
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
        content.register_script_message_handler("annotation", Some(WORLD));
        for script in [SCROLL_SCRIPT, annotations::SCRIPT] {
            content.add_script(&webkit6::UserScript::for_world(
                script,
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserScriptInjectionTime::End,
                WORLD,
                &[],
                &[],
            ));
        }

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
        // A button that looks like one, set apart from the chapters, so it isn't taken for a
        // heading over them.
        let library_inside = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        library_inside.append(&gtk::Image::from_icon_name("view-grid-symbolic"));
        library_inside.append(&gtk::Label::new(Some("Library")));
        let library_button = gtk::Button::builder()
            .child(&library_inside)
            .css_classes(["to-library"])
            .tooltip_text("All your books (Ctrl+O)")
            .halign(gtk::Align::Start)
            .build();
        let library_bar = gtk::Box::builder().css_classes(["library-bar"]).build();
        library_bar.append(&library_button);
        let notes_ui = kobo_link::NotesUi::build(&view);
        let link = kobo_link::Link::new();
        let left_pages = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .vexpand(true)
            .build();
        left_pages.add_titled(&chapters_scroll, Some("contents"), "Contents");
        left_pages.add_titled(&notes_ui.scroll, Some("notes"), "Notes");
        let chapters_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chapters_column.append(&library_bar);
        if link.is_some() {
            let switcher = gtk::StackSwitcher::builder()
                .stack(&left_pages)
                .css_classes(["pages"])
                .margin_top(12)
                .margin_start(16)
                .margin_end(16)
                .build();
            chapters_column.append(&switcher);
        }
        chapters_column.append(&left_pages);
        let chapters_panel = gtk::Revealer::builder()
            .child(&chapters_column)
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
        let font_names: Vec<&str> = std::iter::once(fonts::BOOKS_OWN)
            .chain(fonts::CHOICES.iter().map(|choice| choice.label))
            .collect();
        let font_menu = gtk::DropDown::from_strings(&font_names);
        font_menu.set_selected(font_index(memory.font.as_deref()));
        font_menu.add_css_class("font-menu");
        let book_fonts = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();
        let font_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        font_section.append(&heading_label("Font"));
        font_section.append(&font_menu);
        let in_book = heading_label("In this book");
        in_book.add_css_class("later");
        font_section.append(&in_book);
        font_section.append(&book_fonts);
        let colours = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(32)
            .margin_bottom(24)
            .margin_start(20)
            .margin_end(20)
            .width_request(260)
            .build();
        colours.append(&font_section);
        colours.append(&sliders);
        colours.append(&follow_theme);
        colours.append(&zoom_label);
        colours.append(&notes_ui.status);
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

        let shelf = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(true)
            .homogeneous(true)
            .min_children_per_line(2)
            .max_children_per_line(12)
            .column_spacing(18)
            .row_spacing(28)
            .valign(gtk::Align::Start)
            .css_classes(["shelf"])
            .build();
        let shelf_count = gtk::Label::builder()
            .css_classes(["caption"])
            .halign(gtk::Align::Start)
            .build();
        let shelf_empty = gtk::Label::builder()
            .label(format!(
                "No books yet. EPUBs put in {} show up here.",
                library::dir().display()
            ))
            .css_classes(["caption"])
            .wrap(true)
            .visible(false)
            .build();
        let heading = gtk::Label::builder()
            .label("Library")
            .css_classes(["library-title"])
            .halign(gtk::Align::Start)
            .build();
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(16)
            .build();
        let filter = gtk::SearchEntry::builder()
            .placeholder_text("Type to find a book")
            .hexpand(true)
            .halign(gtk::Align::End)
            .width_chars(28)
            .css_classes(["filter"])
            .build();
        head.append(&heading);
        head.append(&shelf_count);
        head.append(&filter);
        let library_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(28)
            .margin_top(48)
            .margin_bottom(64)
            .margin_start(64)
            .margin_end(64)
            .build();
        library_column.append(&head);
        library_column.append(&shelf_empty);
        library_column.append(&shelf);
        let library_page = gtk::ScrolledWindow::builder()
            .child(&library_column)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .css_classes(["library"])
            .build();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .build();
        stack.add_named(&layout, Some("book"));
        stack.add_named(&library_page, Some("library"));

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Reading")
            .default_width(1200)
            .default_height(900)
            .child(&stack)
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
            stack,
            shelf,
            shelf_count,
            shelf_empty,
            filter,
            library_button,
            cards: RefCell::new(Vec::new()),
            filling: Cell::new(0),
            view,
            content,
            chapters_panel,
            chapters_scroll,
            colours_panel,
            list,
            zoom_label,
            font_menu,
            book_fonts,
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
            link,
            notes_ui,
            pending_span: RefCell::new(None),
            read_at: RefCell::new(None),
            user_moving: Cell::new(false),
        });
        reader.connect();
        reader.connect_link();
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
        self.font_menu.connect_selected_notify(move |menu| {
            if let Some(reader) = w.upgrade() {
                reader.font_chosen(menu.selected());
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
        self.shelf.set_filter_func(move |child| {
            w.upgrade()
                .is_none_or(|reader| reader.shown_on_shelf(child))
        });
        let w = weak.clone();
        self.filter.connect_search_changed(move |_| {
            if let Some(reader) = w.upgrade() {
                reader.filtered();
            }
        });
        let w = weak.clone();
        self.filter.connect_activate(move |_| {
            if let Some(reader) = w.upgrade() {
                reader.open_first_found();
            }
        });
        let w = weak.clone();
        self.shelf.connect_child_activated(move |_, child| {
            if let Some(reader) = w.upgrade() {
                let path = reader
                    .cards
                    .borrow()
                    .get(child.index() as usize)
                    .map(|card| card.path.clone());
                if let Some(path) = path {
                    reader.open(&path);
                }
            }
        });
        let w = weak.clone();
        self.library_button.connect_clicked(move |_| {
            if let Some(reader) = w.upgrade() {
                reader.show_library();
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

    /// The last book, where it was left, or the library when there isn't one.
    fn open_last_or_ask(self: &Rc<Self>) {
        let last = self.memory.borrow().last.clone();
        match last.filter(|path| path.exists()) {
            Some(path) => self.open(&path),
            None => self.show_library(),
        }
    }

    // The library.

    fn in_library(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("library")
    }

    /// The shelf, filled again if the books in the folder have changed, with the book being read
    /// picked out.
    fn show_library(self: &Rc<Self>) {
        self.save_now();
        let found = library::scan(&library::dir());
        let same = {
            let cards = self.cards.borrow();
            cards.len() == found.len()
                && cards
                    .iter()
                    .zip(&found)
                    .all(|(card, path)| card.path == *path)
        };
        if !same {
            self.fill_shelf(found);
        }
        self.filter.set_text("");
        self.stack.set_visible_child_name("library");
        self.view.set_cursor(None);
        let current = self.path.borrow().clone();
        let at = current.and_then(|path| {
            self.cards
                .borrow()
                .iter()
                .position(|card| card.path == path)
        });
        match at.and_then(|at| self.shelf.child_at_index(at as i32)) {
            Some(child) => {
                self.shelf.select_child(&child);
                child.grab_focus();
            }
            None => {
                self.shelf.unselect_all();
                if let Some(first) = self.shelf.child_at_index(0) {
                    first.grab_focus();
                }
            }
        }
    }

    /// Whether a card is shown with what's typed into the filter.
    fn shown_on_shelf(&self, child: &gtk::FlowBoxChild) -> bool {
        let query = self.filter.text();
        if query.trim().is_empty() {
            return true;
        }
        let cards = self.cards.borrow();
        let Some(card) = cards.get(child.index() as usize) else {
            return true;
        };
        let file = library::tidy_name(&card.path);
        library::matches(&query, &[&card.title.label(), &card.author.label(), &file])
    }

    /// The cards still shown, in order.
    fn found(&self) -> Vec<gtk::FlowBoxChild> {
        (0..self.cards.borrow().len() as i32)
            .filter_map(|at| self.shelf.child_at_index(at))
            .filter(|child| child.is_child_visible())
            .collect()
    }

    /// The filter changed: the shelf narrows to match, the first book left is picked out, and
    /// the count says how many are left.
    fn filtered(&self) {
        self.shelf.invalidate_filter();
        let total = self.cards.borrow().len();
        let found = self.found();
        let typed = !self.filter.text().trim().is_empty();
        self.shelf_count.set_label(&match (typed, total) {
            (true, _) => format!("{} of {total}", found.len()),
            (false, 1) => "1 book".to_string(),
            (false, n) => format!("{n} books"),
        });
        match found.first() {
            Some(first) if typed => self.shelf.select_child(first),
            _ if typed => self.shelf.unselect_all(),
            _ => {}
        }
    }

    /// Enter in the filter: the picked-out book, or the first one found.
    fn open_first_found(self: &Rc<Self>) {
        let chosen = self
            .shelf
            .selected_children()
            .into_iter()
            .find(|child| child.is_child_visible())
            .or_else(|| self.found().into_iter().next());
        let path = chosen.and_then(|child| {
            self.cards
                .borrow()
                .get(child.index() as usize)
                .map(|card| card.path.clone())
        });
        if let Some(path) = path {
            self.open(&path);
        }
    }

    /// A key in the library. Typing goes into the filter wherever the keyboard is.
    fn library_key(self: &Rc<Self>, key: gdk::Key, plain: bool, ctrl: bool, shift: bool) -> bool {
        let typing = self.filter.has_focus() || self.filter.focus_child().is_some();
        let filtered = !self.filter.text().is_empty();
        // The arrows, and Ctrl+h, j, k and l (the letters alone find a book), move between the
        // books. The reader moves them itself: the shelf's own arrows only work once a click or
        // Tab has given it a cursor, and focusing a book from here doesn't.
        let step = match key {
            gdk::Key::Left if plain => Some((-1, 0)),
            gdk::Key::Right if plain => Some((1, 0)),
            gdk::Key::Up if plain => Some((0, -1)),
            gdk::Key::Down if plain => Some((0, 1)),
            gdk::Key::h if ctrl => Some((-1, 0)),
            gdk::Key::l if ctrl => Some((1, 0)),
            gdk::Key::k if ctrl => Some((0, -1)),
            gdk::Key::j if ctrl => Some((0, 1)),
            _ => None,
        };
        if let Some((across, down)) = step {
            if typing && down == 1 {
                // Down from the filter to the books found.
                self.focus_on_shelf(self.shelf_cursor().unwrap_or(0));
                return true;
            }
            // In the filter, ← → ↑ are the text's.
            if typing && !ctrl {
                return false;
            }
            self.move_on_shelf(across, down);
            return true;
        }
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter if plain && !typing => {
                self.open_first_found();
                true
            }
            gdk::Key::Escape if plain && filtered => {
                self.filter.set_text("");
                if let Some(first) = self.found().first() {
                    first.grab_focus();
                }
                true
            }
            gdk::Key::Escape if plain => {
                self.show_book();
                true
            }
            gdk::Key::o if ctrl => {
                self.show_book();
                true
            }
            gdk::Key::BackSpace if plain && !typing && filtered => {
                let mut text = self.filter.text().to_string();
                text.pop();
                self.filter.set_text(&text);
                true
            }
            _ if typing => false,
            _ if plain || shift => match key.to_unicode() {
                Some(ch) if !ch.is_control() && !(ch == ' ' && !filtered) => {
                    let mut text = self.filter.text().to_string();
                    text.push(ch);
                    self.filter.set_text(&text);
                    self.filter.grab_focus();
                    self.filter.set_position(-1);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Where among the books still shown the keyboard is: the focused book, or else the one
    /// picked out.
    fn shelf_cursor(&self) -> Option<usize> {
        let found = self.found();
        let focused = gtk::prelude::GtkWindowExt::focus(&self.window)
            .and_then(|widget| widget.downcast::<gtk::FlowBoxChild>().ok());
        focused
            .and_then(|child| found.iter().position(|f| *f == child))
            .or_else(|| {
                let selected = self.shelf.selected_children();
                found.iter().position(|f| selected.contains(f))
            })
    }

    /// The book `at` among those shown, picked out and focused, which scrolls it into view.
    fn focus_on_shelf(&self, at: usize) {
        if let Some(child) = self.found().get(at) {
            self.shelf.select_child(child);
            child.grab_focus();
        }
    }

    /// `across` books along and `down` rows down the shelf. Up from the top row goes to the
    /// filter.
    fn move_on_shelf(&self, across: i32, down: i32) {
        let found = self.found();
        if found.is_empty() {
            return;
        }
        let Some(at) = self.shelf_cursor() else {
            self.focus_on_shelf(0);
            return;
        };
        // How many books to a row: those level with the first.
        let top = |child: &gtk::FlowBoxChild| {
            child
                .compute_bounds(&self.shelf)
                .map_or(0.0, |bounds| bounds.y())
        };
        let first = top(&found[0]);
        let columns = found
            .iter()
            .take_while(|child| (top(child) - first).abs() < 1.0)
            .count()
            .max(1) as i32;
        let target = at as i32 + across + down * columns;
        if target < 0 && down < 0 {
            self.filter.grab_focus();
            self.filter.set_position(-1);
            return;
        }
        // Down from a row with nothing below goes to the last book.
        self.focus_on_shelf(target.clamp(0, found.len() as i32 - 1) as usize);
    }

    /// Back to the book, if there is one.
    fn show_book(&self) {
        if self.book.borrow().is_none() {
            return;
        }
        self.stack.set_visible_child_name("book");
        self.view.grab_focus();
    }

    /// A card for each book, named from its file at first; titles, authors and covers follow as
    /// each book is read, off the main thread.
    fn fill_shelf(self: &Rc<Self>, found: Vec<PathBuf>) {
        while let Some(child) = self.shelf.first_child() {
            self.shelf.remove(&child);
        }
        let cards: Vec<Card> = found.iter().map(|path| Card::new(path)).collect();
        for card in &cards {
            self.shelf.append(&card.widget);
        }
        let count = cards.len();
        self.shelf_count.set_label(&match count {
            1 => "1 book".to_string(),
            n => format!("{n} books"),
        });
        self.shelf_empty.set_visible(count == 0);
        *self.cards.borrow_mut() = cards;
        let filling = self.filling.get() + 1;
        self.filling.set(filling);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            for (at, path) in found.into_iter().enumerate() {
                let Ok(shelved) = gio::spawn_blocking(move || library::describe(&path)).await
                else {
                    continue;
                };
                let Some(reader) = weak.upgrade() else {
                    return;
                };
                if reader.filling.get() != filling {
                    return;
                }
                if let Some(card) = reader.cards.borrow().get(at) {
                    card.show(&shelved);
                }
                if !reader.filter.text().is_empty() {
                    reader.filtered();
                }
            }
        });
    }

    fn open(self: &Rc<Self>, path: &Path) {
        self.save_now();
        // The Kobo's KEPUB of this book, when the sync has it.
        let file = self.file_to_open(path);
        let opened = match Book::open(&file) {
            Ok(book) => Rc::new(book),
            Err(err) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                self.show_note(&format!("Couldn’t open {name}"), &err);
                return;
            }
        };
        self.window.set_title(Some(&opened.title));
        self.fill_chapters(&opened);
        self.show_book_fonts(&opened);
        *self.book.borrow_mut() = Some(opened.clone());
        *self.path.borrow_mut() = Some(path.to_path_buf());
        let place = {
            let mut memory = self.memory.borrow_mut();
            memory.last = Some(path.to_path_buf());
            memory.place(path).unwrap_or_default()
        };
        let chapter = place.chapter.min(opened.chapters.len() - 1);
        self.read_at.replace(place.read_at.clone());
        self.user_moving.set(false);
        self.pending_span.replace(None);
        self.stack.set_visible_child_name("book");
        self.view.grab_focus();
        self.load_chapter(chapter, Some(place.fraction));
        self.save_soon();
        self.after_open(path);
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

    /// What the book says of its fonts: each it names, whether it carries it, in what format,
    /// and for those it doesn't, whether this computer has it.
    fn show_book_fonts(&self, book: &Book) {
        while let Some(child) = self.book_fonts.first_child() {
            self.book_fonts.remove(&child);
        }
        let report = &book.fonts;
        let installed: Vec<String> = self
            .window
            .pango_context()
            .list_families()
            .iter()
            .map(|family| family.name().to_lowercase())
            .collect();
        let body_key = report.body.as_ref().map(|name| name.to_lowercase());
        if let Some(body) = &report.body
            && book_fonts::is_generic(body)
        {
            self.book_fonts.append(&font_row(
                "No font named",
                &format!("Body text asks only for {body}"),
            ));
        }
        if report.families.is_empty() && report.body.is_none() {
            self.book_fonts.append(&font_row(
                "No font named",
                "The book leaves the font to the reader",
            ));
        }
        const SHOWN: usize = 6;
        for family in report.families.iter().take(SHOWN) {
            let key = family.name.to_lowercase();
            let mut parts: Vec<String> = Vec::new();
            if Some(&key) == body_key.as_ref() {
                parts.push("Body text".to_string());
            }
            if family.embedded() {
                let formats: Vec<&str> = family.formats().iter().map(|f| f.name()).collect();
                let styles = match family.faces.len() {
                    1 => "1 style".to_string(),
                    n => format!("{n} styles"),
                };
                parts.push(format!("Embedded, {} · {styles}", formats.join(" + ")));
                if let Some(scheme) = family.obfuscation() {
                    parts.push(format!("obfuscated ({})", scheme.name()));
                }
            } else {
                parts.push("Not embedded".to_string());
                if family.missing > 0 {
                    parts.push("its file is missing".to_string());
                }
                parts.push(if installed.contains(&key) {
                    "installed here".to_string()
                } else {
                    "not installed, so another is used".to_string()
                });
            }
            self.book_fonts
                .append(&font_row(&family.name, &parts.join(" · ")));
        }
        if report.families.len() > SHOWN {
            let more = report.families.len() - SHOWN;
            self.book_fonts.append(
                &gtk::Label::builder()
                    .label(format!("and {more} more"))
                    .xalign(0.0)
                    .css_classes(["caption"])
                    .build(),
            );
        }
    }

    /// The font picked from the menu: the page set in it at once, at the same place.
    fn font_chosen(&self, index: u32) {
        let key = index
            .checked_sub(1)
            .and_then(|at| fonts::CHOICES.get(at as usize))
            .map(|choice| choice.key.to_string());
        if self.memory.borrow().font == key {
            return;
        }
        self.memory.borrow_mut().font = key;
        let fraction = self.fraction.get();
        self.recolour();
        let view = self.view.clone();
        glib::timeout_add_local_once(Duration::from_millis(120), move || {
            if let Some(reader) = READER.with(|reader| reader.borrow().clone())
                && reader.view == view
            {
                reader.scroll_to(fraction);
            }
        });
        self.save_soon();
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
        self.user_moving.set(true);
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
                let fraction = self.pending.take();
                if let Some(span) = self.pending_span.take() {
                    self.scroll_to_span(&span);
                } else if let Some(fraction) = fraction {
                    self.scroll_to(fraction);
                }
                self.redraw_marks();
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
        if self.user_moving.get() {
            self.place_moved();
        }
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
        if self.in_library() {
            return self.library_key(key, plain, ctrl, shift);
        }
        // Ctrl+I, not Super+I: Windows keeps the Windows key for itself, so this works under WSL
        // too. Not while a note is being written.
        if ctrl && key.to_lower() == gdk::Key::i && !self.notes_ui.popover.is_visible() {
            self.toggle_panels();
            return true;
        }
        let ctrl_shift = state == gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
        if ctrl_shift && key.to_lower() == gdk::Key::h {
            self.highlight_selection(true);
            return true;
        }
        if ctrl {
            match key {
                gdk::Key::h => self.highlight_selection(false),
                gdk::Key::d => self.toggle_bookmark(),
                gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => self.zoom_by(ZOOM_STEP),
                gdk::Key::minus | gdk::Key::underscore | gdk::Key::KP_Subtract => {
                    self.zoom_by(1.0 / ZOOM_STEP)
                }
                gdk::Key::_0 | gdk::Key::KP_0 => self.zoom_to(1.0),
                gdk::Key::o => self.show_library(),
                gdk::Key::Page_Down => {
                    self.user_moving.set(true);
                    self.step_chapter(true)
                }
                gdk::Key::Page_Up => {
                    self.user_moving.set(true);
                    self.step_chapter(false)
                }
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
            return plain && self.vim_in_panel(key);
        }
        // Anything from here on moves through the book, or the page itself moves.
        self.user_moving.set(true);
        match key {
            gdk::Key::Left | gdk::Key::h if plain => self.step_chapter(false),
            gdk::Key::Right | gdk::Key::l if plain => self.step_chapter(true),
            // j and k scroll as ↓ and ↑ do, and carry on into the next chapter or the one before.
            gdk::Key::j if plain && self.at_bottom.get() => self.step_chapter(true),
            gdk::Key::k if plain && self.at_top.get() && self.chapter.get() > 0 => {
                self.step_chapter(false)
            }
            gdk::Key::j if plain => self.scroll_by_lines(1),
            gdk::Key::k if plain => self.scroll_by_lines(-1),
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

    /// h, j, k and l where the arrows would go, in the panels: up and down the chapter list, and
    /// along the colour sliders. Whether it was one of them.
    fn vim_in_panel(&self, key: gdk::Key) -> bool {
        let Some(focus) = gtk::prelude::GtkWindowExt::focus(&self.window) else {
            return false;
        };
        let Some((step, count)) = vim_move(key) else {
            return false;
        };
        if focus.is_ancestor(&self.list) {
            if step == gtk::MovementStep::DisplayLines {
                self.list.emit_move_cursor(step, count, false, false);
            }
            return true;
        }
        if let Some(range) = focus.downcast_ref::<gtk::Range>() {
            range.emit_move_slider(match key {
                gdk::Key::h => gtk::ScrollType::StepLeft,
                gdk::Key::j => gtk::ScrollType::StepDown,
                gdk::Key::k => gtk::ScrollType::StepUp,
                _ => gtk::ScrollType::StepRight,
            });
            return true;
        }
        false
    }

    /// The page scrolled `lines` arrow-key steps down, or up when negative.
    fn scroll_by_lines(&self, lines: i32) {
        let script = format!("window.scrollBy(0, {})", lines * ARROW_STEP);
        self.view
            .evaluate_javascript(&script, Some(WORLD), None, gio::Cancellable::NONE, |_| {});
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

    /// The page, its text and the panels in the two colours, and the text in the font chosen.
    fn recolour(&self) {
        let (page, text) = self.colours();
        let font = self
            .memory
            .borrow()
            .font
            .as_deref()
            .and_then(fonts::choice)
            .map(fonts::css)
            .unwrap_or_default();
        {
            let memory = self.memory.borrow();
            self.follow_theme
                .set_visible(memory.page_choice.is_some() || memory.text_choice.is_some());
        }
        self.content.remove_all_style_sheets();
        self.content.add_style_sheet(&webkit6::UserStyleSheet::new(
            &format!("{}{font}", page_css(page, text, self.theme.get().accent)),
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
                    read_at: self.read_at.borrow().clone(),
                },
            );
        }
        if let Err(err) = memory.write(&memory::path()) {
            eprintln!("slipstream-reader: couldn't save where you were: {err}");
        }
    }
}

/// The size covers are shown at on the shelf, in logical pixels.
const COVER_W: i32 = 168;
const COVER_H: i32 = 252;

/// One book on the shelf: its cover, or a plain card with its title where it has none, and its
/// title and author beneath.
struct Card {
    path: PathBuf,
    widget: gtk::Box,
    cover: gtk::DrawingArea,
    picture: Rc<RefCell<Option<gdk::gdk_pixbuf::Pixbuf>>>,
    /// The title on the plain card, hidden once there is a cover.
    plain: gtk::Label,
    title: gtk::Label,
    author: gtk::Label,
}

impl Card {
    fn new(path: &Path) -> Card {
        let name = library::tidy_name(path);
        let picture: Rc<RefCell<Option<gdk::gdk_pixbuf::Pixbuf>>> = Rc::default();
        let cover = gtk::DrawingArea::builder()
            .content_width(COVER_W)
            .content_height(COVER_H)
            .css_classes(["cover"])
            .build();
        let painting = picture.clone();
        cover.set_draw_func(move |area, cr, w, h| paint_cover(area, cr, w, h, &painting.borrow()));
        let plain = gtk::Label::builder()
            .label(&name)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(14)
            .lines(6)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .margin_start(14)
            .margin_end(14)
            .css_classes(["plain-title"])
            .build();
        let framed = gtk::Overlay::builder()
            .child(&cover)
            .halign(gtk::Align::Center)
            .build();
        framed.add_overlay(&plain);
        let title = gtk::Label::builder()
            .label(&name)
            .wrap(true)
            .lines(2)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .width_request(COVER_W)
            .xalign(0.0)
            .css_classes(["book-title"])
            .build();
        let author = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .xalign(0.0)
            .visible(false)
            .css_classes(["book-author"])
            .build();
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .build();
        widget.append(&framed);
        widget.append(&title);
        widget.append(&author);
        Card {
            path: path.to_path_buf(),
            widget,
            cover,
            picture,
            plain,
            title,
            author,
        }
    }

    /// What the book says of itself, once it has been read.
    fn show(&self, shelved: &library::Shelved) {
        self.title.set_label(&shelved.title);
        self.plain.set_label(&shelved.title);
        if let Some(author) = &shelved.author {
            self.author.set_label(author);
            self.author.set_visible(true);
        }
        let picture = shelved
            .cover
            .as_ref()
            .and_then(|cover| gdk::gdk_pixbuf::Pixbuf::from_file(cover).ok());
        self.plain.set_visible(picture.is_none());
        *self.picture.borrow_mut() = picture;
        self.cover.queue_draw();
    }
}

/// A cover filling its card, cut to fit and with rounded corners; with none, a plain card a shade
/// off the page, which its title is written over.
fn paint_cover(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    w: i32,
    h: i32,
    picture: &Option<gdk::gdk_pixbuf::Pixbuf>,
) {
    let (w, h) = (w as f64, h as f64);
    let radius = 6.0;
    cr.new_sub_path();
    cr.arc(
        w - radius,
        radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        0.0,
    );
    cr.arc(
        w - radius,
        h - radius,
        radius,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        radius,
        h - radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        radius,
        radius,
        radius,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    cr.close_path();
    cr.clip();
    match picture {
        Some(pixbuf) => {
            let (pw, ph) = (pixbuf.width() as f64, pixbuf.height() as f64);
            let scale = (w / pw).max(h / ph);
            cr.translate((w - pw * scale) / 2.0, (h - ph * scale) / 2.0);
            cr.scale(scale, scale);
            cr.set_source_pixbuf(pixbuf, 0.0, 0.0);
            cr.source().set_filter(gtk::cairo::Filter::Good);
            let _ = cr.paint();
        }
        None => {
            let ink = area.color();
            cr.set_source_rgba(
                ink.red() as f64,
                ink.green() as f64,
                ink.blue() as f64,
                0.08,
            );
            let _ = cr.paint();
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

/// What h, j, k and l move by, as ←, ↓, ↑ and → would.
fn vim_move(key: gdk::Key) -> Option<(gtk::MovementStep, i32)> {
    match key {
        gdk::Key::h => Some((gtk::MovementStep::VisualPositions, -1)),
        gdk::Key::j => Some((gtk::MovementStep::DisplayLines, 1)),
        gdk::Key::k => Some((gtk::MovementStep::DisplayLines, -1)),
        gdk::Key::l => Some((gtk::MovementStep::VisualPositions, 1)),
        _ => None,
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

/// Where `key` is in the font menu: 0 for the book's own.
fn font_index(key: Option<&str>) -> u32 {
    key.and_then(|key| fonts::CHOICES.iter().position(|choice| choice.key == key))
        .map_or(0, |at| at as u32 + 1)
}

/// A small heading over a part of the panel.
fn heading_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .build()
}

/// One of the book's fonts: its name, and what's known of it beneath.
fn font_row(name: &str, detail: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(26)
            .css_classes(["font-name"])
            .build(),
    );
    row.append(
        &gtk::Label::builder()
            .label(detail)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(26)
            .css_classes(["caption"])
            .build(),
    );
    row
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
fn page_css(page: palette::Rgb, text: palette::Rgb, accent: palette::Rgb) -> String {
    let page_hex = palette::hex(page);
    let text_hex = palette::hex(text);
    let faint = palette::rgba(text, 0.35);
    let selection = palette::rgba(text, 0.22);
    let mark = palette::rgba(accent, 0.32);
    let accent_hex = palette::hex(accent);
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
::selection {{ background: {selection} !important; }}
::highlight(slipstream-mark) {{ background-color: {mark}; }}
::highlight(slipstream-note) {{ background-color: {mark}; text-decoration: underline;
  text-decoration-color: {accent_hex}; text-decoration-thickness: 2px; }}"
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
.library {{ background: {page_hex}; }}
.library-title {{ font-size: 1.8em; font-weight: 300; }}
.shelf {{ background: transparent; }}
.shelf flowboxchild {{ padding: 12px; border-radius: 10px; outline: none; }}
.shelf flowboxchild:hover {{ background: {hover}; }}
.shelf flowboxchild:focus-visible {{ box-shadow: inset 0 0 0 2px {edge}; }}
.shelf flowboxchild:selected {{ background: {chosen}; box-shadow: inset 0 0 0 2px {accent_hex}; }}
.cover {{ box-shadow: 0 2px 10px rgba(0, 0, 0, 0.35); border-radius: 6px; }}
.plain-title {{ font-size: 1.05em; }}
.book-title {{ font-weight: 600; }}
.book-author {{ color: {quiet}; font-size: 0.9em; }}
entry.filter {{ background: {panel}; color: {text_hex}; border: 1px solid {edge}; box-shadow: none; }}
entry.filter:focus-within {{ border-color: {accent_hex}; outline-color: {accent_hex}; }}
.library-bar {{ padding: 16px; border-bottom: 1px solid {edge}; }}
button.to-library {{ background: {chosen}; color: {text_hex}; border: 1px solid {edge};
  box-shadow: none; padding: 6px 14px; font-weight: 600; }}
button.to-library:hover {{ background: {hover}; border-color: {accent_hex}; }}
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
.caption, .zoom {{ color: {quiet}; font-size: 0.9em; }}
.heading {{ color: {quiet}; font-size: 0.8em; font-weight: 600; letter-spacing: 0.08em; }}
.heading.later {{ margin-top: 10px; }}
.font-name {{ font-weight: 600; }}
dropdown.font-menu > button {{ background: {hover}; color: {text_hex}; border: 1px solid {edge};
  box-shadow: none; }}
dropdown.font-menu > button:hover {{ background: {chosen}; }}
popover > contents {{ background: {panel}; color: {text_hex}; border: 1px solid {edge}; }}
popover listview row:hover, popover listview row:selected {{ background: {chosen}; }}
stackswitcher.pages button {{ background: none; border: 1px solid {edge}; box-shadow: none;
  color: {quiet}; padding: 4px 14px; }}
stackswitcher.pages button:checked {{ background: {chosen}; color: {text_hex}; border-color: {accent_hex}; }}
.notes row {{ border-bottom: 1px solid {edge}; }}
.notes row:hover {{ background: {hover}; }}
.quote {{ color: {text_hex}; }}
.note-said {{ color: {accent_hex}; font-style: italic; }}
textview.note-text, textview.note-text text {{ background: {page_hex}; color: {text_hex}; }}
textview.note-text {{ border: 1px solid {edge}; padding: 6px; }}"
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
