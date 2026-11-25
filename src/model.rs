// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plain data types and the narrow, defensive JSON readers for Spotify payloads.
//! Every field Spotify has removed or renamed over the last year is optional here.

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub image: Option<String>,
    pub duration_ms: u64,
    pub playable: bool,
}

impl Track {
    pub fn title(&self) -> &str {
        if self.name.is_empty() {
            "Unknown track"
        } else {
            &self.name
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Playlist {
    pub uri: String,
    pub id: String,
    pub name: String,
    pub image: Option<String>,
    pub owner: String,
    pub count: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Album {
    pub uri: String,
    pub id: String,
    pub name: String,
    pub artists: String,
    pub image: Option<String>,
    pub year: String,
    pub count: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Artist {
    pub uri: String,
    pub id: String,
    pub name: String,
    pub image: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub active: bool,
    pub volume: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Playback {
    pub playing: bool,
    pub track: Option<Track>,
    pub position_ms: u64,
    /// Unix milliseconds at which `position_ms` was true.
    pub anchor_ms: u64,
    pub volume: Option<u8>,
    pub shuffle: Option<bool>,
    pub repeat: Option<String>,
    pub device: String,
    pub active: bool,
}

impl Playback {
    /// Interpolated position, clamped to the track length.
    pub fn position_now(&self) -> u64 {
        if !self.playing {
            return self.position_ms;
        }
        let now = now_ms();
        let drift = now.saturating_sub(self.anchor_ms);
        let pos = self.position_ms.saturating_add(drift);
        match self.track.as_ref() {
            Some(t) if t.duration_ms > 0 => pos.min(t.duration_ms),
            _ => pos,
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn esc(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace('+', "%2B")
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn artists(v: &Value) -> String {
    v.get("artists")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Spotify returns images largest-first. `big` picks the hero size, else the thumbnail.
pub fn image_of(v: &Value, big: bool) -> Option<String> {
    let arr = v.get("images")?.as_array()?;
    let pick = if big { arr.first() } else { arr.last() }?;
    pick.get("url")?.as_str().map(str::to_string)
}

fn image_from_album(v: &Value, big: bool) -> Option<String> {
    image_of(v.get("album").unwrap_or(&Value::Null), big)
}

pub fn track_from(v: &Value) -> Option<Track> {
    let uri = s(v, "uri");
    if uri.is_empty() {
        return None;
    }
    Some(Track {
        uri,
        name: s(v, "name"),
        artists: artists(v),
        album: v.get("album").map(|a| s(a, "name")).unwrap_or_default(),
        image: image_from_album(v, false),
        duration_ms: u(v, "duration_ms"),
        playable: v
            .get("is_playable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

/// `/me/tracks` items wrap the track in `track`; playlist items use `item` (older: `track`).
pub fn track_from_wrapped(v: &Value) -> Option<Track> {
    let inner = v.get("item").or_else(|| v.get("track")).unwrap_or(v);
    track_from(inner)
}

pub fn playlist_from(v: &Value) -> Option<Playlist> {
    let id = s(v, "id");
    if id.is_empty() {
        return None;
    }
    let count = v
        .pointer("/items/total")
        .or_else(|| v.pointer("/tracks/total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(Playlist {
        uri: format!("spotify:playlist:{id}"),
        name: s(v, "name"),
        image: image_of(v, false),
        owner: v
            .pointer("/owner/display_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        count,
        id,
    })
}

pub fn album_from(v: &Value) -> Option<Album> {
    let id = s(v, "id");
    if id.is_empty() {
        return None;
    }
    let year = s(v, "release_date").get(..4).unwrap_or("").to_string();
    Some(Album {
        uri: format!("spotify:album:{id}"),
        name: s(v, "name"),
        artists: artists(v),
        image: image_of(v, false),
        year,
        count: u(v, "total_tracks"),
        id,
    })
}

pub fn artist_from(v: &Value) -> Option<Artist> {
    let id = s(v, "id");
    if id.is_empty() {
        return None;
    }
    Some(Artist {
        uri: format!("spotify:artist:{id}"),
        name: s(v, "name"),
        image: image_of(v, false),
        id,
    })
}

/// Soloist plays entities; this maps its envelope (`decorations`) into a Track.
pub fn track_from_entity(v: &Value) -> Option<Track> {
    let uri = s(v, "uri");
    if uri.is_empty() {
        return None;
    }
    let dec = v.get("decorations").cloned().unwrap_or(Value::Null);
    let cover = dec
        .pointer("/visual_identity/cover")
        .and_then(Value::as_array)
        .and_then(|a| a.last())
        .and_then(|c| c.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let artists = dec
        .get("creators")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.pointer("/entity/decorations/identity/name"))
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let album = dec
        .pointer("/parent/entity/decorations/identity/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(Track {
        uri,
        name: dec
            .pointer("/identity/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        artists,
        album,
        image: cover,
        duration_ms: dec
            .pointer("/playback/duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        playable: true,
    })
}

pub fn fmt_time(ms: u64) -> String {
    let total = ms / 1000;
    let m = total / 60;
    let s = total % 60;
    format!("{m}:{s:02}")
}
