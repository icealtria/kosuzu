//! Settings dialog backed by libadwaita, mirroring Dialect's preferences:
//! AdwPreferencesDialog with pages/groups/rows, opened over the main window.

use crate::config::Config;
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
    dialog.present(Some(parent));
}
