//! Built-in looks, recursive presets, legacy top-level shaders, and saved parameters.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub const SHADERS_DIR: &str = "Shaders";
pub const SHADER_LCD: &str = "LCD";
pub const SHADER_OFF: &str = "Off";

pub fn is_builtin(name: &str) -> bool {
    name.eq_ignore_ascii_case(SHADER_LCD) || name.eq_ignore_ascii_case(SHADER_OFF)
}
fn files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 32 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if e.file_name().to_string_lossy().starts_with("._") {
                continue;
            }
            let Ok(kind) = e.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(&path, base, out, depth + 1);
            } else if kind.is_file() {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if is_builtin(stem) {
                    continue;
                }
                let ext = path.extension().unwrap_or_default();
                if ext.eq_ignore_ascii_case("glslp")
                    || (depth == 0 && ext.eq_ignore_ascii_case("glsl"))
                {
                    if let Ok(relative) = path.strip_prefix(base) {
                        out.push(relative.into());
                    }
                }
            }
        }
    }
    let base = root.join(SHADERS_DIR);
    let mut out = Vec::new();
    walk(&base, &base, &mut out, 0);
    out.sort_by_key(|p| {
        (
            p.parent().is_some_and(|p| !p.as_os_str().is_empty()),
            p.parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .to_lowercase(),
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
        )
    });
    out
}
fn identifier(path: &Path) -> String {
    if path.parent() == Some(Path::new(""))
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("glsl"))
    {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    } else {
        path.to_string_lossy().replace('\\', "/")
    }
}

/// Top-level `.glsl` identifiers keep their old stems so existing slot.state files still work.
pub fn list_shaders(root: &Path) -> Vec<String> {
    let mut out = vec![SHADER_LCD.into(), SHADER_OFF.into()];
    out.extend(files(root).iter().map(|p| identifier(p)));
    out
}
pub fn shader_path(root: &Path, name: &str) -> Option<PathBuf> {
    if is_builtin(name) {
        return None;
    }
    files(root)
        .into_iter()
        .find(|p| identifier(p) == name)
        .map(|p| root.join(SHADERS_DIR).join(p))
}
fn look_stem(path: &Path) -> std::borrow::Cow<'_, str> {
    let name = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("glslp"))
    {
        path.file_stem()
    } else {
        path.file_name()
    };
    name.unwrap_or_default().to_string_lossy()
}
pub fn shader_label(name: &str, choices: &[String]) -> String {
    if is_builtin(name) {
        return name.into();
    }
    let path = Path::new(name);
    let stem = look_stem(path);
    let count = choices
        .iter()
        .filter(|s| look_stem(Path::new(s)).eq_ignore_ascii_case(&stem))
        .count();
    if count > 1 {
        format!(
            "{} ({})",
            stem,
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .display()
        )
    } else {
        stem.into_owned()
    }
}

pub fn params_path(root: &Path, shader: &str) -> Option<PathBuf> {
    if Path::new(shader).components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    let canonical = shader_path(root, shader).and_then(|p| {
        p.strip_prefix(root.join(SHADERS_DIR))
            .ok()
            .map(Path::to_path_buf)
    });
    let relative = canonical.as_deref().unwrap_or(Path::new(shader));
    let name = relative
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_str().map(|s| {
                // Escaping the separator and escape character keeps component boundaries reversible.
                s.replace('%', "%25").replace('_', "%5F")
            })),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?
        .join("__");
    if name.is_empty() {
        return None;
    }
    Some(
        root.join("Config/shader-params")
            .join(format!("{name}.params")),
    )
}
pub fn parse_params(text: &str) -> BTreeMap<String, f32> {
    text.lines()
        .filter_map(|line| {
            if line.trim().starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let value: f32 = value.trim().trim_matches('"').parse().ok()?;
            (value.is_finite() && !key.trim().is_empty()).then(|| (key.trim().into(), value))
        })
        .collect()
}
pub fn read_shader_params(root: &Path, shader: &str) -> BTreeMap<String, f32> {
    params_path(root, shader)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|s| parse_params(&s))
        .unwrap_or_default()
}
/// Callers pass only non-default values. Deterministic lines make the files RetroArch-readable.
pub fn write_shader_params(
    root: &Path,
    shader: &str,
    values: &BTreeMap<String, f32>,
) -> std::io::Result<()> {
    let path = params_path(root, shader).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid relative shader path",
        )
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text: String = values
        .iter()
        .filter(|(_, v)| v.is_finite())
        .map(|(name, value)| format!("{name} = \"{value}\"\n"))
        .collect();
    crate::atomic_write(&path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursion_collisions_sorting_and_legacy_resolution() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("Shaders/z")).unwrap();
        for file in [
            "same.glsl",
            "Alpha.glslp",
            ".hidden.glslp",
            "dotted.name.glsl",
            "._hidden.glsl",
            "LCD.glslp",
            "z/Off.glslp",
            "z/same.glslp",
            "z/pass.glsl",
        ] {
            std::fs::write(root.join("Shaders").join(file), "").unwrap();
        }
        let choices = list_shaders(root);
        assert_eq!(
            choices,
            [
                "LCD",
                "Off",
                ".hidden.glslp",
                "Alpha.glslp",
                "dotted.name",
                "same",
                "z/same.glslp"
            ]
        );
        assert_eq!(
            shader_path(root, "same"),
            Some(root.join("Shaders/same.glsl"))
        );
        assert_eq!(shader_label("same", &choices), "same (.)");
        assert_eq!(shader_label("z/same.glslp", &choices), "same (z)");
        assert!(shader_path(root, "../outside.glslp").is_none());
        assert_eq!(shader_label("dotted.name", &choices), "dotted.name");
        assert!(params_path(root, "same")
            .unwrap()
            .ends_with("same.glsl.params"));
    }
    #[test]
    fn parameter_paths_do_not_collide_with_component_separators_or_escapes() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("Config/shader-params");
        let names = ["a/b.glslp", "a__b.glslp", "a%5F_b.glslp", "a_b.glslp"];
        let paths: Vec<_> = names
            .iter()
            .map(|name| params_path(d.path(), name).unwrap())
            .collect();
        for (i, path) in paths.iter().enumerate() {
            assert_eq!(path.parent(), Some(dir.as_path()));
            assert!(!paths[..i].contains(path), "collision for {}", names[i]);
            let values = BTreeMap::from([("A".into(), i as f32)]);
            write_shader_params(d.path(), names[i], &values).unwrap();
        }
        for (i, name) in names.iter().enumerate() {
            assert_eq!(read_shader_params(d.path(), name)["A"], i as f32);
        }
    }

    #[test]
    fn parameter_paths_reject_parent_absolute_and_empty_paths() {
        let d = tempfile::tempdir().unwrap();
        for name in ["../test.glslp", "a/../test.glslp", "/test.glslp", "", "."] {
            assert!(params_path(d.path(), name).is_none());
            assert!(read_shader_params(d.path(), name).is_empty());
            let error = write_shader_params(d.path(), name, &BTreeMap::new()).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
        #[cfg(windows)]
        for name in [
            r"C:\test.glslp",
            r"C:test.glslp",
            r"\\server\share\test.glslp",
        ] {
            assert!(params_path(d.path(), name).is_none());
        }
    }

    #[test]
    fn params_round_trip_atomic_and_per_shader() {
        let d = tempfile::tempdir().unwrap();
        let values = BTreeMap::from([("A".into(), 0.25), ("B".into(), 2.0)]);
        write_shader_params(d.path(), "handheld/test.glslp", &values).unwrap();
        assert_eq!(read_shader_params(d.path(), "handheld/test.glslp"), values);
        assert!(read_shader_params(d.path(), "other").is_empty());
        assert!(params_path(d.path(), "handheld/test.glslp")
            .unwrap()
            .ends_with("handheld__test.glslp.params"));
    }
}
