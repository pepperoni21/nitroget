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

/// Fallbacks for automatic layout when a widget leaves a dimension to GTK.
pub const DEFAULT_WIDTH: i32 = 320;
pub const DEFAULT_HEIGHT: i32 = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Set by `config.toml` or the automatic layout, never by the widget.
    pub anchor: Option<String>,
    /// Set by `config.toml` or the automatic layout, never by the widget.
    pub offset: Option<(i32, i32)>,
    /// Intrinsic size, with an optional `config.toml` override. `None` lets
    /// GTK use the natural/minimum content size.
    pub width: Option<i32>,
    pub height: Option<i32>,
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
            width: None,
            height: None,
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
    pub lua_dirs: Vec<PathBuf>,
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

    let (script, script_dir, src) = match extract_block(src, "script") {
        Some(block) => {
            let body = block.body;
            match attr_value(&block.attrs, "src") {
                Some(script_src) => {
                    if !body.trim().is_empty() {
                        bail!("<script src=\"{script_src}\"> cannot also contain inline code");
                    }
                    let file = resolve_script(&script_src, path)?;
                    let content = std::fs::read_to_string(&file)
                        .with_context(|| format!("cannot read script {}", file.display()))?;
                    (content, Some(widget_dir(&file)), block.rest)
                }
                None => (body, None, block.rest),
            }
        }
        None => (String::new(), None, src.to_string()),
    };
    let (css, src) = match extract_block(&src, "style") {
        Some(block) => (block.body, block.rest),
        None => (String::new(), src),
    };

    let root_dir = widget_dir(path);
    let mut lua_dirs = script_dir.into_iter().collect::<Vec<_>>();
    if !lua_dirs.contains(&root_dir) {
        lua_dirs.push(root_dir);
    }

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
        lua_dirs,
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
        let width: i32 = w.parse().map_err(|_| anyhow!("width must be a number"))?;
        if width <= 0 {
            bail!("width must be positive");
        }
        p.width = Some(width);
    }
    if let Some(h) = root.attr("height") {
        let height: i32 = h.parse().map_err(|_| anyhow!("height must be a number"))?;
        if height <= 0 {
            bail!("height must be positive");
        }
        p.height = Some(height);
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

struct Block {
    attrs: String,
    body: String,
    rest: String,
}

fn extract_block(src: &str, tag: &str) -> Option<Block> {
    let open = format!("<{tag}");
    let start = src.find(&open)?;

    let after_name = start + open.len();
    if let Some(c) = src[after_name..].chars().next()
        && !c.is_whitespace()
        && c != '>'
        && c != '/'
    {
        return None;
    }

    let gt = src[after_name..].find('>')? + after_name;
    let attrs = src[after_name..gt].to_string();
    let self_closing = attrs.trim_end().ends_with('/');
    let body_start = gt + 1;

    let (body, end) = if self_closing {
        (String::new(), body_start)
    } else {
        let close = format!("</{tag}>");
        let body_end = src[body_start..].find(&close)? + body_start;
        (
            src[body_start..body_end].to_string(),
            body_end + close.len(),
        )
    };

    let mut rest = String::with_capacity(src.len() - (end - start));
    rest.push_str(&src[..start]);
    rest.push_str(&src[end..]);

    Some(Block { attrs, body, rest })
}

fn attr_value(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }

        let key_start = i;
        while i < bytes.len()
            && !(bytes[i] as char).is_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if key_start == i {
            i += 1;
            continue;
        }
        let key = &attrs[key_start..i];

        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }

        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                i += 1;
            }

            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let value_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let value = &attrs[value_start..i];
                if i < bytes.len() {
                    i += 1;
                }
                if key == name {
                    return Some(value.to_string());
                }
            } else {
                let value_start = i;
                while i < bytes.len() && !(bytes[i] as char).is_whitespace() && bytes[i] != b'/' {
                    i += 1;
                }
                if key == name {
                    return Some(attrs[value_start..i].to_string());
                }
            }
        } else if key == name {
            return Some(String::new());
        }
    }

    None
}

fn widget_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn resolve_script(value: &str, path: &Path) -> Result<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        bail!("<script src> is empty");
    }

    let relative = Path::new(value);
    if relative.is_absolute() {
        bail!("<script src> must be relative to the widget file");
    }
    for component in relative.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => bail!("<script src> must stay inside the widget folder"),
        }
    }

    let base = widget_dir(path);
    let file = base.join(relative);
    let base_real = std::fs::canonicalize(&base)
        .with_context(|| format!("cannot resolve {}", base.display()))?;
    let file_real = std::fs::canonicalize(&file)
        .with_context(|| format!("cannot resolve script {}", file.display()))?;
    if !file_real.starts_with(&base_real) {
        bail!("<script src> must stay inside the widget folder");
    }

    let meta = std::fs::symlink_metadata(&file)
        .with_context(|| format!("cannot inspect script {}", file.display()))?;
    if meta.file_type().is_symlink() {
        bail!("{} is a symlink; refusing to load it", file.display());
    }
    if !meta.file_type().is_file() {
        bail!("{} is not a regular file", file.display());
    }
    if meta.len() > MAX_COMPONENT_BYTES {
        bail!(
            "{} is too large (limit {MAX_COMPONENT_BYTES} bytes)",
            file.display()
        );
    }

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_dimensions_are_automatic() {
        let path = Path::new("auto.nitroget");
        let comp = parse("<widget name=\"auto\"></widget>", path).unwrap();
        assert_eq!(comp.placement.width, None);
        assert_eq!(comp.placement.height, None);

        let comp = parse("<widget name=\"auto\" width=\"200\"></widget>", path).unwrap();
        assert_eq!(comp.placement.width, Some(200));
        assert_eq!(comp.placement.height, None);

        let comp = parse("<widget name=\"auto\" height=\"80\"></widget>", path).unwrap();
        assert_eq!(comp.placement.width, None);
        assert_eq!(comp.placement.height, Some(80));
    }

    #[test]
    fn reads_script_src_attribute() {
        assert_eq!(
            attr_value(" src=\"a.lua\"", "src").as_deref(),
            Some("a.lua")
        );
        assert_eq!(
            attr_value(" src='a.lua' /", "src").as_deref(),
            Some("a.lua")
        );
        assert_eq!(attr_value(" other=\"x\"", "src"), None);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nitroget-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_external_script_and_module_dirs() {
        let dir = temp_dir("external-script");
        let scripts = dir.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("main.lua"), "value = 1").unwrap();

        let path = dir.join("widget.nitroget");
        let comp = parse(
            "<widget name=\"widget\"></widget>\n<script src=\"scripts/main.lua\"></script>",
            &path,
        )
        .unwrap();

        assert_eq!(comp.script, "value = 1");
        assert!(comp.lua_dirs.contains(&dir));
        assert!(comp.lua_dirs.contains(&scripts));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn accepts_self_closing_external_script() {
        let dir = temp_dir("self-closing-script");
        std::fs::write(dir.join("main.lua"), "value = 2").unwrap();

        let path = dir.join("widget.nitroget");
        let comp = parse(
            "<widget name=\"widget\"></widget>\n<script src=\"main.lua\" />",
            &path,
        )
        .unwrap();

        assert_eq!(comp.script, "value = 2");
        assert_eq!(comp.lua_dirs, vec![dir.clone()]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_inline_body_with_external_script() {
        let dir = temp_dir("inline-and-src");
        let path = dir.join("widget.nitroget");
        let error = parse(
            "<widget name=\"widget\"></widget>\n<script src=\"main.lua\">value = 1</script>",
            &path,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("cannot also contain inline code")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_script_outside_widget_folder() {
        let dir = temp_dir("outside-script");
        let path = dir.join("widget.nitroget");
        let error = parse(
            "<widget name=\"widget\"></widget>\n<script src=\"../main.lua\"></script>",
            &path,
        )
        .unwrap_err();

        assert!(error.to_string().contains("inside the widget folder"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
