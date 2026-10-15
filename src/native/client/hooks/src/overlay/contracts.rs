#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GameplayModeKind {
    #[default]
    Official,
    Mod,
}

impl From<hooks_config::GameplayMode> for GameplayModeKind {
    fn from(value: hooks_config::GameplayMode) -> Self {
        match value {
            hooks_config::GameplayMode::Official => Self::Official,
            hooks_config::GameplayMode::Mod => Self::Mod,
        }
    }
}

/// Immutable runtime projection for the UI layer. The selected mode comes
/// exclusively from the explicit scbl.toml field; missing configuration and
/// unknown field values both fail closed to Official in hooks-config.
pub(crate) fn current_gameplay_mode() -> GameplayModeKind {
    hooks_config::get().map_or(GameplayModeKind::Official, |config| config.gameplay_mode.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_mode_projects_without_feature_or_plugin_inference() {
        assert_eq!(GameplayModeKind::from(hooks_config::GameplayMode::Official), GameplayModeKind::Official);
        assert_eq!(GameplayModeKind::from(hooks_config::GameplayMode::Mod), GameplayModeKind::Mod);
    }
}
