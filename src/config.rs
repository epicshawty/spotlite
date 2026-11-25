// SPDX-License-Identifier: AGPL-3.0-or-later
//! Config lives in %APPDATA%\spotlite\config.json and holds OAuth tokens in plain text.
//! That is a deliberate trade-off for a single-user desktop tool; the docs say so.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_port() -> u16 {
    8899
}
fn default_ws() -> u16 {
    9091
}
fn default_device() -> String {
    "Spotlite".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub client_id: String,
    #[serde(default = "default_port")]
    pub redirect_port: u16,
    #[serde(default = "default_ws")]
    pub ws_port: u16,
    #[serde(default = "default_device")]
    pub device_name: String,
    #[serde(default)]
    pub soloist_path: String,
    #[serde(default)]
    pub soloist_key: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub expires_at: u64,
    #[serde(default)]
    pub volume: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            redirect_port: default_port(),
            ws_port: default_ws(),
            device_name: default_device(),
            soloist_path: String::new(),
            soloist_key: String::new(),
            refresh_token: String::new(),
            access_token: String::new(),
            expires_at: 0,
            volume: 70,
        }
    }
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("spotlite")
}

pub fn local_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("spotlite")
}

impl Config {
    pub fn path() -> PathBuf {
        data_dir().join("config.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("config.json"), body).map_err(|e| e.to_string())
    }
}
