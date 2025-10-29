use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemePalette {
    pub name: String,
    pub label: String,
    pub version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub colors: ThemeColors,
}

impl ThemePalette {
    pub fn primary_accent(&self) -> &str {
        &self.colors.accents.primary
    }

    pub fn secondary_accent(&self) -> &str {
        &self.colors.accents.secondary
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeColors {
    pub background: BackgroundColors,
    pub foreground: ForegroundColors,
    pub accents: AccentColors,
    pub status: StatusColors,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundColors {
    pub dim: String,
    pub surface: String,
    pub floating: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForegroundColors {
    pub primary: String,
    pub muted: String,
    pub subtle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccentColors {
    pub primary: String,
    pub secondary: String,
    #[serde(default)]
    pub mint: String,
    #[serde(default)]
    pub ghost: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusColors {
    pub info: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeWriteAction {
    Created,
    Updated,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct ThemeInstallOutcome {
    pub path: PathBuf,
    pub action: ThemeWriteAction,
}

pub struct ThemeRegistry;

impl ThemeRegistry {
    pub const DEFAULT_THEME: &'static str = "tokyonight";

    pub fn normalize(name: &str) -> String {
        name.trim().to_ascii_lowercase().replace([' ', '_'], "-")
    }

    fn builtin_raw(name: &str) -> Option<&'static str> {
        match Self::normalize(name).as_str() {
            "tokyonight" | "tokyo-night" => Some(include_str!("../assets/themes/tokyonight.json")),
            "archon-light" | "archonlight" => {
                Some(include_str!("../assets/themes/archon-light.json"))
            }
            _ => None,
        }
    }

    pub fn load(name: &str, config_dir: Option<&Path>) -> Result<ThemePalette> {
        let normalized = Self::normalize(name);
        if let Some(dir) = config_dir {
            let candidate = dir.join("themes").join(format!("{normalized}.json"));
            if candidate.exists() {
                let raw = fs::read_to_string(&candidate)
                    .with_context(|| format!("Unable to read theme at {}", candidate.display()))?;
                match serde_json::from_str::<ThemePalette>(&raw) {
                    Ok(palette) => return Ok(palette),
                    Err(err) => {
                        warn!(path = %candidate.display(), error = ?err, "Malformed theme palette; falling back to builtin");
                    }
                }
            }
        }

        let builtin =
            Self::builtin_raw(&normalized).ok_or_else(|| anyhow!("Theme '{name}' not found"))?;
        let palette: ThemePalette = serde_json::from_str(builtin)
            .with_context(|| format!("Embedded theme '{normalized}' is invalid"))?;
        Ok(palette)
    }

    pub fn ensure_installed(
        name: &str,
        config_dir: Option<&Path>,
    ) -> Result<Option<ThemeInstallOutcome>> {
        let Some(dir) = config_dir else {
            return Ok(None);
        };
        let normalized = Self::normalize(name);
        let Some(raw) = Self::builtin_raw(&normalized) else {
            return Ok(None);
        };
        let theme_dir = dir.join("themes");
        fs::create_dir_all(&theme_dir)
            .with_context(|| format!("Failed to create theme directory {}", theme_dir.display()))?;
        let theme_path = theme_dir.join(format!("{normalized}.json"));
        let action = if theme_path.exists() {
            let existing = fs::read_to_string(&theme_path).with_context(|| {
                format!("Failed to read existing theme {}", theme_path.display())
            })?;
            if existing != raw {
                fs::write(&theme_path, raw).with_context(|| {
                    format!("Failed to update theme at {}", theme_path.display())
                })?;
                ThemeWriteAction::Updated
            } else {
                ThemeWriteAction::Skipped
            }
        } else {
            fs::write(&theme_path, raw)
                .with_context(|| format!("Failed to write theme to {}", theme_path.display()))?;
            ThemeWriteAction::Created
        };
        debug!(path = %theme_path.display(), ?action, "Ensured theme asset");
        Ok(Some(ThemeInstallOutcome {
            path: theme_path,
            action,
        }))
    }

    pub fn default_palette() -> ThemePalette {
        serde_json::from_str(Self::builtin_raw(Self::DEFAULT_THEME).expect("default theme exists"))
            .expect("default theme parses")
    }
}
