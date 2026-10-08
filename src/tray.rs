//! System tray: hand-rolled StatusNotifierItem + com.canonical.dbusmenu.
//!
//! Why hand-rolled: GTK4/Wayland has no tray API (XEmbed is X11-only), and letting
//! the host render the menu sidesteps Wayland popup-positioning limits.
//!
//! Protocol facts (AyatanaIndicators/libdbusmenu, verified against real hosts):
//! - SNI name `org.kde.StatusNotifierItem-<pid>-1`, object /StatusNotifierItem,
//!   registered via org.kde.StatusNotifierWatcher.RegisterStatusNotifierItem;
//!   the host may come up later, so we watch for it.
//! - dbusmenu layout is recursive (ia{sv}av), int32 ids, root id=0.
//! - IconPixmap is a(iiay): 4 bytes per pixel, network order, non-premultiplied ARGB.
//! - Properties/Introspectable are handled by GDBus from the registered XML.

use anyhow::Context;
use gtk4::glib::prelude::*;
use gtk4::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;

use crate::config::data_dir;

const SNI_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const SNI_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";
/// static menu revision; the menu never changes
const REVISION: u32 = 1;

const SNI_XML: &str = r#"<node>
  <interface name="org.kde.StatusNotifierItem">
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
    <property name="OverlayIconName" type="s" access="read"/>
    <property name="AttentionIconName" type="s" access="read"/>
    <property name="IconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
    <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
    <property name="Menu" type="o" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <method name="ContextMenu">
      <arg type="i" name="x" direction="in"/>
      <arg type="i" name="y" direction="in"/>
    </method>
    <method name="Activate">
      <arg type="i" name="x" direction="in"/>
      <arg type="i" name="y" direction="in"/>
    </method>
    <method name="SecondaryActivate">
      <arg type="i" name="x" direction="in"/>
      <arg type="i" name="y" direction="in"/>
    </method>
    <method name="Scroll">
      <arg type="i" name="delta" direction="in"/>
      <arg type="s" name="orientation" direction="in"/>
    </method>
    <signal name="NewTitle"/>
    <signal name="NewIcon"/>
    <signal name="NewStatus">
      <arg type="s" name="status"/>
    </signal>
  </interface>
</node>"#;

const MENU_XML: &str = r#"<node>
  <interface name="com.canonical.dbusmenu">
    <property name="Version" type="u" access="read"/>
    <property name="TextDirection" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconThemePath" type="as" access="read"/>
    <method name="GetLayout">
      <arg type="i" name="parentId" direction="in"/>
      <arg type="i" name="recursionDepth" direction="in"/>
      <arg type="as" name="propertyNames" direction="in"/>
      <arg type="u" name="revision" direction="out"/>
      <arg type="(ia{sv}av)" name="layout" direction="out"/>
    </method>
    <method name="GetGroupProperties">
      <arg type="ai" name="ids" direction="in"/>
      <arg type="as" name="propertyNames" direction="in"/>
      <arg type="a(ia{sv})" name="properties" direction="out"/>
    </method>
    <method name="GetProperty">
      <arg type="i" name="id" direction="in"/>
      <arg type="s" name="name" direction="in"/>
      <arg type="v" name="value" direction="out"/>
    </method>
    <method name="Event">
      <arg type="i" name="id" direction="in"/>
      <arg type="s" name="eventId" direction="in"/>
      <arg type="v" name="data" direction="in"/>
      <arg type="u" name="timestamp" direction="in"/>
    </method>
    <method name="EventGroup">
      <arg type="a(isvu)" name="events" direction="in"/>
      <arg type="ai" name="idErrors" direction="out"/>
    </method>
    <method name="AboutToShow">
      <arg type="i" name="id" direction="in"/>
      <arg type="b" name="needUpdate" direction="out"/>
    </method>
    <method name="AboutToShowGroup">
      <arg type="ai" name="ids" direction="in"/>
      <arg type="ai" name="updatesNeeded" direction="out"/>
      <arg type="ai" name="idErrors" direction="out"/>
    </method>
    <signal name="LayoutUpdated">
      <arg type="u" name="revision"/>
      <arg type="i" name="parent"/>
    </signal>
    <signal name="ItemsPropertiesUpdated">
      <arg type="a(ia{sv})" name="updatedProps"/>
      <arg type="a(ias)" name="removedProps"/>
    </signal>
  </interface>
</node>"#;

/// app art: the raster pixels drive the tray pixmap, the scalable source drives theme lookups.
const ICON_PNG: &[u8] = include_bytes!("../assets/kosuzu.png");

const ICON_SVG: &[u8] = include_bytes!("../assets/icon.svg");

struct Actions {
    on_open: Box<dyn Fn()>,
    on_selection: Box<dyn Fn()>,
    on_screenshot: Box<dyn Fn()>,
    on_quit: Box<dyn Fn()>,
}

enum Item {
    Root(&'static [i32]),
    Action(&'static str),
    Separator,
}

/// menu structure.
const MENU: [(i32, Item); 6] = [
    (0, Item::Root(&[1, 2, 3, 4, 5])),
    (1, Item::Action("Open kosuzu")),
    (2, Item::Action("Translate selection")),
    (3, Item::Action("Screenshot OCR translate")),
    (4, Item::Separator),
    (5, Item::Action("Quit")),
];

/// connected = a StatusNotifierWatcher accepted the registration.
pub(crate) struct Tray {
    connected: Rc<Cell<bool>>,
    // keep the registration alive: closing the connection unregisters
    _conn: gio::DBusConnection,
    // signal subscriptions are RAII
    _watch: gio::SignalSubscription,
    // GDBus keeps interface-info pointers for introspection; lifetime must match registration
    _sni_node: gio::DBusNodeInfo,
    _menu_node: gio::DBusNodeInfo,
}

impl Tray {
    pub(crate) fn connected(&self) -> bool {
        self.connected.get()
    }

    /// connect to the bus and subscribe to the host before the main loop runs; None on any failure.
    pub(crate) fn start(
        on_open: impl Fn() + 'static,
        on_selection: impl Fn() + 'static,
        on_screenshot: impl Fn() + 'static,
        on_quit: impl Fn() + 'static,
    ) -> Option<Tray> {
        install_icons();
        let actions = Actions {
            on_open: Box::new(on_open),
            on_selection: Box::new(on_selection),
            on_screenshot: Box::new(on_screenshot),
            on_quit: Box::new(on_quit),
        };
        match try_start(actions) {
            Ok(tray) => Some(tray),
            Err(e) => {
                debug(format!("tray disabled: {e:#}"));
                None
            }
        }
    }
}

fn try_start(actions: Actions) -> anyhow::Result<Tray> {
    let actions = Rc::new(actions);
    let conn = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)
        .context("session bus unavailable")?;
    let sni_node = gio::DBusNodeInfo::for_xml(SNI_XML).context("parse SNI XML")?;
    let menu_node = gio::DBusNodeInfo::for_xml(MENU_XML).context("parse menu XML")?;
    let sni_info = sni_node
        .lookup_interface(SNI_IFACE)
        .context("missing SNI interface")?;
    let menu_info = menu_node
        .lookup_interface(MENU_IFACE)
        .context("missing menu interface")?;
    let connected = Rc::new(Cell::new(false));

    conn.register_object(SNI_PATH, &sni_info)
        .method_call({
            let actions = actions.clone();
            move |_conn, _sender, _path, _iface, method, params, invocation| {
                on_sni_method(&actions, method, &params, invocation)
            }
        })
        .property({
            let props = SniProps::build();
            move |_conn, sender, _path, iface, name| {
                debug(format!(
                    "property {iface}.{name} from {}",
                    sender.unwrap_or("?")
                ));
                props.get(name)
            }
        })
        .build()
        .context("register /StatusNotifierItem")?;

    conn.register_object(MENU_PATH, &menu_info)
        .method_call({
            let actions = actions.clone();
            move |_conn, sender, _path, _iface, method, params, invocation| {
                on_menu_method(&actions, sender, method, &params, invocation)
            }
        })
        .property(move |_conn, _sender, _path, _iface, name| {
            debug(format!("property {name}"));
            menu_property(name)
        })
        .build()
        .context("register /MenuBar")?;

    let service = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "RequestName",
        Some(&(service.as_str(), 0u32).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        None::<&gio::Cancellable>,
    )
    .with_context(|| format!("RequestName {service}"))?;
    debug(format!("name owned: {service}"));

    // subscribe first, then query the initial value: covers both host-early and host-late orderings
    let _watch = conn.subscribe_to_signal(
        None,
        Some("org.freedesktop.DBus"),
        Some("NameOwnerChanged"),
        Some("/org/freedesktop/DBus"),
        Some(WATCHER_NAME),
        gio::DBusSignalFlags::NONE,
        {
            let conn = conn.clone();
            let connected = connected.clone();
            let service = service.clone();
            move |sig| {
                let Some((_name, _old, new)) = sig.parameters.get::<(String, String, String)>()
                else {
                    return;
                };
                if new.is_empty() {
                    debug("watcher vanished");
                    connected.set(false);
                } else {
                    debug(format!("watcher present: {new}"));
                    try_register(&conn, &service, &connected);
                }
            }
        },
    );
    match conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        Some(&glib::Variant::tuple_from_iter([WATCHER_NAME.to_variant()])),
        // body is a (s) tuple; we only check Ok/Err
        None,
        gio::DBusCallFlags::NONE,
        -1,
        None::<&gio::Cancellable>,
    ) {
        Ok(_) => {
            debug("watcher present");
            try_register(&conn, &service, &connected);
        }
        // NameHasNoOwner = host not up yet; wait for NameOwnerChanged
        Err(e) => debug(format!("watcher absent: {e}")),
    }

    Ok(Tray {
        connected,
        _conn: conn,
        _watch,
        _sni_node: sni_node,
        _menu_node: menu_node,
    })
}

fn try_register(conn: &gio::DBusConnection, service: &str, connected: &Rc<Cell<bool>>) {
    let service = service.to_string();
    let connected = connected.clone();
    conn.call(
        Some(WATCHER_NAME),
        WATCHER_PATH,
        WATCHER_NAME,
        "RegisterStatusNotifierItem",
        Some(&glib::Variant::tuple_from_iter([service.to_variant()])),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        None::<&gio::Cancellable>,
        move |res| match res {
            Ok(_) => {
                connected.set(true);
                debug("registered to watcher");
            }
            Err(e) => {
                connected.set(false);
                debug(format!("register failed: {e}"));
            }
        },
    );
}

fn on_sni_method(
    actions: &Actions,
    method: &str,
    params: &glib::Variant,
    invocation: gio::DBusMethodInvocation,
) {
    match method {
        "Activate" | "SecondaryActivate" | "ContextMenu" => {
            let Some((x, y)) = params.get::<(i32, i32)>() else {
                return invalid_args(invocation, method);
            };
            debug(format!("{method}({x},{y})"));
            match method {
                "Activate" => (actions.on_open)(),
                "SecondaryActivate" => (actions.on_selection)(),
                // the host renders the menu itself; we pop no window
                _ => {}
            }
            invocation.return_value(None);
        }
        "Scroll" => {
            let Some((delta, orientation)) = params.get::<(i32, String)>() else {
                return invalid_args(invocation, "Scroll");
            };
            debug(format!("Scroll({delta},{orientation})")); // scroll-to-switch-language intentionally unimplemented
            invocation.return_value(None);
        }
        _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
    }
}

fn on_menu_method(
    actions: &Actions,
    sender: Option<&str>,
    method: &str,
    params: &glib::Variant,
    invocation: gio::DBusMethodInvocation,
) {
    match method {
        "GetLayout" => {
            let Some((parent, depth, _names)) = params.get::<(i32, i32, Vec<String>)>() else {
                return invalid_args(invocation, "GetLayout");
            };
            debug(format!(
                "GetLayout parent={parent} depth={depth} from {}",
                sender.unwrap_or("?")
            ));
            // unknown parent falls back to root; static menu recurses fully regardless of depth
            let parent = if item(parent).is_some() { parent } else { 0 };
            let reply = glib::Variant::tuple_from_iter([REVISION.to_variant(), build_node(parent)]);
            invocation.return_value(Some(&reply));
        }
        "GetGroupProperties" => {
            let Some((ids, _names)) = params.get::<(Vec<i32>, Vec<String>)>() else {
                return invalid_args(invocation, "GetGroupProperties");
            };
            debug(format!("GetGroupProperties {ids:?}"));
            invocation.return_value(Some(&glib::Variant::tuple_from_iter([group_properties(
                &ids,
            )])));
        }
        "GetProperty" => {
            let Some((id, name)) = params.get::<(i32, String)>() else {
                return invalid_args(invocation, "GetProperty");
            };
            let dict = glib::VariantDict::new(Some(&menu_props(id)));
            match dict.lookup_value(&name, None) {
                // single out-params are still tuples
                Some(v) => invocation.return_value(Some(&glib::Variant::tuple_from_iter([
                    glib::Variant::from_variant(&v),
                ]))),
                None => invocation.return_dbus_error(
                    "org.freedesktop.DBus.Error.UnknownProperty",
                    &format!("item {id} has no property {name}"),
                ),
            }
        }
        "Event" => {
            let Some((id, event, _data, _ts)) = params.get::<(i32, String, glib::Variant, u32)>()
            else {
                return invalid_args(invocation, "Event");
            };
            debug(format!("Event id={id} event={event}"));
            handle_event(actions, id, &event);
            invocation.return_value(None);
        }
        "EventGroup" => {
            // single-arg body is still a tuple
            let Some((events,)) = params.get::<(Vec<(i32, String, glib::Variant, u32)>,)>() else {
                return invalid_args(invocation, "EventGroup");
            };
            for (id, event, _data, _ts) in events {
                debug(format!("EventGroup id={id} event={event}"));
                handle_event(actions, id, &event);
            }
            invocation.return_value(Some(&glib::Variant::tuple_from_iter([empty_ai()])));
        }
        "AboutToShow" => {
            // body is the argument tuple
            let Some((id,)) = params.get::<(i32,)>() else {
                return invalid_args(invocation, "AboutToShow");
            };
            debug(format!("AboutToShow {id}"));
            // static menu: cannot change before display
            invocation.return_value(Some(&glib::Variant::tuple_from_iter([false.to_variant()])));
        }
        "AboutToShowGroup" => {
            let Some((ids,)) = params.get::<(Vec<i32>,)>() else {
                return invalid_args(invocation, "AboutToShowGroup");
            };
            debug(format!("AboutToShowGroup {ids:?}"));
            invocation.return_value(Some(&glib::Variant::tuple_from_iter([
                empty_ai(),
                empty_ai(),
            ])));
        }
        _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
    }
}

fn handle_event(actions: &Actions, id: i32, event: &str) {
    if event != "clicked" {
        return;
    }
    match id {
        1 => {
            debug("menu action 1");
            (actions.on_open)();
        }
        2 => {
            debug("menu action 2");
            (actions.on_selection)();
        }
        3 => {
            debug("menu action 3");
            (actions.on_screenshot)();
        }
        5 => {
            debug("menu action 5");
            (actions.on_quit)();
        }
        _ => {} // root/separator/unknown ids have no action
    }
}

fn invalid_args(invocation: gio::DBusMethodInvocation, expect: &str) {
    invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", expect);
}

fn item(id: i32) -> Option<&'static Item> {
    MENU.iter().find(|(i, _)| *i == id).map(|(_, item)| item)
}

fn menu_props(id: i32) -> glib::Variant {
    let dict = glib::VariantDict::new(None::<&glib::Variant>);
    match item(id) {
        Some(Item::Action(label)) => {
            dict.insert_value("label", &label.to_variant());
            dict.insert_value("enabled", &true.to_variant());
        }
        Some(Item::Separator) => dict.insert_value("type", &"separator".to_variant()),
        // root/unknown ids get empty properties
        _ => {}
    }
    dict.end()
}

fn build_node(id: i32) -> glib::Variant {
    let children: Vec<glib::Variant> = match item(id) {
        Some(Item::Root(ids)) => ids.iter().map(|&c| build_node(c).to_variant()).collect(),
        _ => Vec::new(),
    };
    glib::Variant::tuple_from_iter([
        id.to_variant(),
        menu_props(id),
        glib::Variant::array_from_iter::<glib::Variant>(children),
    ])
}

fn group_properties(ids: &[i32]) -> glib::Variant {
    let entries: Vec<glib::Variant> = ids
        .iter()
        .filter(|id| item(**id).is_some())
        .map(|&id| glib::Variant::tuple_from_iter([id.to_variant(), menu_props(id)]))
        .collect();
    glib::Variant::array_from_iter_with_type(variant_ty("(ia{sv})"), entries)
}

fn empty_ai() -> glib::Variant {
    glib::Variant::array_from_iter::<i32>(std::iter::empty())
}

fn menu_property(name: &str) -> glib::Variant {
    match name {
        "Version" => 3u32.to_variant(),
        "TextDirection" => "ltr".to_variant(),
        "Status" => "normal".to_variant(),
        "IconThemePath" => glib::Variant::array_from_iter::<String>(std::iter::empty()),
        other => {
            debug(format!("unknown property {other}"));
            "".to_variant()
        }
    }
}

/// fixed type strings; a bad one is a typo and panics loudly.
fn variant_ty(s: &str) -> &glib::VariantTy {
    glib::VariantTy::new(s).expect("valid type string")
}

struct SniProps {
    pixmaps: glib::Variant,
    empty_pixmaps: glib::Variant,
}

impl SniProps {
    fn build() -> Self {
        let pixmaps = glib::Variant::array_from_iter_with_type(
            variant_ty("(iiay)"),
            [48, 96].map(|size| {
                glib::Variant::tuple_from_iter([
                    size.to_variant(),
                    size.to_variant(),
                    render_icon(size).to_variant(),
                ])
            }),
        );
        Self {
            pixmaps,
            empty_pixmaps: glib::Variant::array_from_iter_with_type(
                variant_ty("(iiay)"),
                std::iter::empty::<glib::Variant>(),
            ),
        }
    }

    fn get(&self, name: &str) -> glib::Variant {
        match name {
            "Category" => "ApplicationStatus".to_variant(),
            "Id" => "dev.kosuzu.Kosuzu".to_variant(),
            "Title" => "kosuzu".to_variant(),
            "Status" => "Active".to_variant(),
            "IconName" => "kosuzu".to_variant(),
            "IconThemePath" | "OverlayIconName" | "AttentionIconName" => "".to_variant(),
            "IconPixmap" => self.pixmaps.clone(),
            "AttentionIconPixmap" | "OverlayIconPixmap" => self.empty_pixmaps.clone(),
            "Menu" => glib::variant::ObjectPath::try_from(MENU_PATH)
                .expect("valid object path")
                .to_variant(),
            "ItemIsMenu" => false.to_variant(),
            // GDBus validates names against the interface first; reaching here means an undeclared property
            other => {
                debug(format!("unknown property {other}"));
                "".to_variant()
            }
        }
    }
}

/// rasterize the embedded art to `size`; empty on any failure (the named theme icon covers it).
fn render_icon(size: i32) -> Vec<u8> {
    let Ok(img) = image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png) else {
        return Vec::new();
    };
    let Ok(size) = u32::try_from(size) else {
        return Vec::new();
    };
    let img = image::imageops::resize(
        &img.to_rgba8(),
        size,
        size,
        image::imageops::FilterType::Triangle,
    );
    // image crate: RGBA rows top-down -> SNI wire format: network-order non-premultiplied ARGB
    img.pixels()
        .flat_map(|p| [p.0[3], p.0[0], p.0[1], p.0[2]])
        .collect()
}

/// hicolor icons, installed under both the theme name and the app id: the compositor matches
/// either one. Written every start: a stale icon from an earlier version must not survive.
fn install_icons() {
    let hicolor = data_dir().join("icons/hicolor");
    for name in ["kosuzu", "dev.kosuzu.Kosuzu"] {
        for (dir, ext, bytes) in [
            ("scalable/apps", "svg", ICON_SVG),
            ("256x256/apps", "png", ICON_PNG),
        ] {
            let dst = hicolor.join(dir).join(format!("{name}.{ext}"));
            // a write failure is harmless: IconPixmap still works
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(dst, bytes);
        }
    }
}

fn debug(msg: impl std::fmt::Display) {
    if std::env::var_os("KOSUZU_TRAY_DEBUG").is_some() {
        eprintln!("[tray] {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_signatures_match_dbusmenu() {
        let node = build_node(0);
        assert_eq!(node.type_().as_str(), "(ia{sv}av)");
        // return_value holds the out-param tuple
        let reply = glib::Variant::tuple_from_iter([REVISION.to_variant(), node]);
        assert_eq!(reply.type_().as_str(), "(u(ia{sv}av))");
        assert_eq!(group_properties(&[1, 4, 77]).type_().as_str(), "a(ia{sv})");
        assert_eq!(empty_ai().type_().as_str(), "ai");
    }

    #[test]
    fn root_layout_has_five_children() {
        let node = build_node(0);
        let children = node.child_value(2);
        assert_eq!(children.type_().as_str(), "av");
        assert_eq!(children.n_children(), 5);
        // av elements are variants wrapping the recursive node
        let first = children.child_value(0);
        assert_eq!(first.type_().as_str(), "v");
        assert_eq!(first.child_value(0).type_().as_str(), "(ia{sv}av)");
    }

    fn prop(variant: &glib::Variant, key: &str) -> Option<glib::Variant> {
        glib::VariantDict::new(Some(variant)).lookup_value(key, None)
    }

    #[test]
    fn item_props_per_kind() {
        let entry = menu_props(1);
        assert_eq!(
            prop(&entry, "label").and_then(|v| v.get::<String>()),
            Some("Open kosuzu".to_string())
        );
        assert_eq!(
            prop(&entry, "enabled").and_then(|v| v.get::<bool>()),
            Some(true)
        );
        let sep = menu_props(4);
        assert_eq!(
            prop(&sep, "type").and_then(|v| v.get::<String>()),
            Some("separator".to_string())
        );
        // root/unknown ids: empty properties -> GetProperty returns UnknownProperty
        assert_eq!(menu_props(0).n_children(), 0);
        assert_eq!(menu_props(99).n_children(), 0);
    }

    #[test]
    fn group_properties_filters_unknown_ids() {
        let group = group_properties(&[1, 77]);
        assert_eq!(group.n_children(), 1);
        assert_eq!(group.child_value(0).child_value(0).get::<i32>(), Some(1));
    }

    #[test]
    fn icon_pixmap_wire_format() {
        let data = render_icon(48);
        assert_eq!(data.len(), 48 * 48 * 4);
        // the art fills the canvas; the corner stays mostly transparent (downscale bleeds a little)
        assert!(data[0] < 128, "corner should be mostly transparent");
        let center = &data[((24 * 48 + 24) * 4)..][..4];
        assert_eq!(center[0], 255, "center should be opaque");
        // network-order A,R,G,B: the art is copper/orange, so R is the largest channel
        assert!(center[1] > center[2] && center[2] > center[3]);
    }
}
