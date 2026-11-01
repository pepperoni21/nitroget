//! `.nitroget` component files.
//!
//! A widget declares *what* it is (template, styles, script, size, layer).
//! *Where* it lives is machine-local and belongs in `config.toml`.
//!
//! ```text
//! <widget name="clock" width="320" height="150">
//!   <box class="card">
//!     <label class="time">{time}</label>
//!   </box>
//! </widget>
//!
//! <style>
//!   .card { background-color: rgba(0, 0, 0, 0.6); }
//! </style>
//!
//! <script>
//!   time = os.date("%H:%M")
//!   nitroget.every(1000, function() time = os.date("%H:%M") end)
//! </script>
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::template::{self, Element, Node};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Set by `config.toml` or the automatic layout, never by the widget.
    pub anchor: Option<String>,
    /// Set by `config.toml` or the automatic layout, never by the widget.
    pub offset: Option<(i32, i32)>,
    /// Intrinsic size, with an optional `config.toml` override.
    pub width: i32,
    pub height: i32,
    /// `bottom` by default; a widget may ask for `overlay`, etc.
    pub layer: String,
    /// Machine-local monitor selection; `None` lets the compositor choose.
    pub output: Option<String>,
    /// Concrete monitor index, set internally when `output = "all"` expands a
    /// widget into one copy per monitor.
    pub output_index: Option<i32>,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            anchor: None,
            offset: None,
            width: 320,
            height: 160,
            layer: "bottom".into(),
            output: None,
            output_index: None,
        }
    }
}

impl Placement {
    pub fn anchor(&self) -> &str {
        self.anchor.as_deref().unwrap_or("top-right")
    }

    pub fn offset(&self) -> (i32, i32) {
        self.offset.unwrap_or((40, 40))
    }
}

#[derive(Debug, Clone)]
pub struct Component {
    pub path: PathBuf,
    pub name: String,
    pub placement: Placement,
    pub template: Vec<Node>,
    pub css: String,
    pub script: String,
    /// Non-fatal remarks, shown by `nitroget check`.
    pub notes: Vec<String>,
}

/// Widget files are text. A cap keeps a malformed or hostile pack from
/// allocating an unbounded amount of memory before parsing starts.
const MAX_COMPONENT_BYTES: u64 = 2 * 1024 * 1024;

pub fn load(path: &Path) -> Result<Component> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect {}", path.display()))?;
    if meta.file_type().is_symlink() {
        bail!("{} is a symlink; refusing to load it", path.display());
    }
    if !meta.file_type().is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if meta.len() > MAX_COMPONENT_BYTES {
        bail!(
            "{} is too large (limit {MAX_COMPONENT_BYTES} bytes)",
            path.display()
        );
    }

    let src =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse(&src, path)
}

pub fn parse(src: &str, path: &Path) -> Result<Component> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);

    let (script, src) = match extract_block(src, "script") {
        Some((body, rest)) => (body, rest),
        None => (String::new(), src.to_string()),
    };
    let (css, src) = match extract_block(&src, "style") {
        Some((body, rest)) => (body, rest),
        None => (String::new(), src),
    };

    let nodes = template::parse(&src).with_context(|| format!("in {}", path.display()))?;

    let root = nodes
        .iter()
        .find_map(|n| match n {
            Node::Element(e) => Some(e.clone()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("no root element (expected <widget>)"))?;

    if root.tag != "widget" {
        bail!(
            "root element must be <widget>, found <{}> (in {})",
            root.tag,
            path.display()
        );
    }

    let name = root
        .attr("name")
        .map(str::to_string)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "widget".into());

    let (placement, notes) = placement_from(&root)?;

    Ok(Component {
        path: path.to_path_buf(),
        name,
        placement,
        template: root.children,
        css,
        script,
        notes,
    })
}

/// Parses the intrinsic properties. Placement attributes are rejected with a
/// note: they are machine-local and live in `config.toml`.
fn placement_from(root: &Element) -> Result<(Placement, Vec<String>)> {
    let mut p = Placement::default();
    let mut notes = Vec::new();

    for (attr, hint) in [
        ("anchor", "`anchor`"),
        ("offset", "`offset`"),
        ("output", "`output`"),
    ] {
        if root.attr(attr).is_some() {
            notes.push(format!(
                "{hint} is ignored in .nitroget files; set it in config.toml instead"
            ));
        }
    }

    if let Some(w) = root.attr("width") {
        p.width = w.parse().map_err(|_| anyhow!("width must be a number"))?;
        if p.width <= 0 {
            bail!("width must be positive");
        }
    }
    if let Some(h) = root.attr("height") {
        p.height = h.parse().map_err(|_| anyhow!("height must be a number"))?;
        if p.height <= 0 {
            bail!("height must be positive");
        }
    }

    if let Some(l) = root.attr("layer") {
        let l = l.trim().to_lowercase();
        if !["background", "bottom", "top", "overlay"].contains(&l.as_str()) {
            bail!("layer must be background, bottom, top or overlay");
        }
        p.layer = l;
    }

    Ok((p, notes))
}

pub fn is_valid_anchor(anchor: &str) -> bool {
    let anchor = anchor.trim().to_lowercase();
    if anchor.is_empty() {
        return false;
    }
    ["top", "bottom", "left", "right", "center"]
        .iter()
        .any(|k| anchor.contains(k))
}

pub fn is_valid_layer(layer: &str) -> bool {
    ["background", "bottom", "top", "overlay"].contains(&layer.trim().to_lowercase().as_str())
}

/// Removes a top-level `<tag ...> ... </tag>` block from the source and
/// returns its inner content plus the remaining source.
fn extract_block(src: &str, tag: &str) -> Option<(String, String)> {
    let open = format!("<{tag}");
    let start = src.find(&open)?;

    // Avoid matching `<scriptx`.
    let after_name = start + open.len();
    if let Some(c) = src[after_name..].chars().next()
        && !c.is_whitespace()
        && c != '>'
    {
        return None;
    }

    let gt = src[after_name..].find('>')? + after_name;
    let body_start = gt + 1;

    let close = format!("</{tag}>");
    let body_end = src[body_start..].find(&close)? + body_start;

    let body = src[body_start..body_end].to_string();
    let mut rest = String::with_capacity(src.len() - (body_end + close.len() - start));
    rest.push_str(&src[..start]);
    rest.push_str(&src[body_end + close.len()..]);

    Some((body, rest))
}
