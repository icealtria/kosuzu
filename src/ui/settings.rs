//! Settings dialog backed by libadwaita, mirroring Dialect's preferences:
//! AdwPreferencesDialog with pages/groups/rows, opened over the main window.

use crate::config::Config;
use crate::llm::{self, LlmTranslator};
use gtk4::glib;
use kosuzu_ocr::models_dir;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) fn open(
    parent: &gtk4::ApplicationWindow,
    config: Rc<RefCell<Config>>,
    on_changed: Rc<dyn Fn()>,
) {
    let dialog = libadwaita::PreferencesDialog::builder()
        .title("Preferences")
        .build();

    let page = libadwaita::PreferencesPage::builder()
        .title("Settings")
        .build();

    // ---- OCR ----
    let ocr_group = libadwaita::PreferencesGroup::new();
    ocr_group.set_title("OCR");

    let ocr_row = libadwaita::ComboRow::builder()
        .title("OCR model")
        .subtitle("Smaller is faster; larger recognizes denser text.")
        .build();
    let models = gtk4::StringList::new(&["tiny", "small", "medium"]);
    ocr_row.set_model(Some(&models));
    {
        let cfg = config.borrow();
        ocr_row.set_selected(
            ["tiny", "small", "medium"]
                .iter()
                .position(|&p| p == cfg.ocr_model)
                .unwrap_or(1) as u32,
        );
    }
    {
        let cfg2 = config.clone();
        ocr_row.connect_selected_notify(move |r| {
            let presets = ["tiny", "small", "medium"];
            let mut c = cfg2.borrow_mut();
            c.ocr_model = presets[r.selected() as usize].into();
            drop(c);
            cfg2.borrow().save();
        });
    }
    ocr_group.add(&ocr_row);

    let folder_row = libadwaita::ActionRow::builder()
        .title("OCR model files")
        .subtitle("Models are cached locally after first use. Click to open the folder.")
        .build();
    folder_row.set_activatable(true);
    folder_row.connect_activated(|_| {
        let dir = models_dir();
        let _ = std::fs::create_dir_all(&dir);
        let launcher = gtk4::FileLauncher::new(Some(&gtk4::gio::File::for_path(dir)));
        launcher.launch(
            None::<&gtk4::Window>,
            None::<&gtk4::gio::Cancellable>,
            |_| {},
        );
    });
    ocr_group.add(&folder_row);
    page.add(&ocr_group);

    let parent = parent.clone();

    // ---- Providers ----
    let providers = libadwaita::PreferencesGroup::new();
    providers.set_title("Providers");

    let engine_row = libadwaita::ComboRow::builder()
        .title("Translator")
        .subtitle("Choose from the available translation services.")
        .build();
    let engines =
        gtk4::StringList::new(&["Google", "OpenAI-compatible"]);
    engine_row.set_model(Some(&engines));
    {
        let cfg = config.borrow();
        engine_row.set_selected(if cfg.translate_engine == "llm" { 1 } else { 0 });
    }
    // the gear opens the selected provider's config; google has none
    let gear = gtk4::Button::from_icon_name("emblem-system-symbolic");
    gear.add_css_class("flat");
    gear.set_valign(gtk4::Align::Center);
    gear.set_tooltip_text(Some("Configure this provider"));
    gear.set_sensitive(config.borrow().translate_engine == "llm");
    {
        let gear_parent = parent.clone();
        let cfg2 = config.clone();
        let on_ch = on_changed.clone();
        gear.connect_clicked(move |_| {
            open_llm_settings(&gear_parent, cfg2.clone(), on_ch.clone());
        });
    }
    {
        let cfg2 = config.clone();
        let on_ch = on_changed.clone();
        let gear2 = gear.clone();
        engine_row.connect_selected_notify(move |row| {
            let engine = if row.selected() == 1 { "llm" } else { "google" };
            cfg2.borrow_mut().translate_engine = engine.into();
            cfg2.borrow().save();
            gear2.set_sensitive(engine == "llm");
            on_ch();
        });
    }
    engine_row.add_suffix(&gear);
    providers.add(&engine_row);
    page.add(&providers);

    // ---- Behavior ----
    let behavior = libadwaita::PreferencesGroup::new();
    behavior.set_title("Behavior");

    let auto_row = libadwaita::SwitchRow::builder()
        .title("Auto-translate after OCR")
        .subtitle("Run translation right after the screenshot is recognized.")
        .build();
    auto_row.set_active(config.borrow().auto_translate_after_ocr);
    {
        let cfg2 = config.clone();
        auto_row.connect_active_notify(move |r| {
            let mut c = cfg2.borrow_mut();
            c.auto_translate_after_ocr = r.is_active();
            drop(c);
            cfg2.borrow().save();
        });
    }
    behavior.add(&auto_row);

    let lock_row = libadwaita::SwitchRow::builder()
        .title("Pin popup")
        .subtitle("Keep the popup open even when unfocused. Off: popup hides when it loses focus.")
        .build();
    lock_row.set_active(config.borrow().popup_lock);
    {
        let cfg2 = config.clone();
        lock_row.connect_active_notify(move |r| {
            let mut c = cfg2.borrow_mut();
            c.popup_lock = r.is_active();
            drop(c);
            cfg2.borrow().save();
        });
    }
    behavior.add(&lock_row);

    let input_row = libadwaita::SwitchRow::builder()
        .title("Show input in popup")
        .subtitle("Off: the popup only shows the translation.")
        .build();
    input_row.set_active(config.borrow().popup_show_input);
    {
        let cfg2 = config.clone();
        let on_ch = on_changed.clone();
        input_row.connect_active_notify(move |r| {
            let mut c = cfg2.borrow_mut();
            c.popup_show_input = r.is_active();
            drop(c);
            cfg2.borrow().save();
            on_ch();
        });
    }
    behavior.add(&input_row);


    let status_row = libadwaita::SwitchRow::builder()
        .title("Show status bar")
        .subtitle("The bottom line with the status / spinner messages.")
        .build();
    status_row.set_active(config.borrow().show_status_bar);
    {
        let cfg2 = config.clone();
        let on_ch = on_changed.clone();
        status_row.connect_active_notify(move |r| {
            let mut c = cfg2.borrow_mut();
            c.show_status_bar = r.is_active();
            drop(c);
            cfg2.borrow().save();
            on_ch();
        });
    }
    behavior.add(&status_row);
    page.add(&behavior);

    dialog.add(&page);
    dialog.present(Some(&parent));
}

/// The LLM provider's own page, opened from the Providers gear. Fields write
/// config as you type, so there is nothing to remember to save.
fn open_llm_settings(
    parent: &gtk4::ApplicationWindow,
    config: Rc<RefCell<Config>>,
    on_changed: Rc<dyn Fn()>,
) {
    let dialog = libadwaita::PreferencesDialog::builder()
        .title("LLM translation")
        .build();
    let page = libadwaita::PreferencesPage::builder().title("Provider").build();
    let group = libadwaita::PreferencesGroup::new();
    group.set_title("OpenAI-compatible endpoint");
    group.set_description(Some(
        "Any service that speaks /chat/completions: OpenAI, DeepSeek, \
         OpenRouter, or a local Ollama.",
    ));

    let base_row = libadwaita::EntryRow::builder().title("Base URL").build();
    base_row.set_text(&config.borrow().llm_base_url);
    let model_row = libadwaita::EntryRow::builder().title("Model").build();
    model_row.set_text(&config.borrow().llm_model);
    let key_row = libadwaita::PasswordEntryRow::builder()
        .title("API key")
        .build();
    key_row.set_text(&config.borrow().llm_api_key);

    {
        let cfg = config.clone();
        let on_ch = on_changed.clone();
        base_row.connect_changed(move |row| {
            cfg.borrow_mut().llm_base_url = row.text().to_string();
            cfg.borrow().save();
            on_ch();
        });
    }
    {
        let cfg = config.clone();
        let on_ch = on_changed.clone();
        model_row.connect_changed(move |row| {
            cfg.borrow_mut().llm_model = row.text().to_string();
            cfg.borrow().save();
            on_ch();
        });
    }
    {
        let cfg = config.clone();
        let on_ch = on_changed.clone();
        key_row.connect_changed(move |row| {
            cfg.borrow_mut().llm_api_key = row.text().to_string();
            cfg.borrow().save();
            on_ch();
        });
    }

    group.add(&base_row);
    group.add(&model_row);
    group.add(&key_row);

    let test_row = libadwaita::ActionRow::builder()
        .title("Test")
        .subtitle("Send a sample translation with the settings above.")
        .build();
    let test_btn = gtk4::Button::with_label("Test");
    test_btn.add_css_class("flat");
    test_btn.set_valign(gtk4::Align::Center);
    test_row.add_suffix(&test_btn);
    {
        let cfg = config.clone();
        let base_row2 = base_row.clone();
        let model_row2 = model_row.clone();
        let test_row2 = test_row.clone();
        let btn2 = test_btn.clone();
        test_btn.connect_clicked(move |_| {
            btn2.set_sensitive(false);
            test_row2.set_subtitle("Testing…");
            let target = cfg.borrow().target_lang.clone();
            let (base, model, key) = (
                base_row2.text().to_string(),
                model_row2.text().to_string(),
                key_row.text().to_string(),
            );
            let (sender, receiver) = std::sync::mpsc::channel::<(Option<String>, String)>();
            std::thread::spawn(move || {
                let now = std::time::Instant::now();
                let mut sample = String::new();
                let outcome = LlmTranslator::new(&base, &model, &key).translate(
                    "Hello, world!",
                    "en",
                    &target,
                    &mut |chunk| sample.push_str(chunk),
                );
                // only a passing test tidies the url: a failure could be the key
                let tidied = match &outcome {
                    Ok(()) => Some(llm::normalize_base_url(&base))
                        .filter(|fixed| !fixed.is_empty() && *fixed != base),
                    Err(_) => None,
                };
                let message = match outcome {
                    Ok(()) => {
                        format!("OK in {:.1}s: {sample}", now.elapsed().as_secs_f32())
                    }
                    Err(e) => format!("Failed: {e}"),
                };
                let _ = sender.send((tidied, message));
            });
            let base_row3 = base_row.clone();
            let test_row3 = test_row2.clone();
            let btn3 = btn2.clone();
            glib::source::idle_add_local(move || {
                match receiver.try_recv() {
                    Ok((tidied, message)) => {
                        test_row3.set_subtitle(&message);
                        if let Some(fixed) = tidied {
                            // connect_changed writes it to config and rebuilds
                            base_row3.set_text(&fixed);
                        }
                        btn3.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                    Err(_) => glib::ControlFlow::Continue,
                }
            });
        });
    }
    group.add(&test_row);
    page.add(&group);
    dialog.add(&page);
    dialog.present(Some(parent));
}
