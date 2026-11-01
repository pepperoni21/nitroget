//! Reconciliation between the desired widget set and the running windows.
//!
//! The decision logic is a pure function (`decide`) so every transition can be
//! unit tested without a display.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::SystemTime;

use async_channel::Sender;

use crate::component::{Component, Placement};
use crate::config;
use crate::discovery::{collect_paths, mtime_of};
use crate::instance::Instance;
use crate::layout;
use crate::state::{Msg, Running, WidgetKey, WidgetState};

/// Snapshot of a running widget used by `decide`.
pub struct Existing<'a> {
    pub mtime: SystemTime,
    pub placement: &'a Placement,
    pub state: WidgetState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Nothing changed.
    Unchanged,
    /// Same file, only anchor/offset moved: update the mapped surface.
    MoveInPlace,
    /// Re-run the component in the existing surface.
    Reload,
    /// Monitor/layer/size changed: build a new surface and retire the old one.
    Replace,
    /// No running instance for this widget yet.
    Create,
}

pub fn decide(
    existing: Option<Existing<'_>>,
    mtime: SystemTime,
    placement: &Placement,
) -> Decision {
    let Some(existing) = existing else {
        return Decision::Create;
    };

    let in_place = can_update_in_place(existing.placement, placement);

    match existing.state {
        WidgetState::Parked => {
            if in_place {
                Decision::Reload
            } else {
                Decision::Replace
            }
        }
        WidgetState::Active => {
            if existing.mtime == mtime && existing.placement == placement {
                Decision::Unchanged
            } else if existing.mtime == mtime && in_place {
                Decision::MoveInPlace
            } else if in_place {
                Decision::Reload
            } else {
                Decision::Replace
            }
        }
    }
}

/// Anchor/offset changes are applied to the mapped surface. Everything that
/// requires a layer-shell reconfigure (monitor, layer, size) gets a new
/// surface instead.
pub fn can_update_in_place(old: &Placement, new: &Placement) -> bool {
    old.output == new.output
        && old.output_index == new.output_index
        && old.layer == new.layer
        && old.width == new.width
        && old.height == new.height
}

/// Loads widgets, applies `config.toml`, computes automatic placement and
/// reconciles the running windows with the result.
#[allow(clippy::too_many_arguments)]
pub fn sync(
    widgets: &mut HashMap<WidgetKey, Running>,
    retired: &mut Vec<Instance>,
    target: Option<&Path>,
    config_path: &Path,
    windowed: bool,
    app: &gtk::Application,
    tx: &Sender<Msg>,
) {
    let paths = match collect_paths(target) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("nitroget: {e:#}");
            return;
        }
    };

    let config = match config::load(config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("nitroget: {e:#}");
            return;
        }
    };

    let mut components = Vec::new();
    for path in &paths {
        match crate::component::load(path) {
            Ok(mut comp) => {
                if config.apply(&mut comp) {
                    components.push(comp);
                }
            }
            Err(e) => eprintln!("nitroget: {}: {e:#}", path.display()),
        }
    }

    config::auto_place(&mut components, &config);
    warn_duplicate_names(&components);

    let monitors = layout::monitors(&config);

    // A widget pinned to a monitor that is currently absent is parked, not
    // moved somewhere else. Monitor hotplug changes the fingerprint and brings
    // it back.
    components.retain(|comp| {
        let is_all = comp.placement.output.as_deref() == Some("all");
        if !is_all
            && comp.placement.output.is_some()
            && layout::resolve_monitor(&comp.placement, &monitors).is_none()
        {
            eprintln!(
                "nitroget: `{}` is assigned to a monitor that is not attached; parking",
                comp.name
            );
            false
        } else {
            true
        }
    });

    // `output = "all"` becomes one component per monitor, and per-monitor
    // overrides are applied.
    let components = layout::expand(components, &config, &monitors);

    let desired: HashSet<WidgetKey> = components
        .iter()
        .map(|comp| (comp.path.clone(), comp.placement.output_index))
        .collect();
    let stale: Vec<WidgetKey> = widgets
        .keys()
        .filter(|key| !desired.contains(*key))
        .cloned()
        .collect();

    for key in stale {
        if let Some(running) = widgets.get_mut(&key)
            && running.state == WidgetState::Active
        {
            running.instance.park();
            running.state = WidgetState::Parked;
        }
    }

    for comp in components {
        let path = comp.path.clone();
        let key = (path.clone(), comp.placement.output_index);
        let placement = comp.placement.clone();
        let mtime = mtime_of(&path);

        let decision = decide(
            widgets.get(&key).map(|running| Existing {
                mtime: running.mtime,
                placement: &running.placement,
                state: running.state,
            }),
            mtime,
            &placement,
        );

        match decision {
            Decision::Unchanged => {}

            Decision::MoveInPlace => {
                if let Some(running) = widgets.get_mut(&key) {
                    running.instance.set_placement(placement.clone());
                    running.placement = placement;
                    running.state = WidgetState::Active;
                }
            }

            Decision::Reload => {
                if let Some(running) = widgets.get_mut(&key) {
                    match running.instance.reload(comp) {
                        Ok(()) => {
                            running.mtime = mtime;
                            running.placement = placement;
                            running.state = WidgetState::Active;
                        }
                        Err(e) => eprintln!("nitroget: {}: {e:#}", path.display()),
                    }
                }
            }

            Decision::Replace => {
                let previous = widgets.remove(&key);
                match Instance::create(comp, key.clone(), windowed, app, tx.clone()) {
                    Ok(instance) => {
                        if let Some(previous) = previous {
                            let mut instance_to_retire = previous.instance;
                            instance_to_retire.retire();
                            retired.push(instance_to_retire);
                        }
                        widgets.insert(
                            key,
                            Running {
                                instance,
                                mtime,
                                placement,
                                state: WidgetState::Active,
                            },
                        );
                    }
                    Err(e) => {
                        if let Some(previous) = previous {
                            widgets.insert(key, previous);
                        }
                        eprintln!("nitroget: {}: {e:#}", path.display());
                    }
                }
            }

            Decision::Create => {
                match Instance::create(comp, key.clone(), windowed, app, tx.clone()) {
                    Ok(instance) => {
                        widgets.insert(
                            key,
                            Running {
                                instance,
                                mtime,
                                placement,
                                state: WidgetState::Active,
                            },
                        );
                    }
                    Err(e) => eprintln!("nitroget: {}: {e:#}", path.display()),
                }
            }
        }
    }
}

fn warn_duplicate_names(components: &[Component]) {
    let mut seen: HashMap<&str, &Path> = HashMap::new();

    for comp in components {
        if let Some(first) = seen.insert(comp.name.as_str(), &comp.path) {
            eprintln!(
                "nitroget: warning: two widgets are named `{}` ({} and {}); \
                 config.toml applies to both",
                comp.name,
                first.display(),
                comp.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn placement() -> Placement {
        Placement::default()
    }

    fn t(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn creates_when_missing() {
        assert_eq!(decide(None, t(1), &placement()), Decision::Create);
    }

    #[test]
    fn unchanged_when_file_and_placement_match() {
        let p = placement();
        let existing = Existing {
            mtime: t(1),
            placement: &p,
            state: WidgetState::Active,
        };
        assert_eq!(decide(Some(existing), t(1), &p), Decision::Unchanged);
    }

    #[test]
    fn moves_in_place_when_only_anchor_changes() {
        let old = placement();
        let mut new = placement();
        new.anchor = Some("bottom-left".into());
        new.offset = Some((10, 20));

        let existing = Existing {
            mtime: t(1),
            placement: &old,
            state: WidgetState::Active,
        };
        assert_eq!(decide(Some(existing), t(1), &new), Decision::MoveInPlace);
    }

    #[test]
    fn reloads_when_file_changes() {
        let p = placement();
        let existing = Existing {
            mtime: t(1),
            placement: &p,
            state: WidgetState::Active,
        };
        assert_eq!(decide(Some(existing), t(2), &p), Decision::Reload);
    }

    #[test]
    fn replaces_when_monitor_changes() {
        let old = placement();
        let mut new = placement();
        new.output = Some("DP-1".into());

        let existing = Existing {
            mtime: t(1),
            placement: &old,
            state: WidgetState::Active,
        };
        assert_eq!(decide(Some(existing), t(1), &new), Decision::Replace);
    }

    #[test]
    fn parked_widgets_reload_in_place_or_replace() {
        let p = placement();
        let existing = Existing {
            mtime: t(1),
            placement: &p,
            state: WidgetState::Parked,
        };
        assert_eq!(decide(Some(existing), t(1), &p), Decision::Reload);

        let mut new = placement();
        new.layer = "overlay".into();
        let existing = Existing {
            mtime: t(1),
            placement: &p,
            state: WidgetState::Parked,
        };
        assert_eq!(decide(Some(existing), t(1), &new), Decision::Replace);
    }
}
