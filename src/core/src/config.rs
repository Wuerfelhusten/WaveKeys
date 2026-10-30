// Copyright (c) Modding Forge

use crate::MixTarget;
use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf};
use thiserror::Error;

/// Behaviour for media keys when Wave Link cannot handle an action.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FallbackMode {
    /// Allow Windows to handle the key.
    PassThrough,
    /// Consume the key without performing an action.
    Suppress,
    /// Reserved explicit synonym for the Windows volume action.
    SystemVolume,
}

/// Selects whether keys target the focused channel or the entire Wave Link mix.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ControlScope {
    /// Change only the channel assigned to the currently focused app.
    Focus,
    /// Change the complete selected Wave Link mix.
    Global,
}

/// Persistent user settings.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Percentage points applied for a volume-up or volume-down command.
    pub step_percent: f32,
    /// Mix changed by hook commands.
    pub target_mix: MixTarget,
    /// Explicit Wave Link mix IDs controlled in global mode.
    pub target_mix_ids: Vec<String>,
    /// Scope used for media-key volume and mute commands.
    pub control_scope: ControlScope,
    /// Whether successful volume changes display the on-screen overlay.
    pub overlay_enabled: bool,
    /// Whether the overlay includes the affected channel or mix name.
    pub overlay_show_target_name: bool,
    /// Overlay size multiplier.
    pub overlay_scale: f32,
    /// Additional size multiplier for the channel or mix icon.
    pub overlay_icon_scale: f32,
    /// Additional size multiplier for overlay text.
    pub overlay_font_scale: f32,
    /// Installed Windows font family used by the overlay, or `system`.
    pub overlay_font_family: String,
    /// Seconds without a volume change before the overlay is hidden.
    pub overlay_hide_delay_seconds: f32,
    /// Internal visual calibration version for persisted overlay scale values.
    pub overlay_calibration_version: u8,
    /// Action taken while no Wave Link channel is available.
    pub fallback: FallbackMode,
    /// Initial reconnect delay in milliseconds.
    pub reconnect_initial_ms: u64,
    /// Maximum reconnect delay in milliseconds.
    pub reconnect_max_ms: u64,
    /// Maximum wait for a protocol request in milliseconds.
    pub rpc_timeout_ms: u64,
    /// Filter used by tracing-subscriber.
    pub log_level: String,
    /// Wave Link WebSocket URL discovered by the inspector.
    pub endpoint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            step_percent: 3.0,
            target_mix: MixTarget::Monitor,
            target_mix_ids: Vec::new(),
            control_scope: ControlScope::Focus,
            overlay_enabled: true,
            overlay_show_target_name: true,
            overlay_scale: 1.0,
            overlay_icon_scale: 1.0,
            overlay_font_scale: 1.0,
            overlay_font_family: "system".to_owned(),
            overlay_hide_delay_seconds: 1.0,
            overlay_calibration_version: 1,
            fallback: FallbackMode::PassThrough,
            reconnect_initial_ms: 500,
            reconnect_max_ms: 10_000,
            rpc_timeout_ms: 2_000,
            log_level: "info".to_owned(),
            endpoint: "auto".to_owned(),
        }
    }
}

/// Configuration load or validation failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The required APPDATA directory is unavailable.
    #[error("APPDATA is not set")]
    AppDataUnavailable,
    /// A configuration file could not be read or written.
    #[error("configuration I/O failed: {0}")]
    Io(#[from] io::Error),
    /// TOML was not valid configuration.
    #[error("invalid configuration: {0}")]
    Toml(#[from] toml::de::Error),
    /// Settings violated a safe operating range.
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
}

impl Config {
    /// Returns the per-user configuration path.
    pub fn path() -> Result<PathBuf, ConfigError> {
        let app_data = env::var_os("APPDATA").ok_or(ConfigError::AppDataUnavailable)?;
        Ok(PathBuf::from(app_data).join("WaveKeys").join("config.toml"))
    }

    /// Reads settings or returns defaults when the file does not exist.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(path)?;
        let mut config: Self = toml::from_str(&source)?;
        if !source.contains("overlay_calibration_version")
            && source.contains("overlay_scale")
            && source.contains("overlay_font_scale")
        {
            config.overlay_scale = (config.overlay_scale / 0.7).clamp(0.5, 2.0);
            config.overlay_font_scale = (config.overlay_font_scale / 1.5).clamp(0.5, 1.5);
            config.overlay_calibration_version = 1;
            config.save()?;
        }
        config.validate()?;
        Ok(config)
    }

    /// Creates the default file without replacing an existing user file.
    pub fn create_default() -> Result<PathBuf, ConfigError> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(
                &path,
                toml::to_string_pretty(&Self::default()).expect("config is serializable"),
            )?;
        }
        Ok(path)
    }

    /// Persists this configuration in the standard per-user location.
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        self.validate()?;
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &path,
            toml::to_string_pretty(self).expect("config is serializable"),
        )?;
        Ok(path)
    }

    /// Verifies all values that control request rates and volume values.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(0.1..=100.0).contains(&self.step_percent) {
            return Err(ConfigError::Invalid(
                "step_percent must be between 0.1 and 100",
            ));
        }
        if !(0.5..=2.0).contains(&self.overlay_scale) {
            return Err(ConfigError::Invalid(
                "overlay_scale must be between 0.5 and 2.0",
            ));
        }
        if !(0.5..=1.5).contains(&self.overlay_icon_scale) {
            return Err(ConfigError::Invalid(
                "overlay_icon_scale must be between 0.5 and 1.5",
            ));
        }
        if !(0.5..=1.5).contains(&self.overlay_font_scale) {
            return Err(ConfigError::Invalid(
                "overlay_font_scale must be between 0.5 and 1.5",
            ));
        }
        if self.overlay_font_family.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "overlay_font_family must not be empty",
            ));
        }
        if !(0.1..=10.0).contains(&self.overlay_hide_delay_seconds) {
            return Err(ConfigError::Invalid(
                "overlay_hide_delay_seconds must be between 0.1 and 10",
            ));
        }
        if self.overlay_calibration_version != 1 {
            return Err(ConfigError::Invalid(
                "unsupported overlay calibration version",
            ));
        }
        if self.reconnect_initial_ms == 0 || self.reconnect_initial_ms > self.reconnect_max_ms {
            return Err(ConfigError::Invalid("invalid reconnect delay range"));
        }
        if self.rpc_timeout_ms == 0 || self.endpoint.is_empty() {
            return Err(ConfigError::Invalid("timeout and endpoint must be set"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn old_config_keeps_legacy_single_mix_selection() {
        let config: Config = toml::from_str("target_mix = \"stream\"\n").unwrap();
        assert_eq!(config.target_mix, MixTarget::Stream);
        assert!(config.target_mix_ids.is_empty());
    }

    #[test]
    fn zero_step_is_rejected() {
        let config = Config {
            step_percent: 0.0,
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
