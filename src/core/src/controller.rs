// Copyright (c) Modding Forge

use crate::{
    AppCommand, Config, ControlScope, MixTarget, VolumeBackend, VolumeError, VolumeUpdate,
};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

/// State exposed to IPC clients and keyboard hook decisions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControllerStatus {
    /// Whether the hook should currently consume media keys.
    pub hook_active: bool,
    /// Whether hook input has been explicitly paused.
    pub hook_paused: bool,
    /// A concise backend status string.
    pub backend: String,
}

/// Sequential command processor that owns no platform-specific hook code.
pub struct Controller<B: VolumeBackend> {
    backend: Arc<B>,
    config: Config,
    status_tx: watch::Sender<ControllerStatus>,
    volume_updates: Option<Arc<dyn Fn(VolumeUpdate) + Send + Sync>>,
}

impl<B: VolumeBackend> Controller<B> {
    /// Builds a controller and a watch receiver for current availability.
    pub fn new(backend: Arc<B>, config: Config) -> (Self, watch::Receiver<ControllerStatus>) {
        let (status_tx, status_rx) = watch::channel(ControllerStatus::default());
        (
            Self {
                backend,
                config,
                status_tx,
                volume_updates: None,
            },
            status_rx,
        )
    }

    /// Routes confirmed volume changes to a platform-specific overlay.
    pub fn with_volume_updates(
        mut self,
        callback: impl Fn(VolumeUpdate) + Send + Sync + 'static,
    ) -> Self {
        self.volume_updates = Some(Arc::new(callback));
        self
    }

    /// Consumes commands until shutdown or every sender is dropped.
    pub async fn run(self, mut commands: mpsc::Receiver<AppCommand>) {
        let mut paused = false;
        let mut timer = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            let command = tokio::select! {
                _ = timer.tick() => { self.publish(paused).await; continue; }
                command = commands.recv() => command,
            };
            let Some(command) = command else { break };
            if command == AppCommand::Shutdown {
                break;
            }
            if let AppCommand::SetHookPaused(value) = command {
                paused = value;
                self.publish(paused).await;
                continue;
            }
            if paused {
                continue;
            }
            if let Err(error) = self.execute(command).await {
                warn!(%error, "controller command failed");
            }
            self.publish(paused).await;
        }
    }

    async fn execute(&self, command: AppCommand) -> Result<(), VolumeError> {
        let config = Config::load().unwrap_or_else(|_| self.config.clone());
        let mut target = config.target_mix;
        let mut update = match command {
            AppCommand::VolumeUp => match config.control_scope {
                ControlScope::Focus => {
                    self.backend
                        .adjust_current_app(config.step_percent, target)
                        .await?
                }
                ControlScope::Global => {
                    self.backend
                        .adjust_global_mix(config.step_percent, target, &config.target_mix_ids)
                        .await?
                }
            },
            AppCommand::VolumeDown => match config.control_scope {
                ControlScope::Focus => {
                    self.backend
                        .adjust_current_app(-config.step_percent, target)
                        .await?
                }
                ControlScope::Global => {
                    self.backend
                        .adjust_global_mix(-config.step_percent, target, &config.target_mix_ids)
                        .await?
                }
            },
            AppCommand::Adjust {
                delta_percent,
                target: command_target,
            } => {
                target = command_target;
                match config.control_scope {
                    ControlScope::Focus => {
                        self.backend
                            .adjust_current_app(delta_percent, target)
                            .await?
                    }
                    ControlScope::Global => {
                        self.backend
                            .adjust_global_mix(delta_percent, target, &[])
                            .await?
                    }
                }
            }
            AppCommand::ToggleMute => match config.control_scope {
                ControlScope::Focus => self.backend.toggle_mute_current_app(target).await?,
                ControlScope::Global => {
                    self.backend
                        .toggle_mute_global_mix(target, &config.target_mix_ids)
                        .await?
                }
            },
            AppCommand::SetHookPaused(_) | AppCommand::Shutdown => return Ok(()),
        };
        update.show_target_name = config.overlay_show_target_name;
        update.scale = config.overlay_scale;
        update.icon_scale = config.overlay_icon_scale;
        update.font_scale = config.overlay_font_scale;
        update.font_family = config.overlay_font_family;
        update.hide_delay_seconds = config.overlay_hide_delay_seconds;
        debug!(
            result = update.percent,
            target_name = update.target_name,
            ?target,
            "volume adjusted"
        );
        if config.overlay_enabled {
            if let Some(callback) = &self.volume_updates {
                callback(update);
            }
        }
        Ok(())
    }

    async fn publish(&self, paused: bool) {
        let config = Config::load().unwrap_or_else(|_| self.config.clone());
        let focus_actionable = self.backend.is_actionable().await;
        let _ = self.status_tx.send(ControllerStatus {
            hook_active: should_activate_hook(config.control_scope, focus_actionable, paused),
            hook_paused: paused,
            backend: self.backend.status().await,
        });
    }
}

fn should_activate_hook(control_scope: ControlScope, focus_actionable: bool, paused: bool) -> bool {
    !paused && (control_scope == ControlScope::Global || focus_actionable)
}

/// Maps a CLI mix flag to a core mix target.
pub fn parse_mix(value: &str) -> Result<MixTarget, &'static str> {
    match value {
        "monitor" => Ok(MixTarget::Monitor),
        "stream" => Ok(MixTarget::Stream),
        "chat" => Ok(MixTarget::Chat),
        _ => Err("mix must be monitor, stream, or chat"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_hook_does_not_depend_on_focused_channel() {
        assert!(should_activate_hook(ControlScope::Global, false, false));
    }

    #[test]
    fn focus_hook_still_requires_an_actionable_channel() {
        assert!(!should_activate_hook(ControlScope::Focus, false, false));
        assert!(should_activate_hook(ControlScope::Focus, true, false));
    }

    #[test]
    fn paused_hook_is_inactive_for_every_scope() {
        assert!(!should_activate_hook(ControlScope::Global, true, true));
        assert!(!should_activate_hook(ControlScope::Focus, true, true));
    }
}
