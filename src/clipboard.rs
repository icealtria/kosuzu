//! GDK clipboard read/write, callback-based.
//!
//! Errors collapse to `None`. Cold-start retries live in main_window.

use gtk4::{gdk, prelude::*};

pub fn read_primary_text_async(cb: impl FnOnce(Option<String>) + 'static) {
    match gdk::Display::default() {
        None => cb(None),
        Some(display) => {
            display.primary_clipboard().read_text_async(
                None::<&gtk4::gio::Cancellable>,
                move |res| cb(res.ok().flatten().map(|s| s.to_string())),
            );
        }
    }
}

pub fn read_clipboard_text_async(cb: impl FnOnce(Option<String>) + 'static) {
    match gdk::Display::default() {
        None => cb(None),
        Some(display) => display.clipboard().read_text_async(
            None::<&gtk4::gio::Cancellable>,
            move |res| cb(res.ok().flatten().map(|s| s.to_string())),
        ),
    }
}

pub fn read_clipboard_texture_async(cb: impl FnOnce(Option<gdk::Texture>) + 'static) {
    match gdk::Display::default() {
        None => cb(None),
        Some(display) => display.clipboard().read_texture_async(
            None::<&gtk4::gio::Cancellable>,
            move |res| cb(res.ok().flatten()),
        ),
    }
}

pub fn set_clipboard_text(text: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::glib;

    fn pump<T: 'static>(rx: &std::sync::mpsc::Receiver<T>) -> T {
        let ctx = glib::MainContext::default();
        loop {
            if let Ok(v) = rx.try_recv() {
                return v;
            }
            ctx.iteration(true);
        }
    }

    fn read_clipboard_text_blocking() -> Option<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        read_clipboard_text_async(move |v| {
            let _ = tx.send(v);
        });
        pump(&rx)
    }

    fn read_clipboard_texture_blocking() -> Option<gdk::Texture> {
        let (tx, rx) = std::sync::mpsc::channel();
        read_clipboard_texture_async(move |v| {
            let _ = tx.send(v);
        });
        pump(&rx)
    }

    /// set->read round-trip for text and texture. GTK inits once and binds to the
    /// first-calling thread, so all GTK assertions live in one test.
    #[test]
    fn clipboard_roundtrip() {
        gtk4::init().expect("needs a session display (WAYLAND_DISPLAY/DISPLAY)");

        let sample = "kosuzu-roundtrip test text\nsecond line";
        set_clipboard_text(sample);
        assert_eq!(read_clipboard_text_blocking().as_deref(), Some(sample));

        let (w, h) = (4i32, 2i32);
        let rgba: Vec<u8> = (0..(w * h * 4)).map(|i| (i % 251) as u8).collect();
        let tex = gdk::MemoryTexture::new(
            w,
            h,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(rgba.clone()),
            (w * 4) as usize,
        );
        gdk::Display::default().unwrap().clipboard().set_texture(&tex);

        let got = read_clipboard_texture_blocking().expect("must read back the texture just set");
        assert_eq!((got.width(), got.height()), (w, h));

        let png = std::env::temp_dir().join(format!("kosuzu-tex-{}.png", std::process::id()));
        got.save_to_png(&png).expect("save png");
        let img = image::open(&png).unwrap().to_rgba8();
        let _ = std::fs::remove_file(&png);
        assert_eq!(img.dimensions(), (w as u32, h as u32));
        assert_eq!(img.as_raw(), &rgba, "pixels must survive the clipboard round-trip losslessly");
    }
}
