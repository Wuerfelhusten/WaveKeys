// Copyright (c) Modding Forge

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Wave Link mix that receives an adjustment.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MixTarget {
    /// The local monitor output.
    Monitor,
    /// The stream output.
    Stream,
    /// The Wave Link chat mix.
    Chat,
}

/// Icon supplied by Wave Link for the affected channel or mix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeTargetIcon {
    /// Base64-encoded PNG returned for a concrete channel.
    PngBase64(String),
    /// Wave Link's symbolic mix icon name.
    Named(String),
    /// No usable icon was present in the response.
    None,
}

/// Confirmed state after Wave Link applied a volume change.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeUpdate {
    /// Resulting level in percentage points.
    pub percent: f32,
    /// Human-readable channel or mix name.
    pub target_name: String,
    /// Channel artwork or symbolic mix icon.
    pub icon: VolumeTargetIcon,
    /// Whether the affected channel or mix is currently muted.
    pub is_muted: bool,
    /// Whether the overlay should render the target name.
    pub show_target_name: bool,
    /// Logical size multiplier selected by the user.
    pub scale: f32,
    /// Additional icon size multiplier.
    pub icon_scale: f32,
    /// Additional text size multiplier.
    pub font_scale: f32,
    /// Installed Windows font family, or `system`.
    pub font_family: String,
    /// Idle time before the overlay is hidden.
    pub hide_delay_seconds: f32,
}

/// Failure returned by a volume provider.
#[derive(Debug, Error)]
pub enum VolumeError {
    /// No connected backend can service the command.
    #[error("Wave Link is unavailable")]
    Unavailable,
    /// The focused application has no associated channel.
    #[error("the focused application has no Wave Link channel")]
    ChannelUnavailable,
    /// A backend request did not complete in time.
    #[error("Wave Link request timed out")]
    Timeout,
    /// Protocol data was unsuitable for the requested operation.
    #[error("invalid Wave Link response: {0}")]
    Protocol(String),
}

/// Abstract implementation for focused-channel volume operations.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait VolumeBackend: Send + Sync {
    /// Returns whether an actionable focused channel currently exists.
    async fn is_actionable(&self) -> bool;
    /// Adjusts the focused channel and returns its confirmed display state.
    async fn adjust_current_app(
        &self,
        delta: f32,
        target: MixTarget,
    ) -> Result<VolumeUpdate, VolumeError>;
    /// Toggles mute for one mix of the focused channel.
    async fn toggle_mute_current_app(&self, target: MixTarget)
        -> Result<VolumeUpdate, VolumeError>;
    /// Adjusts the complete selected Wave Link mix.
    async fn adjust_global_mix(
        &self,
        delta: f32,
        target: MixTarget,
        mix_ids: &[String],
    ) -> Result<VolumeUpdate, VolumeError>;
    /// Toggles mute for the complete selected Wave Link mix.
    async fn toggle_mute_global_mix(
        &self,
        target: MixTarget,
        mix_ids: &[String],
    ) -> Result<VolumeUpdate, VolumeError>;
    /// Produces a human-readable backend status.
    async fn status(&self) -> String;
}

/// Clamps a percentage adjustment to the Wave Link-safe range.
pub fn adjust_percent(current: f32, delta: f32) -> f32 {
    (current + delta).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjustment_above_maximum_returns_maximum() {
        assert_eq!(adjust_percent(99.0, 3.0), 100.0);
    }

    #[test]
    fn adjustment_below_minimum_returns_minimum() {
        assert_eq!(adjust_percent(1.0, -3.0), 0.0);
    }
}
