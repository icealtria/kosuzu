//! Main window: full editor windows plus a compact popup.
//!
//! Popup auto-hide on focus loss relies on niri's window focus (`niri msg -j windows`
//! is_focused), not GTK's is-active; `niri_windows_claim_focus` is the pure decision fn.

use crate::clipboard;
use libadwaita::prelude::*;
use crate::config::Config;
use kosuzu_ocr::{ensure_preset, is_downloaded, models_dir, normalize_preset, OcrEngine, OcrLine};
use crate::screenshot::{self, ScreenshotError, ScreenshotResult};
use crate::translate::GoogleTranslator;
use crate::ui::{crop_window, lang_picker::LangPicker, settings};
use crate::worker::run_in_worker;
use gtk4::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Auto-detect"),
    ("af", "Afrikaans"),
    ("sq", "Albanian"),
    ("am", "Amharic"),
    ("ar", "Arabic"),
    ("hy", "Armenian"),
    ("az", "Azerbaijani"),
    ("eu", "Basque"),
    ("be", "Belarusian"),
    ("bn", "Bengali"),
    ("bs", "Bosnian"),
    ("bg", "Bulgarian"),
    ("my", "Burmese"),
    ("ca", "Catalan"),
    ("zh-CN", "Chinese (Simplified)"),
    ("zh-TW", "Chinese (Traditional)"),
    ("hr", "Croatian"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("en", "English"),
    ("et", "Estonian"),
    ("fil", "Filipino"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("fy", "Frisian"),
    ("gl", "Galician"),
    ("ka", "Georgian"),
    ("de", "German"),
    ("el", "Greek"),
    ("gn", "Guarani"),
    ("gu", "Gujarati"),
    ("ht", "Haitian Creole"),
    ("ha", "Hausa"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("is", "Icelandic"),
    ("ig", "Igbo"),
    ("id", "Indonesian"),
    ("ga", "Irish"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("kn", "Kannada"),
    ("km", "Khmer"),
    ("ko", "Korean"),
    ("ky", "Kyrgyz"),
    ("lo", "Lao"),
    ("lv", "Latvian"),
    ("ln", "Lingala"),
    ("lt", "Lithuanian"),
    ("lb", "Luxembourgish"),
    ("mk", "Macedonian"),
    ("ms", "Malay"),
    ("ml", "Malayalam"),
    ("mt", "Maltese"),
    ("mi", "Maori"),
    ("mr", "Marathi"),
    ("mn", "Mongolian"),
    ("ne", "Nepali"),
    ("no", "Norwegian"),
    ("oc", "Occitan"),
    ("or", "Odia (Oriya)"),
    ("ps", "Pashto"),
    ("fa", "Persian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("pa", "Punjabi"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sr", "Serbian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("so", "Somali"),
    ("es", "Spanish"),
    ("sw", "Swahili"),
    ("sv", "Swedish"),
    ("tl", "Tagalog"),
    ("tg", "Tajik"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("th", "Thai"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("uz", "Uzbek"),
    ("vi", "Vietnamese"),
    ("cy", "Welsh"),
    ("zu", "Zulu"),
];

const FULL_SIZE: (i32, i32) = (720, 640);
const POPUP_SIZE: (i32, i32) = (560, 430);
/// cold-start PRIMARY clipboard retry attempts
const CLIPBOARD_ATTEMPTS: u32 = 10;
const CLIPBOARD_DELAY: Duration = Duration::from_millis(300);



/// does this pid have a focused window in `niri msg -j windows` output.
fn niri_windows_claim_focus(raw: &str, pid: u32) -> bool {
    #[derive(serde::Deserialize)]
    struct NiriWindow {
        pid: Option<i32>,
        is_focused: Option<bool>,
    }

    serde_json::from_str::<Vec<NiriWindow>>(raw)
        .map(|wins| {
            wins.iter()
                .any(|w| w.pid == Some(pid as i32) && w.is_focused == Some(true))
        })
        .unwrap_or(false)
}

fn niri_has_focus() -> bool {
    let mut cmd = std::process::Command::new("niri");
    cmd.args(["msg", "-j", "windows"]);
    match screenshot::run_with_timeout(cmd, Duration::from_secs(2)) {
        screenshot::RunOutcome::Exited(_, out, _) => {
            niri_windows_claim_focus(&out, std::process::id())
        }
        _ => false, // treat lookup failure/timeout as unfocused
    }
}

fn texture_from_path(path: &Path) -> Option<gdk::Texture> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(
        gdk::MemoryTexture::new(
            w as i32,
            h as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(img.into_raw()),
            (w * 4) as usize,
        )
        .into(),
    )
}

fn temp_png(prefix: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{prefix}-{}-{stamp}.png", std::process::id()))
}

struct W {
    source_lang: LangPicker,
    target_lang: LangPicker,
    source_view: gtk4::TextView,
    result_view: gtk4::TextView,
    scroll_src: gtk4::ScrolledWindow,
    scroll_res: gtk4::ScrolledWindow,
    row_text: gtk4::Box,
    cards: gtk4::Box,
    header: libadwaita::HeaderBar,
    title_label: gtk4::Label,
    lang_row: gtk4::Box,
    bottom_lang_row: gtk4::Box,
    /// OCR-row buttons hidden in popup mode (copy stays)
    ocr_extra: Vec<gtk4::Button>,
    btn_swap: gtk4::Button,
    btn_clear: gtk4::Button,
    btn_translate: gtk4::Button,
    btn_screenshot: gtk4::Button,
    btn_copy: gtk4::Button,
    btn_expand: gtk4::Button,
    btn_lock: gtk4::ToggleButton,
    status: gtk4::Label,
    spinner: gtk4::Spinner,
    footer: gtk4::Box,
    controls: Vec<gtk4::Widget>,
}

struct S {
    config: Rc<RefCell<Config>>,
    busy: Cell<bool>,
    popup_mode: Cell<bool>,
    hide_when_idle: Cell<bool>,
    was_active: Cell<bool>,
    locked: Cell<bool>,
    focus_watch: RefCell<Option<glib::SourceId>>,
    screenshot_was_visible: Cell<bool>,
    /// one translator session, reused across calls
    translator: Arc<Mutex<GoogleTranslator>>,
    /// lazy-loaded OCR engine, reused across invocations
    engine: Arc<Mutex<Option<(String, OcrEngine)>>>,
    w: W,
}

#[derive(Clone)]
pub(crate) struct MainWindow {
    pub(crate) win: libadwaita::ApplicationWindow,
    s: Rc<S>,
}

impl MainWindow {
    pub(crate) fn new(app: &libadwaita::Application) -> Self {
        let win = libadwaita::ApplicationWindow::builder()
            .application(app)
            .title("kosuzu")
            .default_width(FULL_SIZE.0)
            .default_height(FULL_SIZE.1)
            .build();
        let (w, outer) = build_ui();
        let this = Self {
            win,
            s: Rc::new(S {
                config: Rc::new(RefCell::new(Config::load())),
                busy: Cell::new(false),
                popup_mode: Cell::new(false),
                hide_when_idle: Cell::new(false),
                was_active: Cell::new(false),
                locked: Cell::new(false),
                focus_watch: RefCell::new(None),
                screenshot_was_visible: Cell::new(false),
                translator: Arc::new(Mutex::new(GoogleTranslator::new(Duration::from_secs(15)))),
                engine: Arc::new(Mutex::new(None)),
                w,
            }),
        };
        // widgets are placed first, signals connected after (closures need Self)
        let toolbar = libadwaita::ToolbarView::new();
        toolbar.add_top_bar(&this.s.w.header);
        toolbar.set_content(Some(&outer));
        this.win.set_content(Some(&toolbar));
        this.restore_config();
        this.connect_events();

        this
    }

    // ---------- layout ----------

    pub(crate) fn set_popup_mode(&self, popup: bool) {
        if popup == self.s.popup_mode.get() {
            return;
        }
        let was_visible = self.win.is_visible();
        if was_visible {
            self.win.set_visible(false); // unmap first so the new default size applies
        }
        self.s.popup_mode.set(popup);
        self.s.w.row_text.set_visible(!popup);
        for btn in &self.s.w.ocr_extra {
            btn.set_visible(!popup);
        }
        self.s.w.btn_expand.set_visible(popup);
        self.s.w.btn_lock.set_visible(popup);
        self.s.w.scroll_src.set_min_content_height(if popup { 70 } else { 130 });
        self.s.w.scroll_res.set_min_content_height(if popup { 110 } else { 130 });
        let (w, h) = if popup { POPUP_SIZE } else { FULL_SIZE };
        self.win.set_default_size(w, h);
        if was_visible {
            self.win.present();
        }
    }

    // ---------- CLI entry points (called by app.rs) ----------

    pub(crate) fn cmd_start_selection(&self) {
        self.set_busy(true, Some("Reading selection…"));
        self.retry_primary(CLIPBOARD_ATTEMPTS);
    }

    pub(crate) fn cmd_clipboard_ocr(&self) {
        self.set_busy(true, Some("Reading clipboard image…"));
        self.retry_texture(CLIPBOARD_ATTEMPTS);
    }

    pub(crate) fn start_screenshot(&self) {
        if self.s.busy.get() {
            return;
        }
        self.set_busy(true, Some("Capturing… (drag a region, Esc to cancel)"));
        // the window would cover the screen: hide it, restored by present_after_screenshot
        let was_visible = self.win.is_visible();
        self.s.screenshot_was_visible.set(was_visible);
        if was_visible {
            self.win.set_visible(false);
        }
        let backend = self.s.config.borrow().screenshot_backend.clone();
        let this = self.clone();
        run_in_worker(
            move || screenshot::take_screenshot(&backend, true).map_err(anyhow::Error::new),
            move |result| this.handle_screenshot_result(result),
        );
    }

    pub(crate) fn ocr_image(&self, path: &Path) {
        let (preset, auto) = {
            let cfg = self.s.config.borrow();
            (normalize_preset(&cfg.ocr_model), cfg.auto_translate_after_ocr)
        };
        let dir = models_dir();
        let status = if is_downloaded(&dir, preset) {
            "Running OCR…"
        } else {
            "Running OCR… (first run downloads the models)"
        };
        self.set_busy(true, Some(status));
        let started = Instant::now();
        let engine = self.s.engine.clone();
        let path = path.to_path_buf();
        let this = self.clone();
        run_in_worker(
            move || {
                let img = image::open(&path)?.to_rgb8();
                ensure_preset(&dir, preset)?;
                let mut guard = engine.lock().expect("OCR engine mutex poisoned");
                if !guard.as_ref().is_some_and(|(loaded, _)| loaded == preset) {
                    *guard = Some((preset.to_owned(), OcrEngine::from_dir(&dir, preset)?));
                }
                let lines = guard.as_mut().expect("just initialized").1.recognize(&img)?;
                Ok(group_paragraphs(&lines))
            },
            move |result| {
                this.set_busy(false, None);
                match result {
                    Ok(text) => {
                        if text.trim().is_empty() {
                            this.set_status("No text recognized");
                            return;
                        }
                        this.set_source_text(&text);
                        if auto {
                            this.cmd_translate();
                        } else {
                            this.set_status(&format!(
                                "OCR done · {} chars · {:.1}s",
                                text.chars().count(),
                                started.elapsed().as_secs_f32()
                            ));
                        }
                    }
                    Err(e) => this.set_status(&format!("OCR failed: {e}")),
                }
            },
        );
    }

    pub(crate) fn cmd_translate_text(&self, text: &str) {
        self.set_source_text(text);
        self.cmd_translate();
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.s.busy.get()
    }

    // ---------- translate ----------

    fn cmd_translate(&self) {
        if self.s.busy.get() {
            return;
        }
        let text = self.source_text();
        if text.trim().is_empty() {
            self.set_status("Nothing to translate");
            return;
        }
        let (src, tgt) = self.selected_langs();
        let started = Instant::now();
        self.set_busy(true, Some("Translating…"));

        let translator = self.s.translator.clone();
        let this = self.clone();
        run_in_worker(
            move || {
                translator
                    .lock()
                    .expect("translator mutex poisoned")
                    .translate(&text, &src, &tgt)
            },
            move |result| {
                this.set_busy(false, None);
                match result {
                    Ok(r) => {
                        this.set_result_text(&r.text);
                        let detail = r.detected_source_lang.as_deref().unwrap_or("auto");
                        this.set_status(&format!(
                            "Translated · {detail} · {:.1}s",
                            started.elapsed().as_secs_f32()
                        ));
                    }
                    Err(e) => this.set_status(&format!("Translate failed: {e}")),
                }
            },
        );
    }

    fn cmd_translate_selection(&self, text: Option<String>) {
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
            self.set_busy(false, None);
            self.set_status("No selection read");
            return;
        };
        // clear the read-stage busy first, else the translate guard rejects this request
        self.set_busy(false, None);
        self.cmd_translate_text(&text);
    }

    // ---------- clipboard reads (cold-start retries) ----------

    fn retry_primary(&self, remaining: u32) {
        let this = self.clone();
        clipboard::read_primary_text_async(move |value| {
            let has_value = value.as_deref().is_some_and(|t| !t.trim().is_empty());
            if has_value {
                this.cmd_translate_selection(value);
            } else if remaining > 1 {
                glib::timeout_add_local_once(CLIPBOARD_DELAY, move || {
                    this.retry_primary(remaining - 1)
                });
            } else {
                this.cmd_translate_selection(value); // gave up: the caller reports the miss
            }
        });
    }

    fn retry_texture(&self, remaining: u32) {
        let this = self.clone();
        clipboard::read_clipboard_texture_async(move |value| {
            if value.is_some() {
                this.after_clipboard_texture(value);
            } else if remaining > 1 {
                glib::timeout_add_local_once(CLIPBOARD_DELAY, move || {
                    this.retry_texture(remaining - 1)
                });
            } else {
                this.after_clipboard_texture(value);
            }
        });
    }

    fn after_clipboard_texture(&self, texture: Option<gdk::Texture>) {
        let Some(texture) = texture else {
            self.set_busy(false, None);
            self.set_status("No image in clipboard");
            return;
        };
        let path = temp_png("kosuzu-clip");
        if let Err(e) = texture.save_to_png(&path) {
            self.set_busy(false, None);
            self.set_status(&format!("Failed to save clipboard image: {e}"));
            return;
        }
        self.ocr_image(&path);
    }

    // ---------- screenshot flow ----------

    fn handle_screenshot_result(&self, result: anyhow::Result<ScreenshotResult>) {
        let shot = match result {
            Err(e) => {
                self.present_after_screenshot(false);
                self.set_busy(false, None);
                match e.downcast_ref::<ScreenshotError>() {
                    Some(ScreenshotError::Cancelled) => self.set_status("Screenshot cancelled"),
                    _ => self.set_status(&format!("Screenshot failed: {e}")),
                }
                return;
            }
            Ok(shot) => shot,
        };
        if shot.region_selected {
            // slurp already picked a region: OCR directly
            self.present_after_screenshot(true);
            self.ocr_image(&shot.path);
            return;
        }
        match texture_from_path(&shot.path) {
            None => {
                self.present_after_screenshot(false);
                self.set_busy(false, None);
                self.set_status("Cannot read the screenshot");
            }
            Some(texture) => {
                // full-screen backends crop first; the main window stays hidden
                let this = self.clone();
                crop_window::open(self.win.upcast_ref(), &texture, shot.path.clone(), move |path| {
                    this.after_crop(path)
                });
            }
        }
    }

    fn after_crop(&self, path: Option<PathBuf>) {
        let Some(path) = path else {
            self.present_after_screenshot(false);
            self.set_busy(false, None);
            self.set_status("Cancelled");
            return;
        };
        // present first, then busy=False: the auto-hide check then sees the returned focus
        self.present_after_screenshot(true);
        self.ocr_image(&path);
    }

    fn present_after_screenshot(&self, force: bool) {
        if !force && !self.s.screenshot_was_visible.get() {
            return;
        }
        // clear the hide-set flag, else the fresh result would be hidden too
        self.s.hide_when_idle.set(false);
        self.win.present();
    }

    // ---------- button handlers ----------

    fn on_clear(&self) {
        self.s.w.source_view.buffer().set_text("");
        self.s.w.result_view.buffer().set_text("");
        self.set_status("Cleared");
    }

    fn on_copy_result(&self) {
        let text = self.result_text();
        if text.is_empty() {
            self.set_status("No translation to copy");
            return;
        }
        clipboard::set_clipboard_text(&text);
        self.set_status("Translation copied");
    }

    fn on_swap_langs(&self) {
        let (src, tgt) = self.selected_langs();
        if src == "auto" {
            return;
        }
        self.s.w.source_lang.set_code(&tgt, LANGUAGES);
        self.s.w.target_lang.set_code(&src, LANGUAGES);
    }

    fn on_lang_changed(&self) {
        let (src, tgt) = self.selected_langs();
        let mut cfg = self.s.config.borrow_mut();
        cfg.source_lang = src;
        cfg.target_lang = tgt;
        cfg.save();
    }

    fn on_lock_toggled(&self, btn: &gtk4::ToggleButton) {
        let locked = btn.is_active();
        self.s.locked.set(locked);
        {
            let mut cfg = self.s.config.borrow_mut();
            cfg.popup_lock = locked;
            cfg.save();
        }
        if !locked && !self.s.busy.get() && !self.is_active() {
            // unlocking while unfocused: hide right away, no new event will fire the check
            self.hide_check();
        }
    }

    fn on_active_changed(&self) {
        if self.is_active() {
            self.s.was_active.set(true);
            self.s.hide_when_idle.set(false);
        } else if self.s.was_active.replace(false) && self.s.popup_mode.get() {
            // hide on focus loss, but only after the running flow finished
            if self.s.busy.get() {
                self.s.hide_when_idle.set(true);
            } else {
                self.hide_check();
            }
        }
    }

    // ---------- hide on focus loss ----------

    fn hide_check(&self) {
        if self.s.locked.get()
            || !self.s.popup_mode.get()
        {
            return;
        }
        if self.open_popovers() || niri_has_focus() {
            self.start_focus_watch();
            return;
        }
        self.win.set_visible(false);
    }

    fn open_popovers(&self) -> bool {
        let mut stack = vec![self.win.clone().upcast::<gtk4::Widget>()];
        while let Some(cur) = stack.pop() {
            if cur.is::<gtk4::Popover>() && cur.is_visible() {
                return true;
            }
            let mut child = cur.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                stack.push(c);
            }
        }
        false
    }

    fn start_focus_watch(&self) {
        if self.s.focus_watch.borrow().is_some() {
            return;
        }
        let this = self.clone();
        let id = glib::timeout_add_local(Duration::from_millis(400), move || {
            this.focus_watch_tick()
        });
        *self.s.focus_watch.borrow_mut() = Some(id);
    }

    fn focus_watch_tick(&self) -> glib::ControlFlow {
        if self.is_active() {
            // keyboard is back: the event path takes over
            *self.s.focus_watch.borrow_mut() = None;
            return glib::ControlFlow::Break;
        }
        if !self.s.popup_mode.get()
            || self.s.locked.get()
        {
            *self.s.focus_watch.borrow_mut() = None;
            return glib::ControlFlow::Break;
        }
        if self.s.busy.get() {
            self.s.hide_when_idle.set(true);
            *self.s.focus_watch.borrow_mut() = None;
            return glib::ControlFlow::Break; // set_busy(false) reruns the check
        }
        if self.open_popovers() || niri_has_focus() {
            return glib::ControlFlow::Continue; // still ours: popover open or niri focus present
        }
        *self.s.focus_watch.borrow_mut() = None;
        self.win.set_visible(false);
        glib::ControlFlow::Break
    }

    fn maybe_hide_deferred(&self) {
        if !self.s.hide_when_idle.get() {
            return;
        }
        if self.is_active() || !self.s.popup_mode.get() {
            self.s.hide_when_idle.set(false);
            return;
        }
        if self.s.busy.get() {
            return; // a new flow started; wait for its set_busy(false)
        }
        self.s.hide_when_idle.set(false);
        self.hide_check();
    }

    // ---------- internals ----------

    fn is_active(&self) -> bool {
        self.win.property("is-active")
    }

    fn set_status(&self, message: &str) {
        self.s.w.status.set_text(message);
    }

    fn set_busy(&self, busy: bool, message: Option<&str>) {
        self.s.busy.set(busy);
        for widget in &self.s.w.controls {
            widget.set_sensitive(!busy);
        }
        if busy {
            self.s.w.spinner.start();
        } else {
            self.s.w.spinner.stop();
        }
        if let Some(message) = message {
            self.set_status(message);
        }
        if !busy && self.s.hide_when_idle.get() {
            // within one flow the synchronous busy=False->True hand-off is not a flow end
            let this = self.clone();
            glib::timeout_add_local_once(Duration::ZERO, move || this.maybe_hide_deferred());
        }
    }

    fn source_text(&self) -> String {
        buffer_text(&self.s.w.source_view)
    }

    fn result_text(&self) -> String {
        buffer_text(&self.s.w.result_view)
    }

    fn set_source_text(&self, text: &str) {
        self.s.w.source_view.buffer().set_text(text);
    }

    fn set_result_text(&self, text: &str) {
        self.s.w.result_view.buffer().set_text(text);
    }

    fn selected_langs(&self) -> (String, String) {
        (self.s.w.source_lang.code(), self.s.w.target_lang.code())
    }

    // ---------- assembly ----------

    fn restore_config(&self) {
        {
            let cfg = self.s.config.borrow();
            self.s.w.source_lang.set_code(&cfg.source_lang, LANGUAGES);
            self.s.w.target_lang.set_code(&cfg.target_lang, LANGUAGES);
            // set_active before connecting; no toggled event would fire
            self.s.locked.set(cfg.popup_lock);
        }
        self.s.w.btn_lock.set_active(self.s.locked.get());
        let cfg = self.s.config.borrow();
        self.s.w.footer.set_visible(cfg.show_status_bar);
    }

    pub(crate) fn sync_status_bar(&self) {
        let visible = self.s.config.borrow().show_status_bar;
        self.s.w.footer.set_visible(visible);
    }

    fn connect_events(&self) {
        // Responsive layout: below ~620px the two cards stack and the language
        // pickers move to the bottom, Dialect-style.
        let s = self.s.clone();
        let win = self.win.clone();
        let wide_cell = Cell::new(false);
        let win2 = win.clone();
        win.add_tick_callback(move |_, _| {
            let (min_w, _, _, _) = win2.measure(gtk4::Orientation::Horizontal, -1);
            // wide layout is entered again only by re-expanding reasonably sized;
            // leaving it happens when it literally cannot shrink any further,
            // otherwise the wide layout's own minimum width blocks ever switching
            let wide = if wide_cell.get() {
                win2.width() > min_w + 4
            } else {
                win2.width() >= 720
            };
            if wide != wide_cell.get() {
                wide_cell.set(wide);
                let w = &s.w;
                w.cards.set_orientation(if wide {
                    gtk4::Orientation::Horizontal
                } else {
                    gtk4::Orientation::Vertical
                });
                w.cards.set_homogeneous(true);
                if wide {
                    w.source_lang.widget.set_size_request(220, -1);
                    w.target_lang.widget.set_size_request(220, -1);
                } else {
                    w.source_lang.widget.set_size_request(130, -1);
                    w.target_lang.widget.set_size_request(130, -1);
                }
                if let Some(parent) = w.lang_row.parent() {
                    if let Some(b) = parent.downcast_ref::<gtk4::Box>() {
                        b.remove(&w.lang_row);
                    }
                }
                if wide {
                    w.header.set_title_widget(Some(&w.lang_row));
                } else {
                    w.header.set_title_widget(Some(&w.title_label));
                    w.bottom_lang_row.append(&w.lang_row);
                }
            }
            glib::ControlFlow::Continue
        });

        let css = gtk4::CssProvider::new();
        css.load_from_string(
            "window { background-color: #fafafb; }\
             headerbar { background: #fafafb; border: none; box-shadow: none; }\
             frame { background: #ffffff; border: 1px solid #e4e4e8; border-radius: 12px; }\
             scrolledwindow { background: transparent; }\
             scrolledwindow > viewport { background: transparent; }\
             textview { background: transparent; }\
             textview text { background: transparent; }\
             menubutton.lang-pill, menubutton.lang-pill > button { background: #ededf1; box-shadow: none; }\
             menubutton.lang-pill { border-radius: 14px; padding: 1px 2px; }\
             menubutton.lang-pill > button { border-radius: 14px; }\
             .settings-card { background: #ffffff; border: 1px solid #e4e4e8; border-radius: 12px; }\
             label.heading { font-weight: bold; }\
             .settings-dim { background-color: rgba(0,0,0,0.18); }\
             .settings-panel { background: #ffffff; border-radius: 14px; box-shadow: 0 8px 30px rgba(0,0,0,0.25); }",
        );
        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::style_context_add_provider_for_display(
                &display,
                &css,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let this = self.clone();
        self.s.w.btn_swap.connect_clicked(move |_| this.on_swap_langs());
        let this = self.clone();
        self.s
            .w
            .source_lang
            .connect_changed(move || this.on_lang_changed());
        let this = self.clone();
        self.s
            .w
            .target_lang
            .connect_changed(move || this.on_lang_changed());

        let this = self.clone();
        self.s.w.btn_clear.connect_clicked(move |_| this.on_clear());
        let this = self.clone();
        self.s.w.btn_translate.connect_clicked(move |_| this.cmd_translate());
        let this = self.clone();
        self.s.w.btn_screenshot.connect_clicked(move |_| this.start_screenshot());
        let this = self.clone();
        self.s.w.btn_copy.connect_clicked(move |_| this.on_copy_result());
        let this = self.clone();
        self.s.w.btn_expand.connect_clicked(move |_| this.set_popup_mode(false));
        let this = self.clone();
        self.s.w.btn_lock.connect_toggled(move |btn| this.on_lock_toggled(btn));


        // Ctrl+V on the source view: an image in the clipboard goes straight to
        // OCR; text pastes as usual.
        let sc = gtk4::ShortcutController::new();
        sc.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let this = self.clone();
        let trigger = gtk4::ShortcutTrigger::parse_string("<Control>v");
        let paste_action = gtk4::CallbackAction::new(move |_, _| {
                let this = this.clone();
                clipboard::read_clipboard_texture_async(move |texture| {
                    if texture.is_some() {
                        this.after_clipboard_texture(texture);
                    } else {
                        let this = this.clone();
                        clipboard::read_clipboard_text_async(move |text| {
                            if let Some(text) = text {
                                let buf = this.s.w.source_view.buffer();
                                let mut iter = buf.iter_at_mark(&buf.get_insert());
                                buf.insert(&mut iter, &text);
                            }
                        });
                    }
                });
                glib::Propagation::Stop
        });
        sc.add_shortcut(gtk4::Shortcut::new(trigger, Some(paste_action)));
        self.s.w.source_view.add_controller(sc);

        let action = gio::SimpleAction::new("translate", None::<&glib::VariantTy>);
        {
            let this = self.clone();
            action.connect_activate(move |_, _| this.cmd_translate());
        }
        self.win.add_action(&action);
        let prefs = gio::SimpleAction::new("preferences", None::<&glib::VariantTy>);
        {
            let this = self.clone();
            prefs.connect_activate(move |_, _| {
                settings::open(
                    this.win.upcast_ref(),
                    this.s.config.clone(),
                    Rc::new({
                        let this = this.clone();
                        move || this.sync_status_bar()
                    }) as Rc<dyn Fn()>,
                );
            });
        }
        self.win.add_action(&prefs);
        if let Some(app) = self.win.application() {
            app.set_accels_for_action("win.translate", &["<Control>Return"]);
        }

        let key_ctl = gtk4::EventControllerKey::new();
        {
            let this = self.clone();
            key_ctl.connect_key_pressed(move |_, keyval, _, _| {
                if keyval == gdk::Key::Escape && this.s.popup_mode.get() {
                    this.win.set_visible(false);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        self.win.add_controller(key_ctl);

        let this = self.clone();
        self.win.connect_is_active_notify(move |_| this.on_active_changed());
    }
}

fn group_paragraphs(lines: &[OcrLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut lines: Vec<&OcrLine> = lines.iter().collect();
    lines.sort_by_key(|l| l.poly.iter().map(|p| p[1]).min().unwrap());
    let heights: Vec<i32> = lines
        .iter()
        .map(|l| {
            let ys: Vec<i32> = l.poly.iter().map(|p| p[1]).collect();
            ys.iter().max().unwrap() - ys.iter().min().unwrap()
        })
        .collect();
    let avg_h = heights.iter().sum::<i32>() / heights.len() as i32;

    let mut paras: Vec<Vec<&OcrLine>> = vec![vec![lines[0]]];
    for (prev, cur) in lines.iter().zip(lines.iter().skip(1)) {
        let prev_bottom = prev.poly.iter().map(|p| p[1]).max().unwrap();
        let cur_top = cur.poly.iter().map(|p| p[1]).min().unwrap();
        let prev_l = prev.poly.iter().map(|p| p[0]).min().unwrap();
        let prev_r = prev.poly.iter().map(|p| p[0]).max().unwrap();
        let cur_l = cur.poly.iter().map(|p| p[0]).min().unwrap();
        let cur_r = cur.poly.iter().map(|p| p[0]).max().unwrap();
        let x_overlap = cur_l < prev_r && prev_l < cur_r;
        if x_overlap && cur_top - prev_bottom < avg_h / 2 {
            paras.last_mut().unwrap().push(cur);
        } else {
            paras.push(vec![cur]);
        }
    }
    paras
        .iter()
        .map(|p| join_paragraph(p))
        .collect::<Vec<_>>()
        .join("\n")
}

/// CJK wraps mid-sentence, so a line break there is glue; English wraps mid-word, so the break
/// needs a space or the two halves of a word fuse together.
fn join_paragraph(lines: &[&OcrLine]) -> String {
    let mut out = String::new();
    for l in lines {
        if !glued(out.chars().last(), l.text.chars().next()) {
            out.push(' ');
        }
        out.push_str(&l.text);
    }
    out
}

fn glued(prev: Option<char>, cur: Option<char>) -> bool {
    prev.is_none_or(|c| c.is_whitespace() || cjk(c)) || cur.is_none_or(cjk)
}

/// Han, kana, Hangul, CJK punctuation and fullwidth forms.
fn cjk(c: char) -> bool {
    let c = c as u32;
    matches!(
        c,
        0x1100..=0x11FF
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFFEF
            | 0x20000..=0x2FA1F
    )
}

fn buffer_text(view: &gtk4::TextView) -> String {
    let buf = view.buffer();
    buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string()
}

fn build_ui() -> (W, gtk4::Box) {
    let source_lang = LangPicker::new(LANGUAGES, "auto");
    let target_lang = LangPicker::new(&LANGUAGES[1..], "en");
    source_lang.widget.set_hexpand(false);
    target_lang.widget.set_hexpand(false);

    let button = |label: &str, tooltip: Option<&str>| {
        let btn = gtk4::Button::with_label(label);
        if let Some(t) = tooltip {
            btn.set_tooltip_text(Some(t));
        }
        btn
    };

    let btn_swap = button("⇄", Some("Swap languages"));
    btn_swap.add_css_class("flat");
    let btn_clear = gtk4::Button::from_icon_name("edit-clear-symbolic");
    btn_clear.add_css_class("flat");
    btn_clear.set_tooltip_text(Some("Clear"));
    let btn_translate = button("Translate", Some("Ctrl+Enter"));
    btn_translate.add_css_class("suggested-action");
    let btn_screenshot = gtk4::Button::from_icon_name("camera-photo-symbolic");
    btn_screenshot.add_css_class("flat");
    btn_screenshot.set_tooltip_text(Some("Screenshot OCR"));
    let btn_copy = gtk4::Button::from_icon_name("edit-copy-symbolic");
    btn_copy.add_css_class("flat");
    btn_copy.set_tooltip_text(Some("Copy translation"));
    let btn_expand = gtk4::Button::from_icon_name("view-fullscreen-symbolic");
    btn_expand.set_tooltip_text(Some("Switch to the full editor"));
    btn_expand.set_visible(false);
    let btn_lock = gtk4::ToggleButton::new();
    btn_lock.set_child(Some(&gtk4::Image::from_icon_name("view-pin-symbolic")));
    btn_lock.set_tooltip_text(Some("Pinned: keep the popup open when unfocused"));
    btn_lock.set_visible(false);

    let source_view = gtk4::TextView::builder()
        .wrap_mode(gtk4::WrapMode::WordChar)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(10)
        .margin_end(10)
        .build();
    let scroll_src = gtk4::ScrolledWindow::builder()
        .min_content_height(130)
        .min_content_width(120)
        .vexpand(true)
        .child(&source_view)
        .build();
    let result_view = gtk4::TextView::builder()
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(10)
        .margin_end(10)
        .build();
    let scroll_res = gtk4::ScrolledWindow::builder()
        .min_content_height(130)
        .min_content_width(120)
        .vexpand(true)
        .child(&result_view)
        .build();

    let status = gtk4::Label::builder()
        .label("Ready")
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    let spinner = gtk4::Spinner::new();

    let controls: Vec<gtk4::Widget> = vec![
        source_lang.widget.clone().upcast(),
        target_lang.widget.clone().upcast(),
        btn_swap.clone().upcast(),
        btn_clear.clone().upcast(),
        btn_translate.clone().upcast(),
        btn_screenshot.clone().upcast(),
        btn_copy.clone().upcast(),
    ];
    let ocr_extra = vec![btn_screenshot.clone()];

    let header = libadwaita::HeaderBar::builder().build();
    let title_label = gtk4::Label::new(Some("kosuzu"));
    header.set_title_widget(Some(&title_label));
    let menu_btn = gtk4::MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    let menu = gio::Menu::new();
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("Quit"), Some("app.quit"));
    let settings_pop = gtk4::PopoverMenu::from_model(Some(&menu));
    menu_btn.set_popover(Some(&settings_pop));
    header.pack_end(&menu_btn);
    header.pack_start(&btn_expand);
    header.pack_start(&btn_lock);

    let outer = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(8)
        .margin_top(10)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();

    let lang_row = gtk4::Box::builder().spacing(4).build();
    lang_row.set_halign(gtk4::Align::Center);
    source_lang.widget.set_halign(gtk4::Align::Center);
    target_lang.widget.set_halign(gtk4::Align::Center);
    btn_swap.set_halign(gtk4::Align::Center);
    lang_row.append(&source_lang.widget);
    lang_row.append(&btn_swap);
    lang_row.append(&target_lang.widget);

    let bottom_lang_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    bottom_lang_row.set_halign(gtk4::Align::Center);
    bottom_lang_row.append(&lang_row);

    let row_text = gtk4::Box::builder().spacing(6).build();
    row_text.set_margin_start(8);
    row_text.set_margin_end(8);
    row_text.set_margin_bottom(8);
    row_text.append(&btn_screenshot);
    row_text.append(&btn_clear);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row_text.append(&spacer);
    let counter = gtk4::Label::new(Some("0 / 2000"));
    counter.add_css_class("dim-label");
    row_text.append(&counter);
    row_text.append(&btn_translate);
    {
        let c2 = counter.clone();
        source_view.buffer().connect_changed(move |b| {
            let n = b.text(&b.start_iter(), &b.end_iter(), true).chars().count();
            c2.set_label(&format!("{n} / 2000"));
        });
    }

    let src_col = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    src_col.append(&scroll_src);
    src_col.append(&row_text);
    let frame_src = gtk4::Frame::builder().child(&src_col).build();
    frame_src.set_hexpand(true);
    frame_src.set_vexpand(true);

    let res_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    res_row.set_margin_start(8);
    res_row.set_margin_end(8);
    res_row.set_margin_bottom(8);
    let res_spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    res_spacer.set_hexpand(true);
    res_row.append(&res_spacer);
    res_row.append(&btn_copy);
    let res_col = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    res_col.append(&scroll_res);
    res_col.append(&res_row);
    let frame_res = gtk4::Frame::builder().child(&res_col).build();
    frame_res.set_hexpand(true);
    frame_res.set_vexpand(true);

    let cards = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(8)
        .vexpand(true)
        .build();
    cards.append(&frame_src);
    cards.append(&frame_res);
    outer.append(&cards);

    outer.append(&bottom_lang_row);

    let footer = gtk4::Box::builder().spacing(8).build();
    footer.append(&status);
    footer.append(&spinner);
    outer.append(&footer);


    let w = W {
        source_lang,
        target_lang,
        source_view,
        result_view,
        scroll_src,
        scroll_res,
        row_text,
        cards,
        header,
        title_label,
        lang_row,
        bottom_lang_row,
        ocr_extra,
        btn_swap,
        btn_clear,
        btn_translate,
        btn_screenshot,
        btn_copy,
        btn_expand,
        btn_lock,
        status,
        spinner,
        footer,
        controls,
    };
    (w, outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn niri_focus_requires_matching_pid_and_flag() {
        let wins = r#"
        [{"id":1,"pid":100,"is_focused":false,"title":"a"},
         {"id":2,"pid":4242,"is_focused":true,"title":"b"},
         {"id":3,"pid":4242,"is_focused":false,"title":"c"}]"#;
        assert!(niri_windows_claim_focus(wins, 4242));
        assert!(!niri_windows_claim_focus(wins, 100), "pid has windows but none focused");
        assert!(!niri_windows_claim_focus(wins, 999), "no windows for pid");
        assert!(!niri_windows_claim_focus("not json", 4242), "bad JSON counts as not found");
        assert!(!niri_windows_claim_focus("[]", 4242));
        assert!(
            !niri_windows_claim_focus(r#"[{"pid":4242}]"#, 4242),
            "missing is_focused counts as unfocused"
        );
    }

    fn line(text: &str, y: i32) -> OcrLine {
        OcrLine {
            poly: [[10, y], [100, y], [100, y + 20], [10, y + 20]],
            text: text.to_string(),
            score: 1.0,
        }
    }

    #[test]
    fn wrapped_english_words_get_a_space() {
        let lines = vec![line("transla", 0), line("te the text", 8)];
        assert_eq!(group_paragraphs(&lines), "transla te the text");
    }

    #[test]
    fn cjk_lines_stay_glued() {
        let lines = vec![line("こん", 0), line("にちは", 8)];
        assert_eq!(group_paragraphs(&lines), "こんにちは");
    }

    #[test]
    fn paragraphs_split_on_newline() {
        let lines = vec![line("first para", 0), line("second para", 40)];
        assert_eq!(group_paragraphs(&lines), "first para\nsecond para");
    }
}
