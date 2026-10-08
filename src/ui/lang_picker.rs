//! Searchable language picker, Google Translate style: a MenuButton showing the
//! current choice that opens a popover with a search entry over the full list.

use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) struct LangPicker {
    pub widget: gtk4::MenuButton,
    code: Rc<RefCell<String>>,
    on_change: Rc<RefCell<Option<Box<dyn Fn()>>>>,
}

impl LangPicker {
    pub(crate) fn new(languages: &'static [(&'static str, &'static str)], initial: &str) -> Self {
        let code = Rc::new(RefCell::new(initial.to_string()));
        let on_change: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let current_name = languages
            .iter()
            .find(|(c, _)| *c == initial)
            .map(|(_, n)| *n)
            .unwrap_or(initial);
        let widget = gtk4::MenuButton::builder().label(current_name).build();
        widget.add_css_class("lang-pill");
        widget.set_size_request(220, -1);
        widget.set_can_shrink(true);
        fn find_label(w: gtk4::Widget) -> Option<gtk4::Label> {
            if let Some(l) = w.downcast_ref::<gtk4::Label>() {
                return Some(l.clone());
            }
            let mut c = w.first_child();
            while let Some(x) = c {
                if let Some(l) = find_label(x.clone()) {
                    return Some(l);
                }
                c = x.next_sibling();
            }
            None
        }
        if let Some(label) = find_label(widget.clone().upcast()) {
            label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            label.set_max_width_chars(20);
            label.set_width_chars(20);
            label.set_xalign(0.5);
            label.set_hexpand(true);
        }

        let search = gtk4::SearchEntry::builder()
            .placeholder_text("Search languages…")
            .build();
        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::None);
        for (_, name) in languages {
            let row = gtk4::ListBoxRow::new();
            let label = gtk4::Label::builder()
                .label(*name)
                .xalign(0.0)
                .margin_start(8)
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
            row.set_child(Some(&label));
            row.set_widget_name(name);
            list.append(&row);
        }
        let scrolled = gtk4::ScrolledWindow::builder()
            .min_content_width(240)
            .min_content_height(300)
            .child(&list)
            .build();
        let col = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        col.append(&search);
        col.append(&scrolled);
        let popover = gtk4::Popover::new();
        popover.set_child(Some(&col));
        widget.set_popover(Some(&popover));

        let query = Rc::new(RefCell::new(String::new()));
        let q = query.clone();
        let list2 = list.clone();
        search.connect_search_changed(move |e| {
            *q.borrow_mut() = e.text().to_lowercase();
            list2.invalidate_filter();
        });
        let q = query.clone();
        let list2 = list.clone();
        let search2 = search.clone();
        popover.connect_map(move |_| {
            search2.set_text("");
            list2.invalidate_filter();
        });
        list.set_filter_func(move |row| {
            row.widget_name().to_lowercase().contains(q.borrow().as_str())
        });

        let code2 = code.clone();
        let oc2 = on_change.clone();
        let w = widget.clone();
        let p = popover.clone();
        list.connect_row_activated(move |_, row| {
            let name = row.widget_name();
            let c = languages
                .iter()
                .find(|(_, ln)| *ln == name.as_str())
                .map(|(lc, _)| *lc)
                .unwrap_or(name.as_str());
            *code2.borrow_mut() = c.to_string();
            w.set_label(&name);
            p.popdown();
            if let Some(cb) = oc2.borrow().as_ref() {
                cb();
            }
        });

        Self { widget, code, on_change }
    }

    pub(crate) fn code(&self) -> String {
        self.code.borrow().clone()
    }

    pub(crate) fn set_code(&self, code: &str, languages: &'static [(&'static str, &'static str)]) {
        let name = languages
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, n)| *n)
            .unwrap_or(code);
        *self.code.borrow_mut() = code.to_string();
        self.widget.set_label(name);
    }

    pub(crate) fn connect_changed(&self, cb: impl Fn() + 'static) {
        *self.on_change.borrow_mut() = Some(Box::new(cb));
    }
}
