//! The one place that decides where a configuration variable's value
//! comes from: the process environment first, then the values a project's
//! `happyview.toml` supplies. Anything that reads a variable the file can
//! supply goes through [`var`], so the order cannot differ between readers.
//!
//! The layer is process-wide because the readers include the instance
//! settings fallback, which is reached from dozens of call sites that hold
//! a pool but no `Config`.

use std::collections::BTreeMap;
use std::sync::RwLock;

static PROJECT_LAYER: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarSource {
    Env,
    Project,
}

impl VarSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Project => "project",
        }
    }
}

/// Replace the project layer. Called once at boot, before `Config` is built.
/// The layer is process-wide, so a test that installs one must be `#[serial]`.
pub fn set_project_layer(layer: BTreeMap<String, String>) {
    *PROJECT_LAYER.write().unwrap_or_else(|e| e.into_inner()) = layer;
}

/// A set variable wins even when it is empty, as it always has: `BASE_PATH=`
/// means "no base path", and the file must not reinterpret it.
pub fn var_with_source(name: &str) -> Option<(String, VarSource)> {
    if let Ok(value) = std::env::var(name) {
        return Some((value, VarSource::Env));
    }
    PROJECT_LAYER
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(name)
        .map(|value| (value.clone(), VarSource::Project))
}

pub fn var(name: &str) -> Option<String> {
    var_with_source(name).map(|(value, _)| value)
}

/// Clears the layer when dropped, so a test that installs one cannot leak
/// it into the next. Tests holding one must be `#[serial]`.
#[cfg(test)]
pub(crate) struct LayerReset;

#[cfg(test)]
impl Drop for LayerReset {
    fn drop(&mut self) {
        set_project_layer(BTreeMap::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    const NAME: &str = "HAPPYVIEW_VARS_TEST_ONLY";

    #[test]
    #[serial]
    fn the_environment_wins_over_the_project_layer() {
        let _reset = LayerReset;
        unsafe { std::env::remove_var(NAME) };
        set_project_layer([(NAME.to_string(), "from-file".to_string())].into());
        assert_eq!(
            var_with_source(NAME),
            Some(("from-file".into(), VarSource::Project))
        );

        unsafe { std::env::set_var(NAME, "from-env") };
        assert_eq!(
            var_with_source(NAME),
            Some(("from-env".into(), VarSource::Env))
        );
        unsafe { std::env::remove_var(NAME) };
    }

    #[test]
    #[serial]
    fn a_set_but_empty_variable_still_wins() {
        let _reset = LayerReset;
        set_project_layer([(NAME.to_string(), "from-file".to_string())].into());
        unsafe { std::env::set_var(NAME, "") };
        assert_eq!(var(NAME).as_deref(), Some(""));
        unsafe { std::env::remove_var(NAME) };
    }

    #[test]
    #[serial]
    fn neither_source_is_none() {
        let _reset = LayerReset;
        unsafe { std::env::remove_var(NAME) };
        assert_eq!(var(NAME), None);
    }
}
