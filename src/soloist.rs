// SPDX-License-Identifier: AGPL-3.0-or-later
//! Soloist is Spotify's official headless Connect client. We never touch audio:
//! we start the daemon and drive it over its loopback WebSocket API.

use crate::model::*;
use serde_json::{json, Value};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Sender, TryRecvError};
use std::time::Duration;

#[derive(Default)]
pub struct Proc {
    pub child: Option<Child>,
    pub last_error: Option<String>,
}

impl Proc {
    pub fn running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn start(
        &mut self,
        cfg_path: &str,
        key: &str,
        device: &str,
        ws_port: u16,
    ) -> Result<(), String> {
        self.stop();
        let path = if cfg_path.trim().is_empty() {
            default_path()
        } else {
            PathBuf::from(cfg_path)
        };
        if !path.exists() {
            return Err(format!(
                "soloist.exe not found at {}.\nDownload it from developer.spotify.com/documentation/soloist",
                path.display()
            ));
        }
        if key.trim().is_empty() {
            return Err("Add your Soloist API key first.".into());
        }
        let data = crate::config::data_dir().join("soloist");
        let cache = crate::config::local_dir().join("cache");
        let _ = std::fs::create_dir_all(&data);
        let _ = std::fs::create_dir_all(&cache);

        let mut cmd = Command::new(&path);
        cmd.arg("--device-name")
            .arg(device)
            .arg("--api-key")
            .arg(key)
            .arg("--ws")
            .arg(format!("127.0.0.1:{ws_port}"))
            .arg("--data-dir")
            .arg(&data)
            .arg("--cache-dir")
            .arg(&cache)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.last_error = None;
                Ok(())
            }
            Err(e) => {
                let msg = format!("Could not start Soloist: {e}");
                self.last_error = Some(msg.clone());
                Err(msg)
            }
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn default_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("soloist.exe")))
        .unwrap_or_else(|| Path::new("soloist.exe").to_path_buf())
}

pub enum WsEv {
    Connected,
    Disconnected,
    Auth {
        logged_in: bool,
        active: bool,
        device: String,
    },
    State(Playback),
    Track(Track),
    Playing(bool),
    Position {
        ms: u64,
        anchor_ms: u64,
    },
    Volume(u8),
    Options {
        shuffle: Option<bool>,
        repeat: Option<String>,
    },
    Queue(Vec<Track>),
    Error(String),
}

pub enum WsCmd {
    Send(String),
}

pub fn spawn_ws(addr: String, ev: Sender<WsEv>) -> Sender<WsCmd> {
    let (tx, rx) = channel::<WsCmd>();
    std::thread::spawn(move || loop {
        if let Ok((mut sock, _)) = tungstenite::connect(&addr) {
            if let tungstenite::stream::MaybeTlsStream::Plain(s) = sock.get_ref() {
                let _ = s.set_read_timeout(Some(Duration::from_millis(50)));
            }
            let _ = ev.send(WsEv::Connected);
            let _ = sock.send(tungstenite::Message::Text(
                json!({"type":"command","command":"get_state"}).to_string(),
            ));
            'session: loop {
                loop {
                    match rx.try_recv() {
                        Ok(WsCmd::Send(t)) => {
                            let _ = sock.send(tungstenite::Message::Text(t));
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                match sock.read() {
                    Ok(tungstenite::Message::Text(t)) => {
                        if let Some(e) = parse(&t) {
                            let _ = ev.send(e);
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) => break 'session,
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                    }
                    Err(_) => break 'session,
                }
            }
        }
        let _ = ev.send(WsEv::Disconnected);
        std::thread::sleep(Duration::from_secs(2));
    });
    tx
}

fn parse(text: &str) -> Option<WsEv> {
    let v: Value = serde_json::from_str(text).ok()?;
    match v["type"].as_str()? {
        "auth_state" => Some(WsEv::Auth {
            logged_in: v["logged_in"].as_bool().unwrap_or(false),
            active: v["is_active"].as_bool().unwrap_or(false),
            device: v["device_name"].as_str().unwrap_or("").to_string(),
        }),
        "playback_state" => Some(WsEv::State(playback(&v))),
        "track_changed" => track_from_entity(&v["item"]).map(WsEv::Track),
        "playback_changed" => Some(WsEv::Playing(v["status"].as_str() == Some("playing"))),
        "position_sync" => Some(WsEv::Position {
            ms: v
                .pointer("/position/position_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            anchor_ms: v
                .pointer("/position/timestamp_ms")
                .and_then(Value::as_u64)
                .unwrap_or_else(now_ms),
        }),
        "volume_changed" => Some(WsEv::Volume(v["volume"].as_u64().unwrap_or(0) as u8)),
        "options_changed" => Some(WsEv::Options {
            shuffle: v.pointer("/options/shuffle").and_then(Value::as_bool),
            repeat: v
                .pointer("/options/repeat")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        "queue_changed" => Some(WsEv::Queue(
            v["upcoming"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|q| track_from_entity(&q["item"]))
                        .collect()
                })
                .unwrap_or_default(),
        )),
        "error" => Some(WsEv::Error(
            v["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .unwrap_or("Soloist error")
                .to_string(),
        )),
        _ => None,
    }
}

fn playback(v: &Value) -> Playback {
    Playback {
        playing: v["status"].as_str() == Some("playing"),
        track: track_from_entity(&v["item"]),
        position_ms: v
            .pointer("/position/position_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        anchor_ms: v
            .pointer("/position/timestamp_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_ms),
        volume: v["volume"].as_u64().map(|x| x as u8),
        shuffle: v.pointer("/options/shuffle").and_then(Value::as_bool),
        repeat: v
            .pointer("/options/repeat")
            .and_then(Value::as_str)
            .map(str::to_string),
        device: v["device_name"].as_str().unwrap_or("Spotlite").to_string(),
        active: v["is_active"].as_bool().unwrap_or(false),
    }
}
