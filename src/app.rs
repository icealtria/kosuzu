//! Gtk.Application single-instance skeleton.
//!
//! With a tray: closing the window only hides it (process and model stay resident).
//! Without one: closing destroys the window and exits.

use crate::args::Args;
use crate::tray::Tray;
use crate::ui::main_window::MainWindow;
use clap::Parser;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

struct AppState {
    window: Option<MainWindow>,
    tray: Option<Tray>,
}

pub fn run() -> glib::ExitCode {
    let app = libadwaita::Application::new(
        Some("dev.kosuzu.Kosuzu"),
        gio::ApplicationFlags::HANDLES_COMMAND_LINE,
    );
    let state = Rc::new(RefCell::new(AppState {
        window: None,
        tray: None,
    }));
    {
        let state = state.clone();
        app.connect_startup(move |app| {
            let _ = libadwaita::init();
            {
                let quit = gio::SimpleAction::new("quit", None::<&glib::VariantTy>);
                let app2 = app.clone();
                quit.connect_activate(move |_, _| app2.quit());
                app.add_action(&quit);
            }
            let tray = Tray::start(
                tray_action(app, &state, Args::default()),
                tray_action(
                    app,
                    &state,
                    Args {
                        selection: true,
                        ..Args::default()
                    },
                ),
                tray_action(
                    app,
                    &state,
                    Args {
                        screenshot_ocr: true,
                        ..Args::default()
                    },
                ),
                {
                    let app = app.clone();
                    move || app.quit()
                },
            );
            state.borrow_mut().tray = tray;
        });
    }
    {
        let state = state.clone();
        app.connect_command_line(move |app, cmdline| {
            let Ok(args) = Args::try_parse_from(cmdline.arguments()) else {
                return glib::ExitCode::FAILURE;
            };
            dispatch(app, &state, &args)
        });
    }
    app.run_with_args(&std::env::args().collect::<Vec<_>>())
}

fn dispatch(app: &libadwaita::Application, state: &Rc<RefCell<AppState>>, args: &Args) -> glib::ExitCode {
    if args.quit {
        app.quit();
        return glib::ExitCode::SUCCESS;
    }

    let action_given = args.selection
        || args.screenshot_ocr
        || args.clipboard_ocr
        || args.image.is_some()
        || args.text.is_some();
    // screenshot actions must not present first (the window would cover the screen)
    let screenshot_first = args.screenshot_ocr && !args.selection;
    let window = window_for_action(app, state, action_given, !screenshot_first);

    if args.selection {
        window.cmd_start_selection();
    } else if args.screenshot_ocr {
        window.start_screenshot();
    } else if args.clipboard_ocr {
        window.cmd_clipboard_ocr();
    } else if let Some(path) = args.image.as_deref() {
        window.ocr_image(std::path::Path::new(path));
    } else if let Some(text) = args.text.as_deref() {
        window.cmd_translate_text(text);
    }
    glib::ExitCode::SUCCESS
}

/// hotkey/CLI actions switch to the compact popup when the window is hidden; a bare launch restores the full editor.
fn window_for_action(
    app: &libadwaita::Application,
    state: &Rc<RefCell<AppState>>,
    action: bool,
    present: bool,
) -> MainWindow {
    if state.borrow().window.is_none() {
        let window = create_window(app, state);
        state.borrow_mut().window = Some(window);
    }
    let window = state.borrow().window.clone().expect("window just created or exists");
    if action {
        if !window.win.is_visible() {
            window.set_popup_mode(true);
        }
    } else {
        window.set_popup_mode(false);
    }
    if present {
        window.win.present();
    }
    window
}

/// tray actions go through the same `dispatch` as the CLI.
fn tray_action(app: &libadwaita::Application, state: &Rc<RefCell<AppState>>, args: Args) -> impl Fn() {
    let app = app.clone();
    let state = Rc::downgrade(state);
    move || {
        let Some(state) = state.upgrade() else {
            return;
        };
        dispatch(&app, &state, &args);
    }
}

fn create_window(app: &libadwaita::Application, state: &Rc<RefCell<AppState>>) -> MainWindow {
    let window = MainWindow::new(app);
    {
        let state = state.clone();
        let win = window.win.clone();
        window.win.connect_close_request(move |_| {
            if state.borrow().tray.as_ref().is_some_and(|t| t.connected()) {
                // with a tray: hide on close, keep the process (and loaded model) resident
                win.set_visible(false);
                return glib::Propagation::Stop;
            }
            // without a tray: destroy on close
            state.borrow_mut().window = None;
            glib::Propagation::Proceed
        });
    }
    {
        let state = state.clone();
    }
    window
}
