//! Persistent render tree with fine-grained updates.
//!
//! The template is built into GTK widgets once. State changes walk the tree and
//! only touch values that changed; `{#if}` and `{#each}` rebuild their subtree
//! only when their branch or row count changes. The Lua script itself is not
//! re-run.

use std::cell::Cell;
use std::rc::Rc;

use anyhow::Result;
use gtk::prelude::*;
use mlua::Value;

use crate::component::ComponentKind;
use crate::instance::Instance;
use crate::state::Msg;
use crate::template::{Element as TplElement, Node};

pub struct Tree {
    pub root: gtk::Widget,
    pub surface: gtk::Widget,
    pub nodes: Vec<RNode>,
}

impl Tree {
    fn update(&mut self, inst: &mut Instance) -> Result<()> {
        for node in &mut self.nodes {
            node.update(inst)?;
        }
        Ok(())
    }

    pub fn has_visible_entry(&self) -> bool {
        self.nodes.iter().any(RNode::has_visible_entry)
    }
}

fn widget_visible(widget: &gtk::Widget) -> bool {
    let mut current = Some(widget.clone());

    while let Some(widget) = current {
        if widget.is::<gtk::Window>() {
            break;
        }
        if !widget.is_visible() {
            return false;
        }
        current = widget.parent();
    }

    true
}

pub enum RNode {
    Element(RElement),
    Text(RText),
    If(RIf),
    Each(REach),
}

impl RNode {
    fn update(&mut self, inst: &mut Instance) -> Result<()> {
        match self {
            RNode::Element(element) => element.update(inst),
            RNode::Text(text) => text.update(inst),
            RNode::If(branch) => branch.update(inst),
            RNode::Each(each) => each.update(inst),
        }
    }

    fn widgets(&self) -> Vec<gtk::Widget> {
        match self {
            RNode::Element(element) => vec![element.widget.clone()],
            RNode::Text(text) => vec![text.label.clone().upcast()],
            RNode::If(branch) => vec![branch.container.clone().upcast()],
            RNode::Each(each) => vec![each.container.clone().upcast()],
        }
    }

    fn has_visible_entry(&self) -> bool {
        match self {
            RNode::Element(element) => element.has_visible_entry(),
            RNode::Text(_) => false,
            RNode::If(branch) => branch.children.iter().any(RNode::has_visible_entry),
            RNode::Each(each) => each.rows.iter().flatten().any(RNode::has_visible_entry),
        }
    }
}

pub struct RText {
    label: gtk::Label,
    raw: String,
    last: String,
}

impl RText {
    fn update(&mut self, inst: &mut Instance) -> Result<()> {
        let text = inst.interp(&self.raw)?;
        if text != self.last {
            self.label.set_text(&text);
            self.last = text;
        }
        Ok(())
    }
}

pub struct RElement {
    widget: gtk::Widget,
    kind: RKind,
    children: Vec<RNode>,
    class_expr: Option<String>,
    classes: Vec<String>,
    visible_expr: Option<String>,
    visible: bool,
    tooltip_expr: Option<String>,
    tooltip: String,
}

enum RKind {
    Container,
    Static,
    Label {
        label: gtk::Label,
        raw: String,
        last: String,
        markdown: bool,
    },
    Button {
        button: gtk::Button,
        text: Option<String>,
        last: String,
    },
    Bar {
        bar: gtk::ProgressBar,
        value: String,
        max: Option<String>,
        last: f64,
    },
    Slider {
        scale: gtk::Scale,
        value: String,
        last: f64,
        suppress: Rc<Cell<bool>>,
    },
    Image {
        picture: gtk::Picture,
        src: String,
        last: String,
    },
    Entry {
        entry: gtk::Entry,
        text: String,
        last: String,
        suppress: Rc<Cell<bool>>,
    },
    Select {
        dropdown: gtk::DropDown,
        options: String,
        selected: String,
        last_options: Vec<String>,
        last_selected: Option<String>,
        suppress: Rc<Cell<bool>>,
    },
    Scroll,
}

impl RElement {
    fn has_visible_entry(&self) -> bool {
        if matches!(&self.kind, RKind::Entry { .. }) && widget_visible(&self.widget) {
            return true;
        }

        self.children.iter().any(RNode::has_visible_entry)
    }

    fn update(&mut self, inst: &mut Instance) -> Result<()> {
        self.update_common(inst)?;

        match &mut self.kind {
            RKind::Container | RKind::Static => {}

            RKind::Label {
                label,
                raw,
                last,
                markdown,
            } => {
                let text = inst.interp(raw)?;
                if text != *last {
                    if *markdown {
                        label.set_markup(&crate::markdown::to_pango(&text));
                    } else {
                        label.set_text(&text);
                    }
                    *last = text;
                }
            }

            RKind::Button {
                button,
                text: Some(raw),
                last,
            } => {
                let text = inst.interp(raw)?;
                if text != *last {
                    button.set_label(&text);
                    *last = text;
                }
            }
            RKind::Button { text: None, .. } => {}

            RKind::Bar {
                bar,
                value,
                max,
                last,
            } => {
                let value = eval_num(inst, value)?.unwrap_or(0.0);
                let max = match max {
                    Some(expr) => eval_num(inst, expr)?
                        .unwrap_or(100.0)
                        .abs()
                        .max(f64::EPSILON),
                    None => 100.0,
                };
                let fraction = (value / max).clamp(0.0, 1.0);
                if (fraction - *last).abs() > 1e-6 {
                    bar.set_fraction(fraction);
                    *last = fraction;
                }
            }

            RKind::Slider {
                scale,
                value,
                last,
                suppress,
            } => {
                let value = eval_num(inst, value)?.unwrap_or(*last);
                if (value - *last).abs() > 1e-9 {
                    suppress.set(true);
                    scale.set_value(value);
                    suppress.set(false);
                    *last = value;
                }
            }

            RKind::Image { picture, src, last } => {
                let path = inst.interp(src)?;
                if path != *last {
                    set_picture(inst, picture, &path);
                    *last = path;
                }
            }

            RKind::Entry {
                entry,
                text,
                last,
                suppress,
            } => {
                let value = inst.interp(text)?;
                if value != *last {
                    if value != entry.text() {
                        suppress.set(true);
                        entry.set_text(&value);
                        suppress.set(false);
                    }
                    *last = value;
                }
            }

            RKind::Select {
                dropdown,
                options,
                selected,
                last_options,
                last_selected,
                suppress,
            } => {
                let values: Vec<String> = eval_list(inst, options)?
                    .iter()
                    .map(value_to_string)
                    .collect();
                let options_changed = values != *last_options;

                if options_changed {
                    suppress.set(true);
                    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
                    let list = gtk::StringList::new(&refs);
                    dropdown.set_model(Some(&list));
                    suppress.set(false);
                    *last_options = values.clone();
                }

                let selected_value = value_to_string(&inst.eval_value(selected)?);
                let index = values.iter().position(|value| *value == selected_value);
                let active = if values.is_empty() {
                    None
                } else {
                    Some(values[index.unwrap_or(0)].clone())
                };
                let target = if values.is_empty() {
                    gtk::INVALID_LIST_POSITION
                } else {
                    index.unwrap_or(0) as u32
                };

                if options_changed || active != *last_selected || dropdown.selected() != target {
                    suppress.set(true);
                    dropdown.set_selected(target);
                    suppress.set(false);
                    *last_selected = active;
                }

                dropdown.set_sensitive(!values.is_empty());
            }

            RKind::Scroll => {}
        }

        for child in &mut self.children {
            child.update(inst)?;
        }

        Ok(())
    }

    fn update_common(&mut self, inst: &mut Instance) -> Result<()> {
        if let Some(expr) = &self.class_expr {
            let value = inst.interp(expr)?;
            let classes: Vec<String> = value.split_whitespace().map(str::to_string).collect();
            if classes != self.classes {
                for old in &self.classes {
                    if !classes.contains(old) {
                        self.widget.remove_css_class(old);
                    }
                }
                for class in &classes {
                    if !self.classes.contains(class) {
                        self.widget.add_css_class(class);
                    }
                }
                self.classes = classes;
            }
        }

        if let Some(expr) = &self.visible_expr {
            let visible = inst.eval_bool(expr)?;
            if visible != self.visible {
                self.widget.set_visible(visible);
                self.visible = visible;
            }
        }

        if let Some(expr) = &self.tooltip_expr {
            let tooltip = inst.interp(expr)?;
            if tooltip != self.tooltip {
                self.widget.set_tooltip_text(Some(&tooltip));
                self.tooltip = tooltip;
            }
        }

        Ok(())
    }
}

pub struct RIf {
    container: gtk::Box,
    cond: String,
    branch: Option<bool>,
    children: Vec<RNode>,
    then_body: Vec<Node>,
    else_body: Vec<Node>,
    orientation: gtk::Orientation,
}

impl RIf {
    fn update(&mut self, inst: &mut Instance) -> Result<()> {
        let cond = inst.eval_bool(&self.cond)?;

        if self.branch == Some(cond) {
            for child in &mut self.children {
                child.update(inst)?;
            }
            return Ok(());
        }

        clear(&self.container);
        self.children.clear();

        let body = if cond {
            &self.then_body
        } else {
            &self.else_body
        };
        self.children = build_nodes(inst, body, self.orientation)?;
        append_nodes(&self.container, &self.children);
        sync_expand(&self.container, &self.children);
        self.branch = Some(cond);

        Ok(())
    }
}

pub struct REach {
    container: gtk::Box,
    list: String,
    item: String,
    index: Option<String>,
    body: Vec<Node>,
    rows: Vec<Vec<RNode>>,
    orientation: gtk::Orientation,
}

impl REach {
    fn update(&mut self, inst: &mut Instance) -> Result<()> {
        let values = eval_list(inst, &self.list)?;

        if values.len() != self.rows.len() {
            clear(&self.container);
            self.rows.clear();

            let globals = inst.lua.globals();
            let old_item: Value = globals.get(self.item.as_str())?;
            let old_index: Value = match &self.index {
                Some(index) => globals.get(index.as_str())?,
                None => Value::Nil,
            };

            for (position, value) in values.into_iter().enumerate() {
                globals.set(self.item.as_str(), value)?;
                if let Some(index) = &self.index {
                    globals.set(index.as_str(), position as i64 + 1)?;
                }

                inst.each_stack.push(position as i64 + 1);
                let row = build_nodes(inst, &self.body, self.orientation);
                inst.each_stack.pop();
                let row = row?;
                append_nodes(&self.container, &row);
                self.rows.push(row);
            }

            globals.set(self.item.as_str(), old_item)?;
            if let Some(index) = &self.index {
                globals.set(index.as_str(), old_index)?;
            }

            let flattened: Vec<&RNode> = self.rows.iter().flatten().collect();
            sync_expand_refs(&self.container, flattened);
            return Ok(());
        }

        let globals = inst.lua.globals();
        let old_item: Value = globals.get(self.item.as_str())?;
        let old_index: Value = match &self.index {
            Some(index) => globals.get(index.as_str())?,
            None => Value::Nil,
        };

        for (position, value) in values.into_iter().enumerate() {
            globals.set(self.item.as_str(), value)?;
            if let Some(index) = &self.index {
                globals.set(index.as_str(), position as i64 + 1)?;
            }
            inst.each_stack.push(position as i64 + 1);
            let result: Result<()> = (|| {
                for node in &mut self.rows[position] {
                    node.update(inst)?;
                }
                Ok(())
            })();
            inst.each_stack.pop();
            result?;
        }

        globals.set(self.item.as_str(), old_item)?;
        if let Some(index) = &self.index {
            globals.set(index.as_str(), old_index)?;
        }

        Ok(())
    }
}

fn build_nodes(
    inst: &mut Instance,
    nodes: &[Node],
    orientation: gtk::Orientation,
) -> Result<Vec<RNode>> {
    let mut out = Vec::with_capacity(nodes.len());

    for node in nodes {
        match node {
            Node::Text(raw) => {
                let text = inst.interp(raw)?;
                if text.trim().is_empty() {
                    continue;
                }
                let label = gtk::Label::new(Some(&text));
                label.set_xalign(0.0);
                out.push(RNode::Text(RText {
                    label,
                    raw: raw.clone(),
                    last: text,
                }));
            }

            Node::Element(element) => out.push(build_element(inst, element)?),

            Node::If {
                cond,
                then_body,
                else_body,
            } => {
                let container = gtk::Box::new(orientation, 0);
                let branch = inst.eval_bool(cond)?;
                let body = if branch { then_body } else { else_body };
                let children = build_nodes(inst, body, orientation)?;
                append_nodes(&container, &children);
                sync_expand(&container, &children);

                out.push(RNode::If(RIf {
                    container,
                    cond: cond.clone(),
                    branch: Some(branch),
                    children,
                    then_body: then_body.clone(),
                    else_body: else_body.clone(),
                    orientation,
                }));
            }

            Node::Each {
                list,
                item,
                index,
                body,
            } => {
                let container = gtk::Box::new(orientation, 0);
                let values = eval_list(inst, list)?;

                let globals = inst.lua.globals();
                let old_item: Value = globals.get(item.as_str())?;
                let old_index: Value = match index {
                    Some(index) => globals.get(index.as_str())?,
                    None => Value::Nil,
                };

                let mut rows = Vec::with_capacity(values.len());
                for (position, value) in values.into_iter().enumerate() {
                    globals.set(item.as_str(), value)?;
                    if let Some(index) = index {
                        globals.set(index.as_str(), position as i64 + 1)?;
                    }
                    inst.each_stack.push(position as i64 + 1);
                    let row = build_nodes(inst, body, orientation);
                    inst.each_stack.pop();
                    let row = row?;
                    append_nodes(&container, &row);
                    rows.push(row);
                }

                globals.set(item.as_str(), old_item)?;
                if let Some(index) = index {
                    globals.set(index.as_str(), old_index)?;
                }

                let flattened: Vec<&RNode> = rows.iter().flatten().collect();
                sync_expand_refs(&container, flattened);

                out.push(RNode::Each(REach {
                    container,
                    list: list.clone(),
                    item: item.clone(),
                    index: index.clone(),
                    body: body.clone(),
                    rows,
                    orientation,
                }));
            }
        }
    }

    Ok(out)
}

fn build_element(inst: &mut Instance, element: &TplElement) -> Result<RNode> {
    let mut common = Common::default();

    let (widget, kind, children): (gtk::Widget, RKind, Vec<RNode>) = match element.tag.as_str() {
        "box" | "row" => {
            let orientation = if element.tag == "row" {
                gtk::Orientation::Horizontal
            } else {
                gtk::Orientation::Vertical
            };
            let spacing = if let Some(gap) = element.attr("gap") {
                eval_num(inst, gap)?.unwrap_or(0.0) as i32
            } else {
                0
            };

            let container = gtk::Box::new(orientation, spacing);
            let children = build_nodes(inst, &element.children, orientation)?;
            append_nodes(&container, &children);
            sync_expand(&container, &children);

            (container.upcast(), RKind::Container, children)
        }

        "grid" => {
            let columns = match element.attr("columns") {
                Some(raw) => eval_num(inst, raw)?.unwrap_or(1.0) as i32,
                None => 1,
            }
            .max(1) as usize;
            let spacing = match element.attr("gap") {
                Some(raw) => eval_num(inst, raw)?.unwrap_or(0.0) as i32,
                None => 0,
            };

            let grid = gtk::Grid::new();
            grid.set_column_homogeneous(true);
            grid.set_hexpand(true);
            grid.set_column_spacing(spacing.max(0) as u32);
            grid.set_row_spacing(spacing.max(0) as u32);

            let children = build_nodes(inst, &element.children, gtk::Orientation::Vertical)?;
            for (index, node) in children.iter().enumerate() {
                let column = (index % columns) as i32;
                let row = (index / columns) as i32;
                for widget in node.widgets() {
                    grid.attach(&widget, column, row, 1, 1);
                }
            }

            (grid.upcast(), RKind::Container, children)
        }

        "label" => {
            let raw = match element.attr("text") {
                Some(text) => text.to_string(),
                None => raw_children_text(&element.children),
            };
            let text = inst.interp(&raw)?;

            let markdown = element
                .attr("markdown")
                .is_some_and(|value| value == "true");
            let label = gtk::Label::new(Some(&text));
            if markdown {
                label.set_markup(&crate::markdown::to_pango(&text));
            }
            if element.attr("wrap").is_some_and(|value| value == "true") {
                label.set_wrap(true);
            }
            if element
                .attr("selectable")
                .is_some_and(|value| value == "true")
            {
                label.set_selectable(true);
            }
            label.set_xalign(match element.attr("align") {
                Some("start" | "left") => 0.0,
                Some("end" | "right") => 1.0,
                Some("center") => 0.5,
                _ => 0.0,
            });

            (
                label.clone().upcast(),
                RKind::Label {
                    label,
                    raw,
                    last: text,
                    markdown,
                },
                Vec::new(),
            )
        }

        "bar" => {
            let value = element.attr("value").unwrap_or("0").to_string();
            let max = element.attr("max").map(str::to_string);

            let bar = gtk::ProgressBar::new();
            bar.set_show_text(false);

            let current = eval_num(inst, &value)?.unwrap_or(0.0);
            let limit = match &max {
                Some(expr) => eval_num(inst, expr)?
                    .unwrap_or(100.0)
                    .abs()
                    .max(f64::EPSILON),
                None => 100.0,
            };
            let fraction = (current / limit).clamp(0.0, 1.0);
            bar.set_fraction(fraction);

            (
                bar.clone().upcast(),
                RKind::Bar {
                    bar,
                    value,
                    max,
                    last: fraction,
                },
                Vec::new(),
            )
        }

        "slider" => {
            let min = number_attr(inst, element, "min")?.unwrap_or(0.0);
            let max = number_attr(inst, element, "max")?.unwrap_or(100.0);
            let step = number_attr(inst, element, "step")?.unwrap_or(1.0);
            let value_raw = element.attr("value").unwrap_or("0").to_string();
            let value = eval_num(inst, &value_raw)?.unwrap_or(min);

            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
            scale.set_value(value);
            scale.set_draw_value(false);
            scale.set_hexpand(true);
            scale.set_valign(gtk::Align::Center);

            let suppress = Rc::new(Cell::new(false));
            if let Some(code) = element.attr("on:change").map(str::to_string) {
                let tx = inst.tx.clone();
                let key = inst.key.clone();
                let guard = suppress.clone();
                scale.connect_value_changed(move |scale| {
                    if guard.get() {
                        return;
                    }
                    let _ = tx.try_send(Msg::Change {
                        key: key.clone(),
                        code: code.clone(),
                        value: scale.value(),
                    });
                });
            }

            (
                scale.clone().upcast(),
                RKind::Slider {
                    scale,
                    value: value_raw,
                    last: value,
                    suppress,
                },
                Vec::new(),
            )
        }

        "button" => {
            let button = gtk::Button::new();

            let explicit = element.attr("text").map(str::to_string);
            let implicit = if explicit.is_none() {
                let only_text = element
                    .children
                    .iter()
                    .all(|node| matches!(node, Node::Text(_)));
                let text = raw_children_text(&element.children);
                if only_text && !text.is_empty() {
                    Some(text)
                } else {
                    None
                }
            } else {
                None
            };

            match explicit.or(implicit) {
                Some(raw) => {
                    let text = inst.interp(&raw)?;
                    button.set_label(&text);
                    (
                        button.clone().upcast(),
                        RKind::Button {
                            button,
                            text: Some(raw),
                            last: text,
                        },
                        Vec::new(),
                    )
                }
                None => {
                    let children =
                        build_nodes(inst, &element.children, gtk::Orientation::Horizontal)?;
                    if children.len() == 1 {
                        for widget in children[0].widgets() {
                            button.set_child(Some(&widget));
                        }
                    } else {
                        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                        append_nodes(&row, &children);
                        button.set_child(Some(&row));
                    }
                    (
                        button.clone().upcast(),
                        RKind::Button {
                            button,
                            text: None,
                            last: String::new(),
                        },
                        children,
                    )
                }
            }
        }

        "image" => {
            let src = element.attr("src").unwrap_or("").to_string();
            let path = inst.interp(&src)?;

            let picture = gtk::Picture::new();
            picture.set_content_fit(match element.attr("fit") {
                Some("cover") => gtk::ContentFit::Cover,
                Some("fill") => gtk::ContentFit::Fill,
                _ => gtk::ContentFit::Contain,
            });
            picture.set_can_shrink(true);
            set_picture(inst, &picture, &path);

            (
                picture.clone().upcast(),
                RKind::Image {
                    picture,
                    src,
                    last: path,
                },
                Vec::new(),
            )
        }

        "entry" => {
            let raw = element.attr("text").unwrap_or("").to_string();
            let text = inst.interp(&raw)?;

            let entry = gtk::Entry::new();
            entry.set_hexpand(true);
            entry.set_text(&text);

            if let Some(placeholder) = element.attr("placeholder") {
                let placeholder = inst.interp(placeholder)?;
                entry.set_placeholder_text(Some(&placeholder));
            }

            let suppress = Rc::new(Cell::new(false));
            if let Some(code) = element.attr("on:change").map(str::to_string) {
                let tx = inst.tx.clone();
                let key = inst.key.clone();
                let guard = suppress.clone();
                entry.connect_changed(move |entry| {
                    if guard.get() {
                        return;
                    }
                    let _ = tx.try_send(Msg::Text {
                        key: key.clone(),
                        code: code.clone(),
                        value: entry.text().to_string(),
                    });
                });
            }
            if let Some(code) = element.attr("on:submit").map(str::to_string) {
                let tx = inst.tx.clone();
                let key = inst.key.clone();
                entry.connect_activate(move |entry| {
                    let _ = tx.try_send(Msg::Text {
                        key: key.clone(),
                        code: code.clone(),
                        value: entry.text().to_string(),
                    });
                });
            }

            (
                entry.clone().upcast(),
                RKind::Entry {
                    entry,
                    text: raw,
                    last: text,
                    suppress,
                },
                Vec::new(),
            )
        }

        "select" => {
            let options = element.attr("options").unwrap_or("{}").to_string();
            let selected = element.attr("selected").unwrap_or("\"\"").to_string();
            let values: Vec<String> = eval_list(inst, &options)?
                .iter()
                .map(value_to_string)
                .collect();
            let selected_value = value_to_string(&inst.eval_value(&selected)?);
            let index = values.iter().position(|value| *value == selected_value);

            let dropdown = {
                let refs: Vec<&str> = values.iter().map(String::as_str).collect();
                gtk::DropDown::from_strings(&refs)
            };
            dropdown.set_hexpand(true);
            if values.is_empty() {
                dropdown.set_selected(gtk::INVALID_LIST_POSITION);
            } else {
                dropdown.set_selected(index.unwrap_or(0) as u32);
            }

            let suppress = Rc::new(Cell::new(false));
            if let Some(code) = element.attr("on:change").map(str::to_string) {
                let tx = inst.tx.clone();
                let key = inst.key.clone();
                let guard = suppress.clone();
                dropdown.connect_selected_notify(move |dropdown| {
                    if guard.get() {
                        return;
                    }
                    let value = dropdown
                        .selected_item()
                        .and_then(|item| item.downcast::<gtk::StringObject>().ok())
                        .map(|item| item.string().to_string())
                        .unwrap_or_default();
                    let _ = tx.try_send(Msg::Text {
                        key: key.clone(),
                        code: code.clone(),
                        value,
                    });
                });
            }

            let last_selected = if values.is_empty() {
                None
            } else {
                Some(values[index.unwrap_or(0)].clone())
            };

            (
                dropdown.clone().upcast(),
                RKind::Select {
                    dropdown,
                    options,
                    selected,
                    last_options: values,
                    last_selected,
                    suppress,
                },
                Vec::new(),
            )
        }

        "scroll" => {
            let scrolled = gtk::ScrolledWindow::new();
            scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scrolled.set_hexpand(true);

            if let Some(max_height) = number_attr(inst, element, "max-height")? {
                scrolled.set_propagate_natural_height(true);
                scrolled.set_max_content_height((max_height as i32).max(1));
            }

            let children = build_nodes(inst, &element.children, gtk::Orientation::Vertical)?;
            if children.len() == 1 {
                let widgets = children[0].widgets();
                if widgets.len() == 1 {
                    scrolled.set_child(Some(&widgets[0]));
                } else {
                    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    for widget in widgets {
                        container.append(&widget);
                    }
                    scrolled.set_child(Some(&container));
                }
            } else if !children.is_empty() {
                let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
                append_nodes(&container, &children);
                scrolled.set_child(Some(&container));
            }

            if element.attr("follow").is_some_and(|value| value == "true") {
                let adjustment = scrolled.vadjustment();
                let tracking = Rc::new(Cell::new(true));

                let position = tracking.clone();
                adjustment.connect_value_changed(move |adjustment| {
                    let at_bottom =
                        adjustment.value() + adjustment.page_size() >= adjustment.upper() - 1.0;
                    position.set(at_bottom);
                });

                adjustment.connect_changed(move |adjustment| {
                    if tracking.get() {
                        let bottom = (adjustment.upper() - adjustment.page_size()).max(0.0);
                        adjustment.set_value(bottom);
                    }
                });
            }

            (scrolled.upcast(), RKind::Scroll, children)
        }

        "spacer" => {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_hexpand(true);
            if element
                .attr("vertical")
                .is_some_and(|value| value == "true")
            {
                spacer.set_vexpand(true);
            }
            (spacer.upcast(), RKind::Static, Vec::new())
        }

        "sep" => (
            gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
            RKind::Static,
            Vec::new(),
        ),

        other => {
            eprintln!("nitroget: unknown element <{other}>");
            let label = gtk::Label::new(Some(&format!("<{other}>")));
            label.add_css_class("nitroget-unknown");
            (label.upcast(), RKind::Static, Vec::new())
        }
    };

    apply_common(inst, &widget, element, &mut common)?;

    Ok(RNode::Element(RElement {
        widget,
        kind,
        children,
        class_expr: common.class_expr,
        classes: common.classes,
        visible_expr: common.visible_expr,
        visible: common.visible,
        tooltip_expr: common.tooltip_expr,
        tooltip: common.tooltip,
    }))
}

#[derive(Default)]
struct Common {
    class_expr: Option<String>,
    classes: Vec<String>,
    visible_expr: Option<String>,
    visible: bool,
    tooltip_expr: Option<String>,
    tooltip: String,
}

fn apply_common(
    inst: &Instance,
    widget: &gtk::Widget,
    element: &TplElement,
    common: &mut Common,
) -> Result<()> {
    widget.add_css_class(&element.tag);

    if let Some(raw) = element.attr("class") {
        let value = inst.interp(raw)?;
        let classes: Vec<String> = value.split_whitespace().map(str::to_string).collect();
        for class in &classes {
            widget.add_css_class(class);
        }
        common.class_expr = Some(raw.to_string());
        common.classes = classes;
    }

    if let Some(id) = element.attr("id") {
        widget.set_widget_name(id);
    }

    let width = number_attr(inst, element, "width")?.map(|value| value as i32);
    let height = number_attr(inst, element, "height")?.map(|value| value as i32);
    if width.is_some() || height.is_some() {
        widget.set_size_request(width.unwrap_or(-1), height.unwrap_or(-1));
    }

    if element.attr("expand").is_some_and(|value| value == "true") {
        widget.set_hexpand(true);
        widget.set_vexpand(true);
    }
    if element.attr("hexpand").is_some_and(|value| value == "true") {
        widget.set_hexpand(true);
    }
    if element.attr("vexpand").is_some_and(|value| value == "true") {
        widget.set_vexpand(true);
    }

    if let Some(align) = element.attr("halign") {
        widget.set_halign(parse_align(align));
    }
    if let Some(align) = element.attr("valign") {
        widget.set_valign(parse_align(align));
    }

    if let Some(raw) = element.attr("tooltip") {
        let tooltip = inst.interp(raw)?;
        widget.set_tooltip_text(Some(&tooltip));
        common.tooltip_expr = Some(raw.to_string());
        common.tooltip = tooltip;
    }

    if let Some(raw) = element.attr("visible") {
        let visible = inst.eval_bool(raw)?;
        widget.set_visible(visible);
        common.visible_expr = Some(raw.to_string());
        common.visible = visible;
    }

    if let Some(raw) = element.attr("cursor") {
        let name = raw.trim();
        widget.set_cursor_from_name((!name.is_empty()).then_some(name));
    }

    if let Some(code) = element.attr("on:click").map(str::to_string) {
        let tx = inst.tx.clone();
        let key = inst.key.clone();
        let index = inst.each_stack.last().copied();

        if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
            {
                let tx = tx.clone();
                let key = key.clone();
                let code = code.clone();
                let widget = widget.clone();
                button.connect_clicked(move |_| {
                    let (x, y) = widget_center(&widget);
                    let _ = tx.try_send(Msg::Click {
                        key: key.clone(),
                        code: code.clone(),
                        index,
                        button: 1,
                        x,
                        y,
                    });
                });
            }

            let tx = tx.clone();
            let key = key.clone();
            let code = code.clone();
            let widget = widget.clone();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);
            gesture.connect_released(move |gesture, _, x, y| {
                let (x, y) = window_point(&widget, x, y);
                let _ = tx.try_send(Msg::Click {
                    key: key.clone(),
                    code: code.clone(),
                    index,
                    button: gesture.current_button(),
                    x,
                    y,
                });
            });
            button.add_controller(gesture);
        } else {
            let clicked = widget.clone();
            let primary = gtk::GestureClick::new();
            primary.set_button(1);
            primary.connect_pressed({
                let tx = tx.clone();
                let key = key.clone();
                let code = code.clone();
                move |gesture, _, x, y| {
                    let (x, y) = window_point(&clicked, x, y);
                    let _ = tx.try_send(Msg::Click {
                        key: key.clone(),
                        code: code.clone(),
                        index,
                        button: gesture.current_button(),
                        x,
                        y,
                    });
                }
            });
            widget.add_controller(primary);

            let clicked = widget.clone();
            let secondary = gtk::GestureClick::new();
            secondary.set_button(3);
            secondary.connect_released(move |gesture, _, x, y| {
                let (x, y) = window_point(&clicked, x, y);
                let _ = tx.try_send(Msg::Click {
                    key: key.clone(),
                    code: code.clone(),
                    index,
                    button: gesture.current_button(),
                    x,
                    y,
                });
            });
            widget.add_controller(secondary);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------- instance

impl Instance {
    pub fn render_now(&mut self) {
        let error = self.runtime.borrow().error.clone();
        if let Some(error) = error {
            self.show_error(&error);
            return;
        }

        self.set_output_context();
        self.each_stack.clear();

        if let Some(mut tree) = self.tree.take() {
            if let Err(e) = tree.update(self) {
                eprintln!(
                    "nitroget: render error in {}: {e:#}",
                    self.comp.path.display()
                );
            }
            self.tree = Some(tree);
            self.sync_keyboard_mode();
            return;
        }

        match self.build_tree() {
            Ok(tree) => {
                self.window.set_child(Some(&tree.surface));
                self.tree = Some(tree);
                self.sync_keyboard_mode();
            }
            Err(e) => {
                eprintln!(
                    "nitroget: render error in {}: {e:#}",
                    self.comp.path.display()
                );
                let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
                root.add_css_class("nitroget-error");
                let label = gtk::Label::new(Some(&format!("{e}")));
                label.set_wrap(true);
                root.append(&label);
                self.window.set_child(Some(&root));
                self.tree = None;
                self.sync_keyboard_mode();
            }
        }
    }

    fn build_tree(&mut self) -> Result<Tree> {
        let scope = self.scope_class();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("nitroget-root");
        root.add_css_class(&scope);

        let template = self.comp.template.clone();
        let nodes = build_nodes(self, &template, gtk::Orientation::Vertical)?;
        for node in &nodes {
            for widget in node.widgets() {
                root.append(&widget);
            }
        }

        // A single top-level element is the widget's visible surface. Let it
        // fill the configured window; otherwise its background can be smaller
        // than the layer surface and placement outlines no longer match the
        // widget on screen.
        if nodes.len() == 1 {
            for widget in nodes[0].widgets() {
                widget.set_hexpand(true);
                widget.set_vexpand(true);
            }
        }

        let root_widget: gtk::Widget = root.clone().upcast();
        let surface: gtk::Widget = if matches!(self.comp.kind, ComponentKind::Panel(_)) {
            let scrolled = gtk::ScrolledWindow::new();
            scrolled.set_has_frame(false);
            scrolled.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
            scrolled.set_hexpand(true);
            scrolled.set_vexpand(true);
            scrolled.set_child(Some(&root));

            let controllers: Vec<gtk::EventController> = scrolled
                .observe_controllers()
                .iter::<gtk::EventController>()
                .filter_map(Result::ok)
                .collect();
            for controller in controllers {
                scrolled.remove_controller(&controller);
            }

            scrolled.upcast()
        } else {
            root_widget.clone()
        };

        Ok(Tree {
            root: root_widget,
            surface,
            nodes,
        })
    }

    // ---------------------------------------------------------- evaluation

    pub fn eval_value(&self, expr: &str) -> Result<Value> {
        let code = format!("return ({expr})");
        Ok(self.lua.load(&code).set_name(expr).eval::<Value>()?)
    }

    fn eval_bool(&self, expr: &str) -> Result<bool> {
        Ok(match self.eval_value(expr)? {
            Value::Nil => false,
            Value::Boolean(value) => value,
            Value::Integer(value) => value != 0,
            Value::Number(value) => value != 0.0,
            Value::String(value) => !value.to_string_lossy().is_empty(),
            _ => true,
        })
    }

    /// Replaces `{ expression }` segments with their Lua value.
    pub fn interp(&self, raw: &str) -> Result<String> {
        let mut out = String::with_capacity(raw.len());
        let mut i = 0;

        while i < raw.len() {
            let rest = &raw[i..];
            let current = rest.chars().next().unwrap();

            if current == '\\' && rest[1..].starts_with('{') {
                out.push('{');
                i += 2;
                continue;
            }

            if current == '{'
                && let Some(end) = rest[1..].find('}')
            {
                let expr = &rest[1..1 + end];
                let value = self.eval_value(expr)?;
                out.push_str(&value_to_string(&value));
                i += 1 + end + 1;
                continue;
            }

            out.push(current);
            i += current.len_utf8();
        }

        Ok(out)
    }

    pub fn texture(&mut self, path: &str) -> Option<gtk::gdk::Texture> {
        let path = expand_tilde(path);
        if let Some(texture) = self.textures.get(&path) {
            return Some(texture.clone());
        }

        match gtk::gdk::Texture::from_filename(&path) {
            Ok(texture) => {
                if self.textures.len() >= 32 {
                    self.textures.clear();
                }
                self.textures.insert(path.clone(), texture.clone());
                Some(texture)
            }
            Err(e) => {
                eprintln!("nitroget: cannot load image {path}: {e}");
                None
            }
        }
    }
}

// -------------------------------------------------------------------- utils

fn append_nodes(container: &gtk::Box, nodes: &[RNode]) {
    for node in nodes {
        for widget in node.widgets() {
            container.append(&widget);
        }
    }
}

fn sync_expand(container: &gtk::Box, nodes: &[RNode]) {
    let widgets: Vec<gtk::Widget> = nodes.iter().flat_map(|node| node.widgets()).collect();
    container.set_hexpand(widgets.iter().any(|widget| widget.hexpands()));
    container.set_vexpand(widgets.iter().any(|widget| widget.vexpands()));
}

fn sync_expand_refs(container: &gtk::Box, nodes: Vec<&RNode>) {
    let widgets: Vec<gtk::Widget> = nodes.iter().flat_map(|node| node.widgets()).collect();
    container.set_hexpand(widgets.iter().any(|widget| widget.hexpands()));
    container.set_vexpand(widgets.iter().any(|widget| widget.vexpands()));
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn raw_children_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        if let Node::Text(text) = node {
            out.push_str(text);
        }
    }
    out.trim().to_string()
}

fn eval_list(inst: &Instance, expr: &str) -> Result<Vec<Value>> {
    let value = inst.eval_value(expr)?;
    match value {
        Value::Table(table) => {
            let length = table.raw_len();
            if length > 0 {
                let mut out = Vec::with_capacity(length);
                for index in 1..=length {
                    out.push(table.raw_get(index)?);
                }
                Ok(out)
            } else {
                let pairs = table
                    .pairs::<Value, Value>()
                    .collect::<mlua::Result<Vec<_>>>()?;
                Ok(pairs.into_iter().map(|(_, value)| value).collect())
            }
        }
        _ => Ok(Vec::new()),
    }
}

fn eval_num(inst: &Instance, raw: &str) -> Result<Option<f64>> {
    let raw = raw.trim();
    if let Some(inner) = raw
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    {
        return Ok(value_to_number(&inst.eval_value(inner)?));
    }
    Ok(raw.parse::<f64>().ok())
}

fn number_attr(inst: &Instance, element: &TplElement, name: &str) -> Result<Option<f64>> {
    match element.attr(name) {
        Some(raw) => eval_num(inst, raw),
        None => Ok(None),
    }
}

fn set_picture(inst: &mut Instance, picture: &gtk::Picture, path: &str) {
    if path.trim().is_empty() {
        picture.set_paintable(None::<&gtk::gdk::Texture>);
        return;
    }

    match inst.texture(path) {
        Some(texture) => picture.set_paintable(Some(&texture)),
        None => picture.set_paintable(None::<&gtk::gdk::Texture>),
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Nil => String::new(),
        Value::Boolean(boolean) => boolean.to_string(),
        Value::Integer(integer) => integer.to_string(),
        Value::Number(number) => format_number(*number),
        Value::String(string) => string.to_string_lossy().to_string(),
        _ => String::new(),
    }
}

fn value_to_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(integer) => Some(*integer as f64),
        Value::Number(number) => Some(*number),
        Value::String(string) => string.to_string_lossy().parse().ok(),
        Value::Boolean(boolean) => Some(if *boolean { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn format_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        format!("{}", (number * 100.0).round() / 100.0)
    }
}

fn parse_align(value: &str) -> gtk::Align {
    match value {
        "start" | "left" => gtk::Align::Start,
        "end" | "right" => gtk::Align::End,
        "center" => gtk::Align::Center,
        _ => gtk::Align::Fill,
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

fn window_point(widget: &gtk::Widget, x: f64, y: f64) -> (f64, f64) {
    let Some(root) = widget.root() else {
        return (x, y);
    };
    widget
        .compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
        .map(|point| (point.x() as f64, point.y() as f64))
        .unwrap_or((x, y))
}

fn widget_center(widget: &gtk::Widget) -> (f64, f64) {
    let Some(root) = widget.root() else {
        return (0.0, 0.0);
    };
    widget
        .compute_bounds(&root)
        .map(|bounds| {
            (
                (bounds.x() + bounds.width() / 2.0) as f64,
                (bounds.y() + bounds.height() / 2.0) as f64,
            )
        })
        .unwrap_or((0.0, 0.0))
}
