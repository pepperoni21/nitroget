use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use mlua::{Function, Lua, Table, Value};

use crate::state::{Msg, WidgetKey};

#[derive(Clone)]
pub struct MenuAnchor {
    pub window: gtk::Window,
    pub parent: gtk::Widget,
    pub key: WidgetKey,
    pub x: f64,
    pub y: f64,
    pub layer_shell: bool,
}

thread_local! {
    pub static MENU_ANCHOR: RefCell<Option<MenuAnchor>> = const { RefCell::new(None) };
    static OPEN_MENU: RefCell<Option<MenuState>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuLayout {
    Native,
    Row,
    Column,
}

impl MenuLayout {
    fn orientation(self) -> gtk::Orientation {
        match self {
            Self::Row => gtk::Orientation::Horizontal,
            Self::Native | Self::Column => gtk::Orientation::Vertical,
        }
    }

    fn default_close_on_select(self) -> bool {
        !matches!(self, Self::Row | Self::Column)
    }

    fn allows_submenus(self) -> bool {
        matches!(self, Self::Native)
    }

    fn allows_separators(self) -> bool {
        !matches!(self, Self::Row)
    }
}

struct MenuNode {
    id: Option<Value>,
    label: String,
    action: Option<String>,
    item_type: Option<String>,
    submenu: Vec<MenuNode>,
    enabled: bool,
    checked: bool,
    selected: bool,
    separator: bool,
    icon: Option<String>,
    value: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
    show_value: Option<bool>,
}

#[derive(Clone)]
struct MenuOptions {
    x: Option<f64>,
    y: Option<f64>,
    layout: MenuLayout,
    close_on_select: bool,
    callback: Option<Function>,
}

fn parse_menu_nodes(items: &Table) -> mlua::Result<Vec<MenuNode>> {
    let mut nodes = Vec::new();

    for value in items.sequence_values::<Table>() {
        let item = value?;
        let separator = item.get::<Option<bool>>("separator")?.unwrap_or(false);
        if separator {
            nodes.push(MenuNode {
                id: None,
                label: String::new(),
                action: None,
                item_type: None,
                submenu: Vec::new(),
                enabled: true,
                checked: false,
                selected: false,
                separator: true,
                icon: None,
                value: None,
                min: None,
                max: None,
                step: None,
                show_value: None,
            });
            continue;
        }

        let submenu = match item.get::<Option<Table>>("submenu")? {
            Some(submenu) => parse_menu_nodes(&submenu)?,
            None => Vec::new(),
        };

        nodes.push(MenuNode {
            id: item.get::<Option<Value>>("id")?,
            label: item.get::<Option<String>>("label")?.unwrap_or_default(),
            action: item.get::<Option<String>>("action")?,
            item_type: item.get::<Option<String>>("type")?,
            submenu,
            enabled: item.get::<Option<bool>>("enabled")?.unwrap_or(true),
            checked: item.get::<Option<bool>>("checked")?.unwrap_or(false),
            selected: item.get::<Option<bool>>("selected")?.unwrap_or(false),
            separator: false,
            icon: item.get::<Option<String>>("icon")?,
            value: item.get::<Option<f64>>("value")?,
            min: item.get::<Option<f64>>("min")?,
            max: item.get::<Option<f64>>("max")?,
            step: item.get::<Option<f64>>("step")?,
            show_value: item.get::<Option<bool>>("show_value")?,
        });
    }

    Ok(nodes)
}

fn parse_layout(value: Option<String>) -> mlua::Result<MenuLayout> {
    match value.as_deref() {
        None | Some("" | "menu") => Ok(MenuLayout::Native),
        Some("row") => Ok(MenuLayout::Row),
        Some("column") => Ok(MenuLayout::Column),
        Some(_) => Err(mlua::Error::runtime(
            "menu layout must be \"menu\", \"row\" or \"column\"",
        )),
    }
}

fn parse_menu_options(options: Option<Table>) -> mlua::Result<MenuOptions> {
    let Some(options) = options else {
        return Ok(MenuOptions {
            x: None,
            y: None,
            layout: MenuLayout::Native,
            close_on_select: MenuLayout::Native.default_close_on_select(),
            callback: None,
        });
    };

    let layout = parse_layout(options.get::<Option<String>>("layout")?)?;

    Ok(MenuOptions {
        x: options.get::<Option<f64>>("x")?,
        y: options.get::<Option<f64>>("y")?,
        layout,
        close_on_select: options
            .get::<Option<bool>>("close_on_select")?
            .unwrap_or(layout.default_close_on_select()),
        callback: options.get::<Option<Function>>("on_select")?,
    })
}

struct MenuState {
    anchor: MenuAnchor,
    options: MenuOptions,
    lua: Lua,
    output: Option<Table>,
    current_output: Rc<RefCell<Option<String>>>,
    tx: async_channel::Sender<Msg>,
    container: gtk::Box,
    popover: gtk::Popover,
}

struct MenuContext<'a> {
    lua: &'a Lua,
    callback: Option<&'a Function>,
    output: Option<&'a Table>,
    current_output: &'a Rc<RefCell<Option<String>>>,
    tx: &'a async_channel::Sender<Msg>,
    key: &'a WidgetKey,
    close_on_select: bool,
    root: &'a gtk::Popover,
    layout: MenuLayout,
}

pub fn show_menu(
    items: Table,
    anchor: MenuAnchor,
    options: Option<Table>,
    lua: Lua,
    output: Option<Table>,
    current_output: Rc<RefCell<Option<String>>>,
    tx: async_channel::Sender<Msg>,
) -> mlua::Result<()> {
    let nodes = parse_menu_nodes(&items)?;
    let options = parse_menu_options(options)?;

    let old = OPEN_MENU.with(|slot| slot.borrow_mut().take());
    if let Some(old) = old {
        clear_menu(&old.container);
        old.popover.popdown();
        old.popover.unparent();
    }

    let popover = gtk::Popover::new();
    popover.add_css_class("nitroget-menu");
    popover.set_has_arrow(false);
    popover.set_parent(&anchor.parent);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        options.x.unwrap_or(anchor.x).round() as i32,
        options.y.unwrap_or(anchor.y).round() as i32,
        1,
        1,
    )));

    let container = gtk::Box::new(options.layout.orientation(), 2);
    popover.set_child(Some(&container));

    let context = MenuContext {
        lua: &lua,
        callback: options.callback.as_ref(),
        output: output.as_ref(),
        current_output: &current_output,
        tx: &tx,
        key: &anchor.key,
        close_on_select: options.close_on_select,
        root: &popover,
        layout: options.layout,
    };
    build_menu(&container, &nodes, &context);

    let window = anchor.window.clone();
    let layer_shell = anchor.layer_shell;
    popover.connect_closed(move |popover| {
        popover.unparent();
        let closed = OPEN_MENU.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|state| state.popover == *popover) {
                *slot = None;
                true
            } else {
                false
            }
        });
        if closed && layer_shell {
            use gtk4_layer_shell::LayerShell;
            window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
        }
    });

    OPEN_MENU.with(|slot| {
        *slot.borrow_mut() = Some(MenuState {
            anchor: anchor.clone(),
            options,
            lua,
            output,
            current_output,
            tx,
            container,
            popover: popover.clone(),
        });
    });

    if layer_shell {
        use gtk4_layer_shell::LayerShell;
        anchor
            .window
            .set_keyboard_mode(gtk4_layer_shell::KeyboardMode::OnDemand);
    }

    popover.popup();
    popover.child_focus(gtk::DirectionType::TabForward);
    Ok(())
}

pub fn close_menu() {
    let old = OPEN_MENU.with(|slot| slot.borrow_mut().take());
    if let Some(state) = old {
        clear_menu(&state.container);
        state.popover.popdown();
    }
}

pub fn update_menu(items: Table) -> mlua::Result<()> {
    let nodes = parse_menu_nodes(&items)?;
    OPEN_MENU.with(|slot| {
        let slot = slot.borrow();
        let Some(state) = slot.as_ref() else {
            return Ok(());
        };
        let context = MenuContext {
            lua: &state.lua,
            callback: state.options.callback.as_ref(),
            output: state.output.as_ref(),
            current_output: &state.current_output,
            tx: &state.tx,
            key: &state.anchor.key,
            close_on_select: state.options.close_on_select,
            root: &state.popover,
            layout: state.options.layout,
        };
        build_menu(&state.container, &nodes, &context);
        Ok(())
    })
}

fn build_menu(container: &gtk::Box, nodes: &[MenuNode], context: &MenuContext<'_>) {
    clear_menu(container);

    for node in nodes {
        if node.separator {
            if context.layout.allows_separators() {
                container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            continue;
        }

        if !node.submenu.is_empty() {
            if context.layout.allows_submenus() {
                container.append(&build_submenu(node, context));
            }
            continue;
        }

        container.append(&build_item(node, context));
    }
}

fn clear_menu(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        if let Some(button) = child.downcast_ref::<gtk::MenuButton>()
            && let Some(popover) = button.popover()
        {
            if let Some(content) = popover.child().and_downcast::<gtk::Box>() {
                clear_menu(&content);
            }
            popover.popdown();
            button.set_popover(None::<&gtk::Popover>);
        }
        container.remove(&child);
    }
}

fn build_item(node: &MenuNode, context: &MenuContext<'_>) -> gtk::Widget {
    match node.item_type.as_deref() {
        Some("slider") => return build_slider(node, context),
        Some("label") => {
            let label = gtk::Label::new(Some(&node.label));
            label.add_css_class("menu-label");
            label.set_halign(gtk::Align::Center);
            return label.upcast();
        }
        _ => {}
    }

    let button = gtk::Button::new();
    button.set_child(Some(&item_content(node)));
    button.add_css_class("menu-item");
    if node.selected {
        button.add_css_class("selected");
    }

    let locked = node.selected && !node.enabled;
    button.set_sensitive(node.enabled || locked);
    if locked {
        button.set_can_target(false);
        button.set_can_focus(false);
    }

    let lua = context.lua.clone();
    let callback = context.callback.cloned();
    let output = context.output.cloned();
    let current_output = context.current_output.clone();
    let output_key = current_output.borrow().clone();
    let tx = context.tx.clone();
    let key = context.key.clone();
    let id = node.id.clone();
    let code = node.action.clone();
    let checked = next_checked(node);
    let popover = context.root.clone();
    let close_on_select = context.close_on_select;
    button.connect_clicked(move |_| {
        if let Some(callback) = &callback {
            *current_output.borrow_mut() = output_key.clone();
            if let Some(output) = &output {
                let _ = lua.globals().set("output", output.clone());
            }
            let item_id = id.clone().unwrap_or(Value::Nil);
            if let Err(error) = callback.call::<()>((item_id, checked)) {
                eprintln!("nitroget: menu callback error: {error}");
            }
            let _ = tx.try_send(Msg::Render {
                path: key.0.clone(),
            });
        } else if let Some(code) = &code {
            let _ = tx.try_send(Msg::MenuAction {
                key: key.clone(),
                code: code.clone(),
            });
        }
        if close_on_select {
            popover.popdown();
        }
    });

    button.upcast()
}

fn build_slider(node: &MenuNode, context: &MenuContext<'_>) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.add_css_class("menu-slider");
    row.set_halign(gtk::Align::Center);

    let min = node.min.unwrap_or(0.0);
    let max = node.max.unwrap_or(100.0).max(min + 1.0);
    let step = node.step.unwrap_or(1.0).max(f64::EPSILON);
    let value = node.value.unwrap_or(min).clamp(min, max);
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    scale.set_value(value);
    scale.set_draw_value(node.show_value.unwrap_or(true));
    scale.set_digits(if step.fract() == 0.0 { 0 } else { 1 });
    scale.set_hexpand(true);
    scale.set_halign(gtk::Align::Center);
    scale.set_size_request(140, -1);
    scale.set_sensitive(node.enabled);

    if let Some(code) = &node.action {
        let tx = context.tx.clone();
        let key = context.key.clone();
        let code = code.clone();
        scale.connect_value_changed(move |scale| {
            let _ = tx.try_send(Msg::Change {
                key: key.clone(),
                code: code.clone(),
                value: scale.value(),
            });
        });
    }

    row.append(&scale);
    row.upcast()
}

fn build_submenu(node: &MenuNode, context: &MenuContext<'_>) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.add_css_class("menu-item");
    button.add_css_class("submenu");
    button.set_sensitive(node.enabled);

    let content = item_content(node);
    let arrow = gtk::Image::from_icon_name("pan-end-symbolic");
    arrow.set_pixel_size(12);
    content.append(&arrow);
    button.set_child(Some(&content));

    let popover = gtk::Popover::new();
    popover.add_css_class("nitroget-menu");
    popover.set_has_arrow(false);
    let submenu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    popover.set_child(Some(&submenu));
    build_menu(&submenu, &node.submenu, context);
    button.set_popover(Some(&popover));

    button
}

fn item_content(node: &MenuNode) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);

    if let Some(icon) = &node.icon {
        let image = if icon.starts_with('/') {
            gtk::Image::from_file(icon)
        } else {
            gtk::Image::from_icon_name(icon)
        };
        image.set_pixel_size(14);
        content.append(&image);
    }

    if let Some(marker) = marker(node) {
        let label = gtk::Label::new(Some(marker));
        label.set_width_chars(1);
        content.append(&label);
    }

    if !node.label.is_empty() {
        content.append(&gtk::Label::new(Some(&node.label)));
    }

    content
}

fn marker(node: &MenuNode) -> Option<&'static str> {
    match node.item_type.as_deref() {
        Some("radio") if node.checked => Some("●"),
        Some("radio") => Some("○"),
        Some("check") if node.checked => Some("✓"),
        Some("check") => Some(""),
        _ => None,
    }
}

fn next_checked(node: &MenuNode) -> bool {
    matches!(node.item_type.as_deref(), Some("check" | "radio")) && !node.checked
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    static GTK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn gtk_ready() -> bool {
        let _guard = GTK_LOCK.lock().unwrap();
        if gtk::is_initialized() {
            return true;
        }
        match gtk::init() {
            Ok(()) => true,
            Err(error) => {
                if std::env::var_os("NITROGET_REQUIRE_GTK").is_some() {
                    panic!("GTK unavailable: {error}");
                }
                false
            }
        }
    }

    fn test_anchor() -> MenuAnchor {
        let window = gtk::Window::new();
        let parent = window.clone().upcast::<gtk::Widget>();
        MenuAnchor {
            window,
            parent,
            key: (PathBuf::from("test.nitroget"), None),
            x: 0.0,
            y: 0.0,
            layer_shell: false,
        }
    }

    fn test_context<'a>(
        lua: &'a Lua,
        callback: Option<&'a Function>,
        current_output: &'a Rc<RefCell<Option<String>>>,
        tx: &'a async_channel::Sender<Msg>,
        key: &'a WidgetKey,
        root: &'a gtk::Popover,
    ) -> MenuContext<'a> {
        MenuContext {
            lua,
            callback,
            output: None,
            current_output,
            tx,
            key,
            close_on_select: false,
            root,
            layout: MenuLayout::Native,
        }
    }

    #[test]
    fn parses_menu_nodes() {
        let lua = Lua::new();
        let table: Table = lua
            .load(
                r#"return {
                    { label = "A", action = "a()" },
                    { separator = true },
                    { id = 7, label = "B", type = "check", submenu = {
                        { label = "C", enabled = false, checked = true },
                    } },
                }"#,
            )
            .eval()
            .unwrap();

        let nodes = parse_menu_nodes(&table).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].label, "A");
        assert_eq!(nodes[0].action.as_deref(), Some("a()"));
        assert!(nodes[1].separator);
        assert_eq!(nodes[2].label, "B");
        assert!(matches!(&nodes[2].id, Some(Value::Integer(7))));
        assert_eq!(nodes[2].item_type.as_deref(), Some("check"));
        assert_eq!(nodes[2].submenu.len(), 1);
        assert!(!nodes[2].submenu[0].enabled);
        assert!(nodes[2].submenu[0].checked);
        assert!(!nodes[2].submenu[0].selected);
    }

    #[test]
    fn parses_menu_options_layouts() {
        let lua = Lua::new();
        let table: Table = lua
            .load("return { layout = \"row\", x = 10, y = 20 }")
            .eval()
            .unwrap();
        let options = parse_menu_options(Some(table)).unwrap();
        assert_eq!(options.layout, MenuLayout::Row);
        assert!(!options.close_on_select);
        assert_eq!(options.x, Some(10.0));
        assert_eq!(options.y, Some(20.0));

        let table: Table = lua
            .load("return { layout = \"column\", close_on_select = true }")
            .eval()
            .unwrap();
        let options = parse_menu_options(Some(table)).unwrap();
        assert_eq!(options.layout, MenuLayout::Column);
        assert!(options.close_on_select);

        let table: Table = lua.load("return { layout = \"grid\" }").eval().unwrap();
        assert!(parse_menu_options(Some(table)).is_err());
    }

    #[test]
    fn native_menus_close_by_default() {
        let lua = Lua::new();
        let table: Table = lua.load("return {}").eval().unwrap();
        let options = parse_menu_options(Some(table)).unwrap();
        assert_eq!(options.layout, MenuLayout::Native);
        assert!(options.close_on_select);

        let table: Table = lua
            .load("return { close_on_select = false }")
            .eval()
            .unwrap();
        let options = parse_menu_options(Some(table)).unwrap();
        assert!(!options.close_on_select);
    }

    #[test]
    fn selected_is_independent_of_checked() {
        let lua = Lua::new();
        let table: Table = lua
            .load(
                r#"return {
                    { label = "Current", selected = true, enabled = false },
                    { label = "Checked", type = "check", checked = true },
                }"#,
            )
            .eval()
            .unwrap();
        let nodes = parse_menu_nodes(&table).unwrap();
        assert!(nodes[0].selected);
        assert!(!nodes[0].checked);
        assert!(nodes[1].checked);
        assert!(!nodes[1].selected);
    }

    #[test]
    fn toggles_only_check_and_radio_items() {
        let lua = Lua::new();
        let table: Table = lua
            .load(
                r#"return {
                    { label = "Plain" },
                    { label = "Check", type = "check" },
                    { label = "Checked", type = "check", checked = true },
                    { label = "Radio", type = "radio" },
                }"#,
            )
            .eval()
            .unwrap();
        let nodes = parse_menu_nodes(&table).unwrap();
        assert!(!next_checked(&nodes[0]));
        assert!(next_checked(&nodes[1]));
        assert!(!next_checked(&nodes[2]));
        assert!(next_checked(&nodes[3]));
        assert_eq!(marker(&nodes[0]), None);
        assert_eq!(marker(&nodes[1]), Some(""));
        assert_eq!(marker(&nodes[2]), Some("✓"));
        assert_eq!(marker(&nodes[3]), Some("○"));
    }

    #[test]
    fn parses_slider_items() {
        let lua = Lua::new();
        let table: Table = lua
            .load(
                r#"return {
                    { label = "Brightness", type = "slider", value = 40, min = 1, max = 100, step = 1, show_value = false, action = "set_brightness(event.value)" },
                }"#,
            )
            .eval()
            .unwrap();
        let nodes = parse_menu_nodes(&table).unwrap();
        assert_eq!(nodes[0].item_type.as_deref(), Some("slider"));
        assert_eq!(nodes[0].value, Some(40.0));
        assert_eq!(nodes[0].min, Some(1.0));
        assert_eq!(nodes[0].max, Some(100.0));
        assert_eq!(nodes[0].step, Some(1.0));
        assert_eq!(nodes[0].show_value, Some(false));
        assert_eq!(
            nodes[0].action.as_deref(),
            Some("set_brightness(event.value)")
        );
    }

    #[test]
    fn builds_updates_and_invokes_menu() {
        if !gtk_ready() {
            return;
        }

        let lua = Lua::new();
        let table: Table = lua
            .load(
                r#"return {
                    { label = "One", id = 1 },
                    { separator = true },
                    { label = "Two", id = 2 },
                    { label = "Brightness", type = "slider", value = 40, min = 1, max = 100, step = 1, show_value = false, action = "set_brightness(event.value)" },
                    { label = "More", submenu = {
                        { label = "Nested", id = 3 },
                    } },
                }"#,
            )
            .eval()
            .unwrap();
        let nodes = parse_menu_nodes(&table).unwrap();
        let anchor = test_anchor();
        let (tx, rx) = async_channel::unbounded();
        let current_output = Rc::new(RefCell::new(None));
        let popover = gtk::Popover::new();
        let container = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let context = test_context(&lua, None, &current_output, &tx, &anchor.key, &popover);
        build_menu(&container, &nodes, &context);

        let mut children = Vec::new();
        let mut child = container.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            children.push(widget);
        }
        assert_eq!(children.len(), 5);
        assert!(children[1].is::<gtk::Separator>());
        assert!(children[2].is::<gtk::Button>());
        assert!(children[3].is::<gtk::Box>());
        assert!(children[4].is::<gtk::MenuButton>());

        let slider = children[3]
            .last_child()
            .unwrap()
            .downcast::<gtk::Scale>()
            .unwrap();
        assert_eq!(slider.value(), 40.0);
        assert_eq!(slider.adjustment().lower(), 1.0);
        assert_eq!(slider.adjustment().upper(), 100.0);
        assert!(!slider.draws_value());
        slider.set_value(55.0);
        assert!(matches!(
            rx.try_recv().unwrap(),
            Msg::Change { code, value, .. }
                if code == "set_brightness(event.value)" && value == 55.0
        ));

        let state = MenuState {
            anchor,
            options: MenuOptions {
                x: None,
                y: None,
                layout: MenuLayout::Native,
                close_on_select: true,
                callback: None,
            },
            lua: lua.clone(),
            output: None,
            current_output,
            tx,
            container: container.clone(),
            popover: popover.clone(),
        };
        OPEN_MENU.with(|slot| *slot.borrow_mut() = Some(state));

        let updated: Table = lua
            .load(r#"return { { label = "Only", id = 9 } }"#)
            .eval()
            .unwrap();
        update_menu(updated).unwrap();

        assert_eq!(container.first_child().unwrap().next_sibling(), None);
        assert_eq!(
            OPEN_MENU.with(|slot| slot.borrow().as_ref().unwrap().popover.clone()),
            popover
        );

        close_menu();
        assert!(OPEN_MENU.with(|slot| slot.borrow().is_none()));

        let lua = Lua::new();
        let items: Table = lua
            .load(r#"return { { label = "Seven", id = 7, type = "check" } }"#)
            .eval()
            .unwrap();
        let options: Table = lua
            .load("return { on_select = function(id, checked) selected_id = id; selected_checked = checked end }")
            .eval()
            .unwrap();
        let parsed = parse_menu_options(Some(options)).unwrap();
        let nodes = parse_menu_nodes(&items).unwrap();
        let anchor = test_anchor();
        let (tx, rx) = async_channel::unbounded();
        let current_output = Rc::new(RefCell::new(None));
        let popover = gtk::Popover::new();
        let container = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let context = test_context(
            &lua,
            parsed.callback.as_ref(),
            &current_output,
            &tx,
            &anchor.key,
            &popover,
        );
        build_menu(&container, &nodes, &context);

        let button = container
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        button.emit_clicked();

        assert!(matches!(
            lua.globals().get::<Value>("selected_id").unwrap(),
            Value::Integer(7)
        ));
        assert!(lua.globals().get::<bool>("selected_checked").unwrap());
        assert!(matches!(
            rx.try_recv().unwrap(),
            Msg::Render { path } if path.as_os_str() == "test.nitroget"
        ));
    }
}
