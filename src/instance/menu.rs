use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib::variant::ToVariant;
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
    static OPEN_MENU: RefCell<Option<gtk::Popover>> = const { RefCell::new(None) };
}

struct MenuNode {
    id: Option<Value>,
    label: String,
    action: Option<String>,
    item_type: Option<String>,
    submenu: Vec<MenuNode>,
    enabled: bool,
    checked: bool,
    separator: bool,
    icon: Option<String>,
}

struct MenuOptions {
    x: Option<f64>,
    y: Option<f64>,
    layout: Option<String>,
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
                separator: true,
                icon: None,
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
            separator: false,
            icon: item.get::<Option<String>>("icon")?,
        });
    }

    Ok(nodes)
}

fn parse_menu_options(options: Option<Table>) -> mlua::Result<MenuOptions> {
    let Some(options) = options else {
        return Ok(MenuOptions {
            x: None,
            y: None,
            layout: None,
            callback: None,
        });
    };

    Ok(MenuOptions {
        x: options.get::<Option<f64>>("x")?,
        y: options.get::<Option<f64>>("y")?,
        layout: options.get::<Option<String>>("layout")?,
        callback: options.get::<Option<Function>>("on_select")?,
    })
}

struct MenuContext<'a> {
    group: &'a gtk::gio::SimpleActionGroup,
    lua: &'a Lua,
    callback: Option<&'a Function>,
    output: Option<&'a Table>,
    current_output: &'a Rc<RefCell<Option<String>>>,
    tx: &'a async_channel::Sender<Msg>,
    key: &'a WidgetKey,
}

fn append_menu(
    target: &gtk::gio::Menu,
    nodes: &[MenuNode],
    context: &MenuContext<'_>,
    counter: &mut usize,
) {
    let mut section = gtk::gio::Menu::new();
    let mut has_items = false;

    for node in nodes {
        if node.separator {
            if has_items {
                target.append_section(None, &section);
                section = gtk::gio::Menu::new();
                has_items = false;
            }
            continue;
        }

        let marker = match node.item_type.as_deref() {
            Some("radio") => Some(if node.checked { "● " } else { "○ " }),
            Some("check") => Some(if node.checked { "✓ " } else { "  " }),
            _ => None,
        };
        let label = match marker {
            Some(marker) => format!("{marker}{}", node.label),
            None => node.label.clone(),
        };

        let item = if !node.submenu.is_empty() {
            let submenu = gtk::gio::Menu::new();
            append_menu(&submenu, &node.submenu, context, counter);
            gtk::gio::MenuItem::new_submenu(Some(&label), &submenu)
        } else if node.id.is_some() || node.action.is_some() {
            let name = format!("item_{}", *counter);
            *counter += 1;

            let simple = gtk::gio::SimpleAction::new(&name, None);
            simple.set_enabled(node.enabled);
            let lua = context.lua.clone();
            let callback = context.callback.cloned();
            let output = context.output.cloned();
            let current_output = context.current_output.clone();
            let tx = context.tx.clone();
            let key = context.key.clone();
            let id = node.id.clone();
            let output_key = current_output.borrow().clone();
            let code = node.action.clone();
            let checked = if matches!(node.item_type.as_deref(), Some("check" | "radio")) {
                !node.checked
            } else {
                false
            };
            simple.connect_activate(move |_, _| {
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
            });
            context.group.add_action(&simple);

            gtk::gio::MenuItem::new(Some(&label), Some(&format!("menu.{name}")))
        } else {
            gtk::gio::MenuItem::new(Some(&label), None)
        };

        if !node.enabled {
            item.set_attribute_value("enabled", Some(&false.to_variant()));
        }

        if let Some(icon) = &node.icon {
            let icon = if icon.starts_with('/') {
                gtk::gio::FileIcon::new(&gtk::gio::File::for_path(icon)).upcast::<gtk::gio::Icon>()
            } else {
                gtk::gio::ThemedIcon::new(icon).upcast::<gtk::gio::Icon>()
            };
            item.set_icon(&icon);
        }

        section.append_item(&item);
        has_items = true;
    }

    if has_items {
        target.append_section(None, &section);
    }
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

    if options.layout.as_deref() == Some("row") {
        show_row_menu(nodes, anchor, options, lua, output, current_output, tx)
    } else {
        show_vertical_menu(nodes, anchor, options, lua, output, current_output, tx)
    }
}

fn show_vertical_menu(
    nodes: Vec<MenuNode>,
    anchor: MenuAnchor,
    options: MenuOptions,
    lua: Lua,
    output: Option<Table>,
    current_output: Rc<RefCell<Option<String>>>,
    tx: async_channel::Sender<Msg>,
) -> mlua::Result<()> {
    let menu = gtk::gio::Menu::new();
    let group = gtk::gio::SimpleActionGroup::new();
    let mut counter = 0usize;
    let context = MenuContext {
        group: &group,
        lua: &lua,
        callback: options.callback.as_ref(),
        output: output.as_ref(),
        current_output: &current_output,
        tx: &tx,
        key: &anchor.key,
    };
    append_menu(&menu, &nodes, &context, &mut counter);

    let old = OPEN_MENU.with(|slot| slot.borrow_mut().take());
    if let Some(old) = old {
        old.popdown();
        old.unparent();
    }

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.add_css_class("nitroget-menu");
    popover.set_has_arrow(false);
    popover.set_parent(&anchor.parent);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        options.x.unwrap_or(anchor.x).round() as i32,
        options.y.unwrap_or(anchor.y).round() as i32,
        1,
        1,
    )));
    popover.insert_action_group("menu", Some(&group));

    let window = anchor.window.clone();
    let layer_shell = anchor.layer_shell;
    popover.connect_closed(move |popover| {
        popover.unparent();
        OPEN_MENU.with(|slot| *slot.borrow_mut() = None);
        if layer_shell {
            use gtk4_layer_shell::LayerShell;
            window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
        }
    });

    OPEN_MENU.with(|slot| *slot.borrow_mut() = Some(popover.clone().upcast()));

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

fn show_row_menu(
    nodes: Vec<MenuNode>,
    anchor: MenuAnchor,
    options: MenuOptions,
    lua: Lua,
    output: Option<Table>,
    current_output: Rc<RefCell<Option<String>>>,
    tx: async_channel::Sender<Msg>,
) -> mlua::Result<()> {
    let old = OPEN_MENU.with(|slot| slot.borrow_mut().take());
    if let Some(old) = old {
        old.popdown();
        old.unparent();
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

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    for node in &nodes {
        if node.separator || !node.submenu.is_empty() {
            continue;
        }

        let button = gtk::Button::with_label(&node.label);
        button.add_css_class("menu-item");
        if node.checked {
            button.add_css_class("selected");
        }
        button.set_sensitive(node.enabled);

        let lua = lua.clone();
        let callback = options.callback.clone();
        let output = output.clone();
        let current_output = current_output.clone();
        let output_key = current_output.borrow().clone();
        let tx = tx.clone();
        let key = anchor.key.clone();
        let id = node.id.clone();
        let code = node.action.clone();
        let checked = !node.checked;
        let popover_for_click = popover.clone();
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
            popover_for_click.popdown();
        });

        row.append(&button);
    }

    popover.set_child(Some(&row));

    let window = anchor.window.clone();
    let layer_shell = anchor.layer_shell;
    popover.connect_closed(move |popover| {
        popover.unparent();
        OPEN_MENU.with(|slot| *slot.borrow_mut() = None);
        if layer_shell {
            use gtk4_layer_shell::LayerShell;
            window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
        }
    });

    OPEN_MENU.with(|slot| *slot.borrow_mut() = Some(popover.clone()));

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    }

    #[test]
    fn builds_menu_tree() {
        let lua = Lua::new();
        let table: Table = lua
            .load(
                r#"return {
                    { label = "A", action = "a()" },
                    { separator = true },
                    { label = "B", checked = true, action = "b()" },
                    { label = "C", submenu = {
                        { label = "D", enabled = false, action = "d()" },
                    } },
                }"#,
            )
            .eval()
            .unwrap();

        let nodes = parse_menu_nodes(&table).unwrap();
        let menu = gtk::gio::Menu::new();
        let group = gtk::gio::SimpleActionGroup::new();
        let (tx, _rx) = async_channel::unbounded();
        let key: WidgetKey = (PathBuf::from("w.nitroget"), None);
        let current_output = Rc::new(RefCell::new(None));
        let context = MenuContext {
            group: &group,
            lua: &lua,
            callback: None,
            output: None,
            current_output: &current_output,
            tx: &tx,
            key: &key,
        };
        let mut counter = 0;
        append_menu(&menu, &nodes, &context, &mut counter);

        assert_eq!(counter, 3);
    }
}
