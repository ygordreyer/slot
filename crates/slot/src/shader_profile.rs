use std::collections::{BTreeMap, BTreeSet};

use slot_gfx::preset::Profile;
use slot_retro::RetroCore;
use slot_store::Core;
use slot_ui::QuickValue;

pub(crate) struct Options {
    pub values: BTreeMap<String, String>,
    pub applied: BTreeSet<String>,
    pub colour: Option<QuickValue>,
    pub mismatch: bool,
}

pub(crate) fn capture_baseline(core: &dyn RetroCore) -> BTreeMap<String, String> {
    ["mgba_audio_low_pass_filter", "mgba_audio_low_pass_range"]
        .into_iter()
        .filter_map(|key| core.option(key).map(|value| (key.into(), value)))
        .collect()
}

pub(crate) fn options(
    core: Core,
    colour: bool,
    custom: bool,
    profile: Option<&Profile>,
    baseline: &BTreeMap<String, String>,
    previously_applied: &BTreeSet<String>,
) -> Options {
    let mut values = BTreeMap::new();
    for key in previously_applied {
        if let Some(value) = baseline.get(key) {
            values.insert(key.clone(), value.clone());
        }
    }
    if let Some((key, value)) = crate::core::colour_option(core, colour) {
        values.insert(key.into(), value.into());
    }
    if core == Core::Mgba {
        values.insert(
            "mgba_interframe_blending".into(),
            if custom { "OFF" } else { "mix" }.into(),
        );
    }
    let mut plan = Options {
        values,
        applied: BTreeSet::new(),
        colour: None,
        mismatch: false,
    };
    let Some(profile) = profile else {
        return plan;
    };
    // The extension currently validates mGBA options only; a core tag never switches cores.
    plan.mismatch = profile
        .core
        .as_deref()
        .is_some_and(|required| required != core.as_str())
        || (!profile.core_options.is_empty() && core != Core::Mgba);
    if plan.mismatch {
        return plan;
    }
    for (key, value) in &profile.core_options {
        plan.values.insert(key.clone(), value.clone());
        plan.applied.insert(key.clone());
    }
    plan.colour = profile
        .core_options
        .get("mgba_color_correction")
        .map(|value| match value.as_str() {
            "GBA" => QuickValue::Gba,
            "Auto" => QuickValue::Auto,
            _ => QuickValue::Off,
        });
    plan
}
