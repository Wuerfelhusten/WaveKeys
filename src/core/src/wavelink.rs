// Copyright (c) Modding Forge

//! Minimal Wave Link 3.x JSON-RPC client.
//!
//! Method names and message shapes in this module are based on the supplied
//! `reference/WaveLink.Client/PROTOCOL.md` protocol reference.

use crate::{
    volume::adjust_percent, MixTarget, VolumeBackend, VolumeError, VolumeTargetIcon, VolumeUpdate,
};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{env, fs, path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{net::TcpStream, sync::Mutex, time::timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::header::ORIGIN, Message},
    MaybeTlsStream, WebSocketStream,
};
use tracing::trace;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Wave Link protocol or transport failure.
#[derive(Debug, Error)]
pub enum WaveLinkError {
    /// No discovered port was reachable.
    #[error("unable to connect to Wave Link")]
    ConnectionFailed,
    /// A request exceeded its configured deadline.
    #[error("Wave Link RPC timed out")]
    RpcTimeout,
    /// JSON-RPC response did not match the protocol.
    #[error("invalid Wave Link response: {0}")]
    InvalidResponse(String),
    /// The local Wave Link server did not identify itself correctly.
    #[error("unsupported Wave Link protocol")]
    UnsupportedProtocol,
    /// An I/O or WebSocket operation failed.
    #[error("Wave Link transport error: {0}")]
    Transport(String),
}

/// Finds Wave Link's local port using its documented state file then fallback range.
pub async fn discover_endpoint() -> Result<String, WaveLinkError> {
    if let Some(port) = read_ws_info_port() {
        return Ok(format!("ws://127.0.0.1:{port}"));
    }
    for port in 1884..=1893 {
        let endpoint = format!("ws://127.0.0.1:{port}");
        if timeout(Duration::from_millis(250), open_socket(&endpoint))
            .await
            .is_ok()
        {
            return Ok(endpoint);
        }
    }
    Err(WaveLinkError::ConnectionFailed)
}

fn read_ws_info_port() -> Option<u16> {
    let local = env::var_os("LOCALAPPDATA")?;
    let path = PathBuf::from(local)
        .join("Packages")
        .join("Elgato.WaveLink_g54w8ztgkx496")
        .join("LocalState")
        .join("ws-info.json");
    let value: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    value.get("port")?.as_u64()?.try_into().ok()
}

async fn open_socket(endpoint: &str) -> Result<Socket, WaveLinkError> {
    let mut request = endpoint
        .into_client_request()
        .map_err(|error| WaveLinkError::Transport(error.to_string()))?;
    request
        .headers_mut()
        .insert(ORIGIN, "streamdeck://".parse().expect("valid origin"));
    let (socket, _) = connect_async(request)
        .await
        .map_err(|error| WaveLinkError::Transport(error.to_string()))?;
    Ok(socket)
}

/// One JSON-RPC connection with request/response correlation and notification parsing.
pub struct WaveLinkClient {
    socket: Socket,
    next_id: u64,
    timeout: Duration,
    focused_channel: Option<String>,
    focused_process: Option<String>,
}

impl WaveLinkClient {
    /// Connects and validates the Wave Link application info response.
    pub async fn connect(endpoint: &str, rpc_timeout: Duration) -> Result<Self, WaveLinkError> {
        let mut client = Self {
            socket: open_socket(endpoint).await?,
            next_id: 1,
            timeout: rpc_timeout,
            focused_channel: None,
            focused_process: None,
        };
        let app = client.call("getApplicationInfo", Value::Null).await?;
        if app.get("appID").and_then(Value::as_str) != Some("EWL") {
            return Err(WaveLinkError::UnsupportedProtocol);
        }
        client
            .call(
                "setSubscription",
                json!({ "focusedAppChanged": { "isEnabled": true } }),
            )
            .await?;
        Ok(client)
    }

    /// Sends an RPC and waits for its correlated result, processing notifications meanwhile.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, WaveLinkError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "jsonrpc": "2.0", "method": method, "params": params });
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await
            .map_err(|error| WaveLinkError::Transport(error.to_string()))?;
        timeout(self.timeout, async {
            while let Some(frame) = self.socket.next().await {
                let message = frame.map_err(|error| WaveLinkError::Transport(error.to_string()))?;
                let Message::Text(text) = message else {
                    continue;
                };
                let message: Value = serde_json::from_str(&text)
                    .map_err(|error| WaveLinkError::InvalidResponse(error.to_string()))?;
                if message.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(error) = message.get("error") {
                        return Err(WaveLinkError::InvalidResponse(error.to_string()));
                    }
                    return message.get("result").cloned().ok_or_else(|| {
                        WaveLinkError::InvalidResponse("response has no result".to_owned())
                    });
                }
                self.process_notification(&message);
            }
            Err(WaveLinkError::ConnectionFailed)
        })
        .await
        .map_err(|_| WaveLinkError::RpcTimeout)?
    }

    /// Updates focused state from a notification and ignores unknown events safely.
    pub fn process_notification(&mut self, notification: &Value) {
        if notification.get("method").and_then(Value::as_str) != Some("focusedAppChanged") {
            trace!(notification = %notification, "ignored Wave Link notification");
            return;
        }
        // Once Windows foreground tracking has resolved a process, keep that result
        // authoritative. Wave Link can deliver a delayed empty notification after the
        // new foreground process was already mapped, which would otherwise erase it.
        if self.focused_process.is_none() {
            self.focused_channel = focused_channel_from_notification(notification);
        }
    }

    /// Returns the most recently reported focused channel id.
    pub fn focused_channel(&self) -> Option<&str> {
        self.focused_channel.as_deref()
    }

    /// Replaces cached focus with the value reported by the listener.
    pub fn set_focused_channel(&mut self, channel: Option<String>) {
        self.focused_channel = channel;
    }

    /// Receives and processes a single server notification.
    pub async fn receive_notification(&mut self) -> Result<Value, WaveLinkError> {
        while let Some(frame) = self.socket.next().await {
            let frame = frame.map_err(|error| WaveLinkError::Transport(error.to_string()))?;
            if let Message::Text(text) = frame {
                let value = serde_json::from_str(&text)
                    .map_err(|error| WaveLinkError::InvalidResponse(error.to_string()))?;
                self.process_notification(&value);
                return Ok(value);
            }
        }
        Err(WaveLinkError::ConnectionFailed)
    }

    /// Resolves the currently foreground Windows executable to a Wave Link channel.
    pub async fn refresh_focus_from_foreground(&mut self) -> Result<(), WaveLinkError> {
        let Some(process) = foreground_process_name() else {
            return Ok(());
        };
        if self.focused_process.as_deref() == Some(process.as_str()) {
            return Ok(());
        }
        let result = self.call("getChannels", Value::Null).await?;
        let channel = result
            .get("channels")
            .and_then(Value::as_array)
            .and_then(|channels| {
                channels.iter().find(|channel| {
                    channel
                        .get("apps")
                        .and_then(Value::as_array)
                        .is_some_and(|apps| {
                            apps.iter().any(|app| {
                                ["id", "name"]
                                    .iter()
                                    .filter_map(|key| app.get(*key).and_then(Value::as_str))
                                    .any(|name| normalize_app_name(name) == process)
                            })
                        })
                })
            })
            .and_then(|channel| channel.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        self.set_focused_channel(channel);
        self.focused_process = Some(process);
        Ok(())
    }

    /// Reads a channel's mix values from the current Wave Link state.
    pub async fn focused_mix(
        &mut self,
        target: MixTarget,
    ) -> Result<(String, String, f32, bool, String, VolumeTargetIcon), WaveLinkError> {
        let channel_id = self
            .focused_channel
            .clone()
            .ok_or(WaveLinkError::ConnectionFailed)?;
        let (target_mix_id, _, _, _, _) = self.selected_mix(target).await?;
        let result = self.call("getChannels", Value::Null).await?;
        let channel = result
            .get("channels")
            .and_then(Value::as_array)
            .and_then(|channels| {
                channels
                    .iter()
                    .find(|channel| channel.get("id").and_then(Value::as_str) == Some(&channel_id))
            })
            .ok_or_else(|| {
                WaveLinkError::InvalidResponse("focused channel is absent".to_owned())
            })?;
        let mix = channel
            .get("mixes")
            .and_then(Value::as_array)
            .and_then(|mixes| {
                mixes.iter().find(|mix| {
                    mix.get("mixId")
                        .or_else(|| mix.get("id"))
                        .and_then(Value::as_str)
                        == Some(&target_mix_id)
                })
            })
            .ok_or_else(|| WaveLinkError::InvalidResponse("requested mix is absent".to_owned()))?;
        let level = mix
            .get("level")
            .and_then(Value::as_f64)
            .ok_or_else(|| WaveLinkError::InvalidResponse("mix has no level".to_owned()))?
            as f32;
        let muted = mix.get("isMuted").and_then(Value::as_bool).unwrap_or(false);
        let name = channel
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Channel")
            .to_owned();
        let icon = channel
            .pointer("/image/imgData")
            .and_then(Value::as_str)
            .map(|data| VolumeTargetIcon::PngBase64(data.to_owned()))
            .unwrap_or(VolumeTargetIcon::None);
        Ok((channel_id, target_mix_id, level, muted, name, icon))
    }

    /// Returns the selected Wave Link mix id, level, and mute state.
    pub async fn selected_mix(
        &mut self,
        target: MixTarget,
    ) -> Result<(String, f32, bool, String, VolumeTargetIcon), WaveLinkError> {
        let all_mixes = self.call("getMixes", Value::Null).await?;
        let desired_name = match target {
            MixTarget::Monitor => "personal",
            MixTarget::Stream => "stream",
            MixTarget::Chat => "chat",
        };
        let mix = all_mixes
            .get("mixes")
            .and_then(Value::as_array)
            .and_then(|mixes| {
                mixes.iter().find(|mix| {
                    mix.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name.to_ascii_lowercase().contains(desired_name))
                })
            })
            .ok_or_else(|| {
                WaveLinkError::InvalidResponse("requested Wave Link mix is absent".to_owned())
            })?;
        parse_mix(mix)
    }
}

fn normalize_app_name(value: &str) -> String {
    value
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(value)
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

#[cfg(windows)]
fn foreground_process_name() -> Option<String> {
    use windows::{
        core::PWSTR,
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
            UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
        },
    };
    unsafe {
        let window = GetForegroundWindow();
        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let mut buffer = [0u16; 32768];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        result.then(|| normalize_app_name(&String::from_utf16_lossy(&buffer[..length as usize])))
    }
}

#[cfg(not(windows))]
fn foreground_process_name() -> Option<String> {
    None
}

fn focused_channel_from_notification(notification: &Value) -> Option<String> {
    notification
        .pointer("/params/channel/id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// Focused-channel backend that reconnects lazily after a transport failure.
pub struct WaveLinkBackend {
    endpoint: String,
    rpc_timeout: Duration,
    client: Mutex<Option<WaveLinkClient>>,
    focused_channel: Mutex<Option<String>>,
}

impl WaveLinkBackend {
    /// Creates a backend. The first operation performs the connection handshake.
    pub fn new(endpoint: String, rpc_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            rpc_timeout,
            client: Mutex::new(None),
            focused_channel: Mutex::new(None),
        })
    }

    async fn client(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<WaveLinkClient>>, VolumeError> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            let endpoint = if self.endpoint == "auto" {
                discover_endpoint().await
            } else {
                Ok(self.endpoint.clone())
            }
            .map_err(|_| VolumeError::Unavailable)?;
            *guard = Some(
                WaveLinkClient::connect(&endpoint, self.rpc_timeout)
                    .await
                    .map_err(map_error)?,
            );
        }
        Ok(guard)
    }

    /// Reconnects and subscribes to focused-app events until cancelled.
    pub async fn track_focus(self: Arc<Self>, initial: Duration, maximum: Duration) {
        let mut attempt = 0;
        loop {
            let endpoint = if self.endpoint == "auto" {
                discover_endpoint().await
            } else {
                Ok(self.endpoint.clone())
            };
            if let Ok(endpoint) = endpoint {
                if let Ok(mut client) = WaveLinkClient::connect(&endpoint, self.rpc_timeout).await {
                    attempt = 0;
                    let _ = client.refresh_focus_from_foreground().await;
                    *self.focused_channel.lock().await =
                        client.focused_channel().map(ToOwned::to_owned);
                    loop {
                        match timeout(Duration::from_millis(250), client.receive_notification())
                            .await
                        {
                            Ok(Ok(_)) => {
                                if client.refresh_focus_from_foreground().await.is_err() {
                                    break;
                                }
                                *self.focused_channel.lock().await =
                                    client.focused_channel().map(ToOwned::to_owned);
                            }
                            Ok(Err(_)) => break,
                            Err(_) => {
                                if client.refresh_focus_from_foreground().await.is_err() {
                                    break;
                                }
                                *self.focused_channel.lock().await =
                                    client.focused_channel().map(ToOwned::to_owned);
                            }
                        }
                    }
                }
            }
            *self.focused_channel.lock().await = None;
            tokio::time::sleep(reconnect_delay(initial, maximum, attempt)).await;
            attempt = attempt.saturating_add(1);
        }
    }
}

type MixState = (String, f32, bool, String, VolumeTargetIcon);

fn parse_mix(mix: &Value) -> Result<MixState, WaveLinkError> {
    let id = mix
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| WaveLinkError::InvalidResponse("mix has no id".to_owned()))?
        .to_owned();
    let level = mix
        .get("level")
        .and_then(Value::as_f64)
        .ok_or_else(|| WaveLinkError::InvalidResponse("mix has no level".to_owned()))?
        as f32;
    let name = mix
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Mix")
        .to_owned();
    let icon = mix
        .pointer("/image/name")
        .and_then(Value::as_str)
        .map(|name| VolumeTargetIcon::Named(name.to_owned()))
        .unwrap_or(VolumeTargetIcon::None);
    Ok((
        id,
        level,
        mix.get("isMuted").and_then(Value::as_bool).unwrap_or(false),
        name,
        icon,
    ))
}

fn select_mixes(response: &Value, ids: &[String]) -> Result<Vec<MixState>, WaveLinkError> {
    let mixes = response
        .get("mixes")
        .and_then(Value::as_array)
        .ok_or_else(|| WaveLinkError::InvalidResponse("mix list is absent".into()))?;
    ids.iter()
        .map(|id| {
            let mix = mixes
                .iter()
                .find(|mix| mix.get("id").and_then(Value::as_str) == Some(id))
                .ok_or_else(|| {
                    WaveLinkError::InvalidResponse(format!("selected mix {id} is absent"))
                })?;
            parse_mix(mix)
        })
        .collect()
}

fn map_error(error: WaveLinkError) -> VolumeError {
    match error {
        WaveLinkError::RpcTimeout => VolumeError::Timeout,
        other => VolumeError::Protocol(other.to_string()),
    }
}

#[async_trait]
impl VolumeBackend for WaveLinkBackend {
    async fn is_actionable(&self) -> bool {
        self.focused_channel.lock().await.is_some()
    }

    async fn adjust_current_app(
        &self,
        delta: f32,
        target: MixTarget,
    ) -> Result<VolumeUpdate, VolumeError> {
        let mut guard = self.client().await?;
        let client = guard.as_mut().expect("client initialized");
        client.set_focused_channel(self.focused_channel.lock().await.clone());
        let (channel_id, mix_id, level, muted, target_name, icon) =
            client.focused_mix(target).await.map_err(map_error)?;
        let result = adjust_percent(level * 100.0, delta) / 100.0;
        client.call("setChannel", json!({ "id": channel_id, "mixes": [{ "id": mix_id, "isMuted": muted, "level": result }] }))
            .await.map_err(map_error)?;
        Ok(VolumeUpdate {
            percent: result * 100.0,
            target_name,
            icon,
            is_muted: muted,
            show_target_name: true,
            scale: 1.0,
            icon_scale: 1.0,
            font_scale: 1.0,
            font_family: "system".to_owned(),
            hide_delay_seconds: 1.0,
        })
    }

    async fn toggle_mute_current_app(
        &self,
        target: MixTarget,
    ) -> Result<VolumeUpdate, VolumeError> {
        let mut guard = self.client().await?;
        let client = guard.as_mut().expect("client initialized");
        client.set_focused_channel(self.focused_channel.lock().await.clone());
        let (channel_id, mix_id, level, muted, target_name, icon) =
            client.focused_mix(target).await.map_err(map_error)?;
        let new_mute = !muted;
        client.call("setChannel", json!({ "id": channel_id, "mixes": [{ "id": mix_id, "isMuted": new_mute, "level": level }] }))
            .await.map_err(map_error)?;
        Ok(VolumeUpdate {
            percent: level * 100.0,
            target_name,
            icon,
            is_muted: new_mute,
            show_target_name: true,
            scale: 1.0,
            icon_scale: 1.0,
            font_scale: 1.0,
            font_family: "system".to_owned(),
            hide_delay_seconds: 1.0,
        })
    }

    async fn adjust_global_mix(
        &self,
        delta: f32,
        target: MixTarget,
        mix_ids: &[String],
    ) -> Result<VolumeUpdate, VolumeError> {
        let mut guard = self.client().await?;
        let client = guard.as_mut().expect("client initialized");
        let mixes = if mix_ids.is_empty() {
            vec![client.selected_mix(target).await.map_err(map_error)?]
        } else {
            let response = client
                .call("getMixes", Value::Null)
                .await
                .map_err(map_error)?;
            select_mixes(&response, mix_ids).map_err(map_error)?
        };
        for (mix_id, level, muted, _, _) in &mixes {
            let result = adjust_percent(level * 100.0, delta) / 100.0;
            client
                .call("setMix", global_mix_level_params(mix_id, result, *muted))
                .await
                .map_err(map_error)?;
        }
        let (_, level, muted, target_name, icon) =
            mixes.into_iter().next().expect("selection is nonempty");
        let result = adjust_percent(level * 100.0, delta) / 100.0;
        Ok(VolumeUpdate {
            percent: result * 100.0,
            target_name,
            icon,
            is_muted: muted,
            show_target_name: true,
            scale: 1.0,
            icon_scale: 1.0,
            font_scale: 1.0,
            font_family: "system".to_owned(),
            hide_delay_seconds: 1.0,
        })
    }

    async fn toggle_mute_global_mix(
        &self,
        target: MixTarget,
        mix_ids: &[String],
    ) -> Result<VolumeUpdate, VolumeError> {
        let mut guard = self.client().await?;
        let client = guard.as_mut().expect("client initialized");
        let mixes = if mix_ids.is_empty() {
            vec![client.selected_mix(target).await.map_err(map_error)?]
        } else {
            let response = client
                .call("getMixes", Value::Null)
                .await
                .map_err(map_error)?;
            select_mixes(&response, mix_ids).map_err(map_error)?
        };
        let new_mute = mixes.iter().any(|mix| !mix.2);
        for (mix_id, level, _, _, _) in &mixes {
            client
                .call("setMix", global_mix_level_params(mix_id, *level, new_mute))
                .await
                .map_err(map_error)?;
        }
        let (_, level, _, target_name, icon) =
            mixes.into_iter().next().expect("selection is nonempty");
        Ok(VolumeUpdate {
            percent: level * 100.0,
            target_name,
            icon,
            is_muted: new_mute,
            show_target_name: true,
            scale: 1.0,
            icon_scale: 1.0,
            font_scale: 1.0,
            font_family: "system".to_owned(),
            hide_delay_seconds: 1.0,
        })
    }

    async fn status(&self) -> String {
        let guard = self.client.lock().await;
        match self.focused_channel.lock().await.as_deref() {
            Some(channel) => format!("connected; focused channel: {channel}"),
            None if guard.is_some() => "connected; no focused channel".to_owned(),
            None => "disconnected".to_owned(),
        }
    }
}

fn global_mix_level_params(mix_id: &str, level: f32, muted: bool) -> Value {
    // Wave Link 3 requires `mixId` for level changes to a concrete mix.
    // Its official Stream Deck plugin sends both identifiers here.
    json!({ "id": mix_id, "mixId": mix_id, "isMuted": muted, "level": level })
}

/// Produces a capped exponential reconnect delay.
pub fn reconnect_delay(initial: Duration, maximum: Duration, attempt: u32) -> Duration {
    initial
        .saturating_mul(2_u32.saturating_pow(attempt))
        .min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_mix_selection_uses_ids_and_preserves_order() {
        let response = json!({"mixes": [
            {"id":"a", "name":"Renamed", "level":0.99, "isMuted":false},
            {"id":"b", "name":"Other", "level":0.01, "isMuted":true},
            {"id":"c", "name":"TV", "level":0.5, "isMuted":false}
        ]});
        let mixes = select_mixes(&response, &["b".into(), "a".into()]).unwrap();
        assert_eq!(
            mixes.iter().map(|m| m.0.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert!(select_mixes(&response, &["missing".into()]).is_err());
    }

    #[test]
    fn reconnect_backoff_is_capped() {
        assert_eq!(
            reconnect_delay(Duration::from_millis(500), Duration::from_secs(10), 8),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn focused_notification_is_tolerantly_parsed() {
        let notification =
            json!({"method":"focusedAppChanged","params":{"channel":{"id":"Browser"}}});
        assert_eq!(
            focused_channel_from_notification(&notification).as_deref(),
            Some("Browser")
        );
    }

    #[test]
    fn global_mix_level_update_includes_required_mix_id() {
        let params = global_mix_level_params("personal", 0.42, false);
        assert_eq!(params["id"], "personal");
        assert_eq!(params["mixId"], "personal");
        assert_eq!(params["isMuted"], false);
        assert!((params["level"].as_f64().unwrap() - 0.42).abs() < 0.000_001);
    }
}
