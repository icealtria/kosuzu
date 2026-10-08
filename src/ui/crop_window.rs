//! Region-selection window opened after full-screen backends capture.
//!
//! The picture is centered with ContentFit::Contain; selection coordinates must be
//! mapped back through the same letterbox transform (`contain_transform`/`widget_to_image`).

use gtk4::prelude::*;
use gtk4::{gdk, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

type Done = Rc<RefCell<Option<Box<dyn FnOnce(Option<PathBuf>)>>>>;

/// `on_done(Some(path))` is the cropped result (or the original when the selection is
/// empty/too small); `on_done(None)` means cancelled. The callback fires exactly once.
pub fn open(
    parent: &gtk4::ApplicationWindow,
    texture: &gdk::Texture,
    source_path: PathBuf,
    on_done: impl FnOnce(Option<PathBuf>) + 'static,
) {
    let done: Done = Rc::new(RefCell::new(Some(Box::new(on_done))));
    let img_w = texture.width();
    let img_h = texture.height();

    let win = gtk4::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Select OCR region")
        .build();
    let monitor = gtk4::prelude::WidgetExt::display(parent)
        .monitors()
        .item(0)
        .and_then(|obj| obj.downcast::<gdk::Monitor>().ok());
    if let Some(monitor) = monitor {
        let geo = monitor.geometry();
        win.set_default_size(geo.width() * 4 / 5, geo.height() * 4 / 5);
    } else {
        win.set_default_size(img_w, img_h);
    }

    let picture = gtk4::Picture::new();
    picture.set_paintable(Some(texture));
    picture.set_content_fit(gtk4::ContentFit::Contain);

    let area = gtk4::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);

    let drag_state: Rc<RefCell<DragState>> = Rc::default();
    {
        let state = drag_state.clone();
        let area = area.clone();
        let drag = gtk4::GestureDrag::new();
        drag.connect_drag_begin(move |_, x, y| {
            state.borrow_mut().start = Some((x, y));
        });
        let state = drag_state.clone();
        let area_upd = area.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let mut s = state.borrow_mut();
            if let Some((sx, sy)) = s.start {
                s.rect = Some((sx, sy, sx + dx, sy + dy));
            }
            drop(s);
            area_upd.queue_draw();
        });
        let state = drag_state.clone();
        let area_end = area.clone();
        drag.connect_drag_end(move |_, dx, dy| {
            let mut s = state.borrow_mut();
            match s.start.take() {
                Some(_) if dx.abs() < 3.0 && dy.abs() < 3.0 => s.rect = None, // a tap clears the selection
                Some((sx, sy)) => s.rect = Some((sx, sy, sx + dx, sy + dy)),
                None => {}
            }
            drop(s);
            area_end.queue_draw();
        });
        area.add_controller(drag);
    }

    {
        let state = drag_state.clone();
        area.set_draw_func(move |_, cr, w, h| draw(cr, w, h, &state.borrow()));
    }

    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&area);

    let hint = gtk4::Label::builder()
        .label("Drag to select a region; without a selection the whole image is recognized")
        .margin_start(10)
        .margin_end(10)
        .build();
    hint.add_css_class("dim-label");

    let cancel = gtk4::Button::with_label("Cancel");
    let confirm = gtk4::Button::with_label("OK");
    confirm.add_css_class("suggested-action");
    {
        let done = done.clone();
        let win = win.clone();
        cancel.connect_clicked(move |_| finish(&done, None, &win));
    }
    {
        let done = done.clone();
        let win = win.clone();
        let state = drag_state.clone();
        confirm.connect_clicked(move |_| {
            let path = match state.borrow().rect {
                None => source_path.clone(),
                Some(rect) => {
                    let transform = contain_transform(
                        area.width(),
                        area.height(),
                        img_w,
                        img_h,
                    );
                    let (x0, y0, x1, y1) = widget_to_image(rect, transform, img_w, img_h);
                    if x1 - x0 < 4 || y1 - y0 < 4 {
                        source_path.clone() // selection too small: recognize the whole image
                    } else {
                        crop_to_temp(&source_path, x0, y0, x1, y1)
                            .unwrap_or_else(|| source_path.clone())
                    }
                }
            };
            finish(&done, Some(path), &win);
        });
    }

    let buttons = gtk4::Box::builder()
        .spacing(8)
        .margin_top(6)
        .margin_bottom(10)
        .margin_start(10)
        .margin_end(10)
        .build();
    buttons.append(&hint);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    buttons.append(&spacer);
    buttons.append(&cancel);
    buttons.append(&confirm);

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.append(&overlay);
    outer.append(&buttons);
    win.set_child(Some(&outer));

    {
        let done = done.clone();
        win.connect_close_request(move |_| {
            if let Some(cb) = done.borrow_mut().take() {
                cb(None);
            }
            glib::Propagation::Proceed // let GTK close normally
        });
    }

    win.present();
}

#[derive(Default)]
struct DragState {
    start: Option<(f64, f64)>,
    rect: Option<(f64, f64, f64, f64)>,
}

fn finish(done: &Done, path: Option<PathBuf>, win: &gtk4::Window) {
    if let Some(cb) = done.borrow_mut().take() {
        cb(path);
    }
    win.close();
}

/// must mirror ContentFit::Contain letterboxing, else the selection maps to wrong pixels.
fn contain_transform(aw: i32, ah: i32, iw: i32, ih: i32) -> (f64, f64, f64) {
    let aw = aw.max(1) as f64;
    let ah = ah.max(1) as f64;
    let scale = (aw / iw as f64).min(ah / ih as f64);
    (scale, (aw - iw as f64 * scale) / 2.0, (ah - ih as f64 * scale) / 2.0)
}

/// widget-space selection -> integer pixel rect in the original image, clamped.
fn widget_to_image(
    rect: (f64, f64, f64, f64),
    (scale, off_x, off_y): (f64, f64, f64),
    iw: i32,
    ih: i32,
) -> (i32, i32, i32, i32) {
    let (x0, y0, x1, y1) = rect;
    let clamp = |v: f64, hi: i32| v.max(0.0).min(hi as f64) as i32;
    (
        clamp((x0.min(x1) - off_x) / scale, iw),
        clamp((y0.min(y1) - off_y) / scale, ih),
        clamp((x0.max(x1) - off_x) / scale, iw),
        clamp((y0.max(y1) - off_y) / scale, ih),
    )
}

/// crop the original into a temp file; any failure returns None (caller falls back to the whole image).
fn crop_to_temp(source: &Path, x0: i32, y0: i32, x1: i32, y1: i32) -> Option<PathBuf> {
    let img = image::open(source).ok()?;
    let cropped = image::imageops::crop_imm(
        &img,
        x0 as u32,
        y0 as u32,
        (x1 - x0) as u32,
        (y1 - y0) as u32,
    )
    .to_image();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dst = std::env::temp_dir().join(format!("kosuzu-crop-{}-{stamp}.png", std::process::id()));
    cropped.save(&dst).ok()?;
    Some(dst)
}

fn draw(cr: &gtk4::cairo::Context, width: i32, height: i32, state: &DragState) {
    let _ = cr.save();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
    match state.rect {
        None => {
            cr.rectangle(0.0, 0.0, width as f64, height as f64);
            let _ = cr.fill();
        }
        Some(rect) => {
            let (x0, y0, x1, y1) = clamp_rect(rect, width, height);
            for (rx, ry, rw, rh) in [
                (0.0, 0.0, width as f64, y0),
                (0.0, y1, width as f64, height as f64 - y1),
                (0.0, y0, x0, y1 - y0),
                (x1, y0, width as f64 - x1, y1 - y0),
            ] {
                if rw > 0.0 && rh > 0.0 {
                    cr.rectangle(rx, ry, rw, rh);
                    let _ = cr.fill();
                }
            }
            cr.set_source_rgba(0.2, 0.6, 1.0, 1.0);
            cr.set_line_width(2.0);
            cr.rectangle(
                x0 + 1.0,
                y0 + 1.0,
                (x1 - x0 - 2.0).max(1.0),
                (y1 - y0 - 2.0).max(1.0),
            );
            let _ = cr.stroke();
        }
    }
    let _ = cr.restore();
}

/// normalize the selection to (0,0)-(w,h) and sort the corners.
fn clamp_rect(
    rect: (f64, f64, f64, f64),
    width: i32,
    height: i32,
) -> (f64, f64, f64, f64) {
    let (x0, y0, x1, y1) = rect;
    let (x0, x1) = order_clamp(x0, x1, width as f64);
    let (y0, y1) = order_clamp(y0, y1, height as f64);
    (x0, y0, x1, y1)
}

fn order_clamp(a: f64, b: f64, bound: f64) -> (f64, f64) {
    let (a, b) = (a.max(0.0), b.min(bound));
    (a.min(b), a.max(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contain_letterboxes_horizontally() {
        // 1000x500 into 800x800 area: width-limited, scale=0.8, vertical pillarbox 100
        let (scale, off_x, off_y) = contain_transform(800, 800, 1000, 500);
        assert_eq!(scale, 0.8);
        assert_eq!(off_x, 0.0);
        assert_eq!(off_y, 200.0);
    }

    #[test]
    fn contain_letterboxes_vertically() {
        // 500x1000 into 800x800: height-limited, scale=0.8, horizontal pillarbox 200
        let (scale, off_x, off_y) = contain_transform(800, 800, 500, 1000);
        assert_eq!(scale, 0.8);
        assert_eq!(off_x, 200.0);
        assert_eq!(off_y, 0.0);
    }

    #[test]
    fn widget_rect_maps_through_letterbox() {
        // first scenario: (0,200)-(800,600) in the 800x800 area is the whole image
        let t = contain_transform(800, 800, 1000, 500);
        assert_eq!(
            widget_to_image((0.0, 200.0, 800.0, 600.0), t, 1000, 500),
            (0, 0, 1000, 500)
        );
        // left half of the image: (0,0)-(500,500) in image space -> (0,200)-(400,600)
        assert_eq!(
            widget_to_image((0.0, 200.0, 400.0, 600.0), t, 1000, 500),
            (0, 0, 500, 500)
        );
    }

    #[test]
    fn widget_rect_clamps_and_orders() {
        let t = contain_transform(800, 800, 1000, 500);
        // reversed drag (x0>x1) and out-of-bounds both normalize to a positive rect
        assert_eq!(
            widget_to_image((800.0, 600.0, 0.0, 200.0), t, 1000, 500),
            (0, 0, 1000, 500)
        );
        assert_eq!(
            widget_to_image((-50.0, 100.0, 900.0, 900.0), t, 1000, 500),
            (0, 0, 1000, 500)
        );
    }

    #[test]
    fn clamp_rect_orders_reversed_selection() {
        assert_eq!(
            clamp_rect((600.0, 500.0, 100.0, 50.0), 800, 800),
            (100.0, 50.0, 600.0, 500.0)
        );
        // clamp, then sort
        assert_eq!(
            clamp_rect((-20.0, -10.0, 2000.0, 3000.0), 800, 600),
            (0.0, 0.0, 800.0, 600.0)
        );
    }
}
