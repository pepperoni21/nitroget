//! Placement for one widget folder: `<widget folder>/config.toml`.
//!
//! The config is optional; without it the engine
//! lays widgets out automatically.
//!
//! ```toml
//! [outputs]
//! main = "eDP-1"
//!
//! [widgets.clock]
//! anchor = "top-right"
//! offset = [40, 40]
//! output = "main"
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::component::{self, Component};

pub const FILE_NAME: &str = "config.toml";

/// Default margin from the screen edge and gap between stacked widgets.
const DEFAULT_MARGIN: i32 = 40;
const GAP: i32 = 16;

/// Corner order used when no placement is configured.
const SLOTS: [&str; 4] = ["top-right", "top-left", "bottom-left", "bottom-right"];

/// Values with engine meaning, never treated as machine aliases.
const RESERVED_OUTPUTS: [&str; 4] = ["all", "primary", "auto", "focused"];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    /// Machine-local monitor aliases, e.g. `main = "eDP-1"`. Placements can
    /// refer to an alias so they stay portable across machines.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    #[serde(default)]
    pub widgets: BTreeMap<String, Widget>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Default monitor for every widget: an alias, `primary`, `all`, a
    /// connector name, or omitted to let the compositor choose.
    pub output: Option<String>,
    /// Edge margin for automatically placed widgets.
    pub margin: Option<i32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Widget {
    pub enabled: Option<bool>,
    pub anchor: Option<String>,
    pub offset: Option<Offset>,
    pub output: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub layer: Option<String>,
    /// Per-monitor overrides, keyed by alias or connector. Used mainly by
    /// `output = "all"` so a monitor can have its own position.
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputOverride>,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OutputOverride {
    pub anchor: Option<String>,
    pub offset: Option<Offset>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(untagged)]
pub enum Offset {
    One(i32),
    Two(i32, i32),
}

impl Offset {
    fn pair(&self) -> (i32, i32) {
        match self {
            Offset::One(v) => (*v, *v),
            Offset::Two(x, y) => (*x, *y),
        }
    }
}

pub fn load(path: &Path) -> Result<Config> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "{} is a symlink; refusing to read placement config",
                path.display()
            );
        }
        Ok(meta) if !meta.file_type().is_file() => {
            bail!("{} is not a regular file", path.display());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot inspect {}", path.display()));
        }
    }

    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;

    let config: Config = toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
    config.validate()?;

    Ok(config)
}

impl Config {
    pub fn margin(&self) -> i32 {
        self.defaults.margin.unwrap_or(DEFAULT_MARGIN).max(0)
    }

    pub fn default_output(&self) -> Option<&str> {
        self.defaults.output.as_deref()
    }

    /// Resolves a monitor alias to its connector. Reserved values and unknown
    /// names pass through unchanged.
    pub fn resolve_output(&self, output: &str) -> String {
        if RESERVED_OUTPUTS.contains(&output) {
            return output.to_string();
        }
        self.outputs
            .get(output)
            .cloned()
            .unwrap_or_else(|| output.to_string())
    }

    /// The alias a connector is mapped to, if any.
    pub fn alias_for(&self, connector: &str) -> Option<&str> {
        self.outputs
            .iter()
            .find(|(_, target)| target.as_str() == connector)
            .map(|(alias, _)| alias.as_str())
    }

    /// The configured output for a widget (widget override, then defaults).
    pub fn output_for(&self, name: &str) -> Option<&str> {
        self.widgets
            .get(name)
            .and_then(|widget| widget.output.as_deref())
            .or_else(|| self.default_output())
    }

    pub fn is_all(&self, name: &str) -> bool {
        self.output_for(name) == Some("all")
    }

    /// Per-monitor override for a widget, looked up by alias and then connector.
    pub fn output_override(&self, name: &str, keys: &[&str]) -> Option<&OutputOverride> {
        let widget = self.widgets.get(name)?;
        keys.iter().find_map(|key| widget.outputs.get(*key))
    }

    /// Applies a per-monitor override, if one exists. Returns whether it did.
    pub fn apply_output_override(&self, comp: &mut Component, keys: &[&str]) -> bool {
        let Some(over) = self.output_override(&comp.name, keys) else {
            return false;
        };
        if let Some(anchor) = &over.anchor {
            comp.placement.anchor = Some(anchor.to_lowercase());
        }
        if let Some(offset) = &over.offset {
            comp.placement.offset = Some(offset.pair());
        }
        true
    }

    /// Applies per-widget overrides. Returns `false` when the widget is
    /// disabled in the config.
    pub fn apply(&self, comp: &mut Component) -> bool {
        let widget = self.widgets.get(&comp.name);

        if let Some(widget) = widget {
            if !widget.enabled.unwrap_or(true) {
                return false;
            }
            if let Some(anchor) = &widget.anchor {
                comp.placement.anchor = Some(anchor.to_lowercase());
            }
            if let Some(offset) = &widget.offset {
                comp.placement.offset = Some(offset.pair());
            }
            if let Some(width) = widget.width {
                comp.placement.width = Some(width);
            }
            if let Some(height) = widget.height {
                comp.placement.height = Some(height);
            }
            if let Some(layer) = &widget.layer {
                comp.placement.layer = layer.to_lowercase();
            }
        }

        let output = widget
            .and_then(|widget| widget.output.as_deref())
            .or_else(|| self.default_output());

        if let Some(output) = output {
            comp.placement.output = Some(self.resolve_output(output));
        }

        true
    }

    fn validate(&self) -> Result<()> {
        if let Some(margin) = self.defaults.margin
            && margin < 0
        {
            bail!("defaults.margin must be >= 0");
        }
        if let Some(output) = &self.defaults.output {
            validate_output(output, "defaults")?;
        }

        for (alias, target) in &self.outputs {
            if alias.trim().is_empty() {
                bail!("[outputs] alias must not be empty");
            }
            if RESERVED_OUTPUTS.contains(&alias.as_str()) {
                bail!("outputs.{alias}: `{alias}` is a reserved monitor value");
            }
            if target.trim().is_empty() {
                bail!("outputs.{alias} must not be empty");
            }
        }

        for (name, widget) in &self.widgets {
            if let Some(anchor) = &widget.anchor
                && !component::is_valid_anchor(anchor)
            {
                bail!("widgets.{name}.anchor: invalid anchor `{anchor}`");
            }
            if let Some(layer) = &widget.layer
                && !component::is_valid_layer(layer)
            {
                bail!("widgets.{name}.layer: must be background, bottom, top or overlay");
            }
            if let Some(offset) = &widget.offset {
                let (x, y) = offset.pair();
                if x < 0 || y < 0 {
                    bail!("widgets.{name}.offset must be >= 0");
                }
            }
            if let Some(width) = widget.width
                && width <= 0
            {
                bail!("widgets.{name}.width must be positive");
            }
            if let Some(height) = widget.height
                && height <= 0
            {
                bail!("widgets.{name}.height must be positive");
            }
            if let Some(output) = &widget.output {
                validate_output(output, name)?;
            }
            for (key, over) in &widget.outputs {
                if key.trim().is_empty() {
                    bail!("widgets.{name}.outputs: key must not be empty");
                }
                if let Some(anchor) = &over.anchor
                    && !component::is_valid_anchor(anchor)
                {
                    bail!("widgets.{name}.outputs.{key}.anchor: invalid anchor `{anchor}`");
                }
                if let Some(offset) = &over.offset {
                    let (x, y) = offset.pair();
                    if x < 0 || y < 0 {
                        bail!("widgets.{name}.outputs.{key}.offset must be >= 0");
                    }
                }
            }
        }

        Ok(())
    }
}

fn validate_output(output: &str, owner: &str) -> Result<()> {
    if output.trim().is_empty() {
        bail!("{owner}.output must not be empty");
    }
    Ok(())
}

/// One staged change written back by placement mode.
#[derive(Debug, Clone)]
pub struct PlacementChange {
    pub name: String,
    /// New base output for a single-output widget (`"main"`, `"DP-1"`, ...).
    pub output: Option<String>,
    /// When set, write into `[widgets.<name>.outputs.<key>]` instead.
    pub per_output: Option<String>,
    pub anchor: String,
    pub offset: (i32, i32),
}

/// Writes staged placements back to `config.toml`, preserving comments and
/// formatting through `toml_edit`.
pub fn save(path: &Path, changes: &[PlacementChange]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use toml_edit::{Array, DocumentMut, Item, Table, value};

    // Never follow a symlink or write through a special file. A widget pack
    // can otherwise ship `config.toml -> ~/.bashrc` and have placement mode
    // overwrite an unrelated file. The final rename also means a symlink that
    // appears after this check cannot redirect the write.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "{} is a symlink; refusing to write placement config",
                path.display()
            );
        }
        Ok(meta) if !meta.file_type().is_file() => {
            bail!("{} is not a regular file", path.display());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("cannot inspect {}", path.display()));
        }
    }

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };

    let mut doc: DocumentMut = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse()
            .with_context(|| format!("cannot parse {}", path.display()))?
    };

    if doc.get("widgets").is_none() {
        let mut table = Table::new();
        table.set_implicit(false);
        doc["widgets"] = Item::Table(table);
    }

    let widgets = doc["widgets"]
        .as_table_mut()
        .context("`widgets` must be a table")?;

    for change in changes {
        let widget_item = widgets
            .entry(&change.name)
            .or_insert_with(|| Item::Table(Table::new()));
        let widget = widget_item
            .as_table_mut()
            .with_context(|| format!("`widgets.{}` must be a table", change.name))?;

        if let Some(output) = &change.output {
            widget["output"] = value(output.as_str());
        }

        let table = match &change.per_output {
            Some(key) => {
                let outputs_item = widget
                    .entry("outputs")
                    .or_insert_with(|| Item::Table(Table::new()));
                let outputs = outputs_item.as_table_mut().with_context(|| {
                    format!("`widgets.{}.outputs` must be a table", change.name)
                })?;
                let item = outputs
                    .entry(key)
                    .or_insert_with(|| Item::Table(Table::new()));
                item.as_table_mut().with_context(|| {
                    format!("`widgets.{}.outputs.{key}` must be a table", change.name)
                })?
            }
            None => widget,
        };

        table["anchor"] = value(change.anchor.as_str());

        let mut offset = Array::new();
        offset.push(i64::from(change.offset.0));
        offset.push(i64::from(change.offset.1));
        table["offset"] = value(offset);
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".into());
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".{file_name}.{}.{unique}.tmp", std::process::id()));

    let result = (|| -> Result<()> {
        // `create_new` refuses an existing file or symlink, so a predictable
        // temp name cannot be used to redirect the write.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("cannot create {}", tmp.display()))?;
        file.write_all(doc.to_string().as_bytes())
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("cannot sync {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Assigns a corner and stacked offset to every widget that does not have an
/// explicit placement. Deterministic: widgets are expected to be in sorted
/// order, so the same set always produces the same desktop.
pub fn auto_place(components: &mut [Component], config: &Config) {
    let margin = config.margin();
    let mut heights = [0i32; SLOTS.len()];
    let mut next_slot = 0usize;

    for comp in components {
        if comp.placement.output.is_none()
            && let Some(output) = config.default_output()
        {
            comp.placement.output = Some(config.resolve_output(output));
        }

        if comp.placement.anchor.is_some() {
            if comp.placement.offset.is_none() {
                comp.placement.offset = Some((margin, margin));
            }
            continue;
        }

        let slot = next_slot % SLOTS.len();
        comp.placement.anchor = Some(SLOTS[slot].to_string());
        if comp.placement.offset.is_none() {
            comp.placement.offset = Some((margin, margin + heights[slot]));
        }

        heights[slot] += comp.placement.height.unwrap_or(component::DEFAULT_HEIGHT) + GAP;
        next_slot += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(name: &str, height: i32) -> Component {
        Component {
            path: std::path::PathBuf::from(format!("/w/{name}.nitroget")),
            name: name.into(),
            placement: component::Placement {
                height: Some(height),
                ..Default::default()
            },
            template: Vec::new(),
            css: String::new(),
            script: String::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn parses_offsets() {
        let config: Config = toml::from_str(
            r#"
            [widgets.a]
            offset = [10, 20]
            [widgets.b]
            offset = 30
            "#,
        )
        .unwrap();

        assert_eq!(
            config.widgets["a"].offset.as_ref().unwrap().pair(),
            (10, 20)
        );
        assert_eq!(
            config.widgets["b"].offset.as_ref().unwrap().pair(),
            (30, 30)
        );
    }

    #[test]
    fn auto_places_into_four_corners() {
        let config = Config::default();
        let mut widgets = vec![
            component("a", 100),
            component("b", 100),
            component("c", 100),
            component("d", 100),
        ];

        auto_place(&mut widgets, &config);

        assert_eq!(widgets[0].placement.anchor(), "top-right");
        assert_eq!(widgets[1].placement.anchor(), "top-left");
        assert_eq!(widgets[2].placement.anchor(), "bottom-left");
        assert_eq!(widgets[3].placement.anchor(), "bottom-right");
        assert_eq!(widgets[0].placement.offset(), (40, 40));
    }

    #[test]
    fn stacks_in_the_same_corner() {
        let config = Config::default();
        let mut widgets = vec![
            component("a", 100),
            component("b", 50),
            component("c", 100),
            component("d", 100),
            component("e", 80),
        ];

        auto_place(&mut widgets, &config);

        // Fifth widget goes back to the first corner, below the first one.
        assert_eq!(widgets[4].placement.anchor(), "top-right");
        assert_eq!(widgets[4].placement.offset(), (40, 40 + 100 + 16));
    }

    #[test]
    fn explicit_placement_is_kept() {
        let config: Config = toml::from_str(
            r#"
            [widgets.a]
            anchor = "center"
            offset = [0, 0]
            output = "DP-1"
            "#,
        )
        .unwrap();

        let mut comp = component("a", 100);
        assert!(config.apply(&mut comp));
        assert_eq!(comp.placement.anchor(), "center");
        assert_eq!(comp.placement.output.as_deref(), Some("DP-1"));

        let mut widgets = vec![comp];
        auto_place(&mut widgets, &config);
        assert_eq!(widgets[0].placement.anchor(), "center");
    }

    #[test]
    fn disabled_widgets_are_skipped() {
        let config: Config = toml::from_str("[widgets.a]\nenabled = false\n").unwrap();
        let mut comp = component("a", 100);
        assert!(!config.apply(&mut comp));
    }

    #[test]
    fn resolves_monitor_aliases() {
        let config: Config = toml::from_str(
            r#"
            [outputs]
            main = "eDP-1"
            side = "HDMI-A-1"

            [widgets.a]
            output = "main"
            "#,
        )
        .unwrap();
        config.validate().unwrap();

        let mut comp = component("a", 100);
        assert!(config.apply(&mut comp));
        assert_eq!(comp.placement.output.as_deref(), Some("eDP-1"));

        assert_eq!(config.resolve_output("side"), "HDMI-A-1");
        assert_eq!(config.resolve_output("all"), "all");
        assert_eq!(config.resolve_output("DP-3"), "DP-3");
        assert_eq!(config.alias_for("HDMI-A-1"), Some("side"));
    }

    #[test]
    fn applies_per_monitor_overrides() {
        let config: Config = toml::from_str(
            r#"
            [widgets.clock]
            anchor = "top-right"
            offset = [40, 40]
            output = "all"

            [widgets.clock.outputs.main]
            anchor = "top-left"
            offset = [10, 10]
            "#,
        )
        .unwrap();
        config.validate().unwrap();

        let mut comp = component("clock", 100);
        assert!(config.apply(&mut comp));
        assert_eq!(comp.placement.anchor(), "top-right");

        assert!(config.apply_output_override(&mut comp, &["main", "eDP-1"]));
        assert_eq!(comp.placement.anchor(), "top-left");
        assert_eq!(comp.placement.offset(), (10, 10));

        assert!(!config.apply_output_override(&mut comp, &["side"]));
    }

    #[test]
    fn saves_changes_and_preserves_comments() {
        let dir = std::env::temp_dir().join(format!("nitroget-config-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        std::fs::write(
            &path,
            "# my config\n\n[widgets.clock]\n# keep this comment\nanchor = \"top-right\"\n",
        )
        .unwrap();

        save(
            &path,
            &[PlacementChange {
                name: "clock".into(),
                output: Some("main".into()),
                per_output: None,
                anchor: "top-left".into(),
                offset: (10, 20),
            }],
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("# keep this comment"));
        assert!(text.contains("anchor = \"top-left\""));
        assert!(text.contains("offset = [10, 20]"));
        assert!(text.contains("output = \"main\""));

        let parsed: Config = toml::from_str(&text).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.widgets["clock"].anchor.as_deref(), Some("top-left"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saves_per_output_entries() {
        let dir =
            std::env::temp_dir().join(format!("nitroget-config-per-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        save(
            &path,
            &[PlacementChange {
                name: "clock".into(),
                output: None,
                per_output: Some("main".into()),
                anchor: "bottom-right".into(),
                offset: (12, 34),
            }],
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        parsed.validate().unwrap();

        let over = parsed.widgets["clock"].outputs.get("main").unwrap();
        assert_eq!(over.anchor.as_deref(), Some("bottom-right"));
        assert_eq!(over.offset.unwrap().pair(), (12, 34));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_reserved_alias_names() {
        let config: Config = toml::from_str("[outputs]\nall = \"DP-1\"\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_bad_values() {
        let config: Config = toml::from_str("[widgets.a]\nanchor = \"sideways\"\n").unwrap();
        assert!(config.validate().is_err());

        let config: Config = toml::from_str("[widgets.a]\nlayer = \"middle\"\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn saves_hostile_widget_names_as_toml_keys() {
        let dir =
            std::env::temp_dir().join(format!("nitroget-config-key-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let name = "evil\"\n[widgets.injected]\nenabled = false\n#";
        save(
            &path,
            &[PlacementChange {
                name: name.into(),
                output: None,
                per_output: None,
                anchor: "top-left".into(),
                offset: (1, 2),
            }],
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert!(parsed.widgets.contains_key(name));
        assert!(!parsed.widgets.contains_key("injected"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_write_through_a_config_symlink() {
        let dir = std::env::temp_dir().join(format!(
            "nitroget-config-symlink-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let secret = dir.join("secret.txt");
        std::fs::write(&secret, "do not touch").unwrap();

        let config = dir.join("config.toml");
        std::os::unix::fs::symlink(&secret, &config).unwrap();

        let result = save(
            &config,
            &[PlacementChange {
                name: "clock".into(),
                output: None,
                per_output: None,
                anchor: "top-left".into(),
                offset: (1, 2),
            }],
        );

        assert!(result.is_err());
        assert!(load(&config).is_err());
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "do not touch");

        std::fs::remove_dir_all(&dir).ok();
    }
}
