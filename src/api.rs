// SPDX-License-Identifier: AGPL-3.0-or-later
//! Spotify Web API client: PKCE login over a loopback redirect, token refresh,
//! and a single worker thread so the UI never blocks on the network.

use crate::model::*;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

pub struct Auth {
    pub access: String,
    pub refresh: String,
    pub expires_at: u64,
}

pub enum Cmd {
    Login {
        client_id: String,
        port: u16,
    },
    Logout,
    Home,
    Playlist(String),
    Album(String),
    Artist(String),
    Search(String),
    PlayContext {
        uri: String,
        index: usize,
        device: Option<String>,
    },
    PlayUris {
        uris: Vec<String>,
        index: usize,
        device: Option<String>,
    },
    Pause,
    Resume,
    Next,
    Prev,
    Seek(u64),
    Volume(u8),
    Shuffle(bool),
    Repeat(String),
    Save {
        uri: String,
        on: bool,
    },
    AddToPlaylist {
        playlist_id: String,
        uri: String,
    },
    Devices,
    Transfer(String),
    Poll,
    Queue,
    Enqueue(String),
}

pub enum Ev {
    LoggedIn(Auth),
    Me(String),
    Playlists(Vec<Playlist>),
    Tracks {
        title: String,
        tracks: Vec<Track>,
    },
    Albums(Vec<Album>),
    Artists(Vec<Artist>),
    Search {
        tracks: Vec<Track>,
        albums: Vec<Album>,
        artists: Vec<Artist>,
        playlists: Vec<Playlist>,
    },
    Devices(Vec<Device>),
    Playback(Playback),
    Queue(Vec<Track>),
    Error(String),
    Done,
}

const SCOPES: &str = "user-read-private user-read-playback-state user-modify-playback-state \
user-read-currently-playing user-read-recently-played user-top-read \
user-library-read user-library-modify \
playlist-read-private playlist-read-collaborative playlist-modify-private playlist-modify-public";

pub fn spawn(auth: Option<Auth>, client_id: String) -> (Sender<Cmd>, Receiver<Ev>) {
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (ev_tx, ev_rx) = channel::<Ev>();
    std::thread::spawn(move || {
        let mut api = Api::new(client_id, auth);
        while let Ok(cmd) = cmd_rx.recv() {
            if let Err(e) = handle(&mut api, cmd, &ev_tx) {
                let _ = ev_tx.send(Ev::Error(e));
            }
        }
    });
    (cmd_tx, ev_rx)
}

struct Api {
    client_id: String,
    auth: Option<Auth>,
    agent: ureq::Agent,
}

impl Api {
    fn new(client_id: String, auth: Option<Auth>) -> Self {
        Self {
            client_id,
            auth,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(20))
                .build(),
        }
    }

    fn access(&mut self) -> Result<String, String> {
        let needs = match &self.auth {
            Some(a) => a.expires_at <= now_ms() / 1000 + 60,
            None => return Err("Not logged in.".into()),
        };
        if needs {
            self.refresh()?;
        }
        Ok(self
            .auth
            .as_ref()
            .map(|a| a.access.clone())
            .unwrap_or_default())
    }

    fn refresh(&mut self) -> Result<(), String> {
        let refresh = self
            .auth
            .as_ref()
            .map(|a| a.refresh.clone())
            .ok_or("Not logged in.")?;
        let body = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", self.client_id.as_str()),
        ];
        let resp = self
            .agent
            .post("https://accounts.spotify.com/api/token")
            .send_form(&body)
            .map_err(friendly)?;
        let v: Value = resp.into_json().map_err(|e| e.to_string())?;
        let access = v["access_token"].as_str().ok_or("Token refresh failed.")?;
        let expires = v["expires_in"].as_u64().unwrap_or(3600);
        if let Some(a) = self.auth.as_mut() {
            a.access = access.to_string();
            if let Some(r) = v["refresh_token"].as_str() {
                a.refresh = r.to_string();
            }
            a.expires_at = now_ms() / 1000 + expires;
        }
        Ok(())
    }

    fn get(&mut self, path: &str) -> Result<Value, String> {
        self.request("GET", path, None)
    }

    fn request(&mut self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let url = format!("https://api.spotify.com/v1{path}");
        let mut retried = false;
        loop {
            let token = self.access()?;
            let req = self
                .agent
                .request(method, &url)
                .set("Authorization", &format!("Bearer {token}"));
            let result = match &body {
                Some(b) => req.send_json(b.clone()),
                None => req.call(),
            };
            match result {
                Ok(resp) => {
                    let text = resp.into_string().unwrap_or_default();
                    return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
                }
                Err(ureq::Error::Status(401, _)) if !retried => {
                    retried = true;
                    self.refresh()?;
                }
                Err(e) => return Err(friendly(e)),
            }
        }
    }
}

fn friendly(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| v.get("error_description").and_then(Value::as_str))
                .or_else(|| v.get("error").and_then(Value::as_str))
                .unwrap_or("");
            if code == 429 {
                "Rate limited by Spotify. Wait a moment.".to_string()
            } else if msg.is_empty() {
                format!("Spotify returned HTTP {code}.")
            } else {
                format!("{msg} ({code})")
            }
        }
        other => other.to_string(),
    }
}

fn handle(api: &mut Api, cmd: Cmd, tx: &Sender<Ev>) -> Result<(), String> {
    match cmd {
        Cmd::Login { client_id, port } => {
            let auth = login(&client_id, port)?;
            api.client_id = client_id;
            api.auth = Some(Auth {
                access: auth.access.clone(),
                refresh: auth.refresh.clone(),
                expires_at: auth.expires_at,
            });
            let _ = tx.send(Ev::LoggedIn(auth));
            home(api, tx);
        }
        Cmd::Logout => {
            api.auth = None;
            let _ = tx.send(Ev::Done);
        }
        Cmd::Home => home(api, tx),
        Cmd::Playlist(id) => {
            let title_path = format!("/playlists/{id}");
            let meta = api.get(&title_path)?;
            let title = meta["name"].as_str().unwrap_or("Playlist").to_string();
            let mut tracks = Vec::new();
            let mut offset = 0;
            loop {
                let v = api.get(&format!("/playlists/{id}/items?limit=100&offset={offset}"))?;
                let items = v["items"].as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    break;
                }
                tracks.extend(items.iter().filter_map(track_from_wrapped));
                offset += 100;
                match v["next"].as_str() {
                    Some(_) => {}
                    None => break,
                }
            }
            let _ = tx.send(Ev::Tracks { title, tracks });
            let _ = tx.send(Ev::Done);
        }
        Cmd::Album(id) => {
            let v = api.get(&format!("/albums/{id}"))?;
            let title = format!(
                "{} — {}",
                v["name"].as_str().unwrap_or("Album"),
                v["artists"]
                    .as_array()
                    .map(|a| a
                        .iter()
                        .filter_map(|x| x["name"].as_str())
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            );
            let mut tracks = Vec::new();
            let mut offset = 0;
            loop {
                let page = api.get(&format!("/albums/{id}/tracks?limit=50&offset={offset}"))?;
                let items = page["items"].as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    break;
                }
                let cover = image_of(&v, false);
                for item in &items {
                    if let Some(mut t) = track_from(item) {
                        t.album = v["name"].as_str().unwrap_or("").to_string();
                        t.image = cover.clone();
                        tracks.push(t);
                    }
                }
                offset += 50;
                if page["next"].is_null() {
                    break;
                }
            }
            let _ = tx.send(Ev::Tracks { title, tracks });
            let _ = tx.send(Ev::Done);
        }
        Cmd::Artist(id) => {
            let v = api.get(&format!(
                "/artists/{id}/albums?include_groups=album,single&limit=50"
            ))?;
            let albums = v["items"]
                .as_array()
                .map(|a| a.iter().filter_map(album_from).collect())
                .unwrap_or_default();
            let _ = tx.send(Ev::Albums(albums));
            let _ = tx.send(Ev::Done);
        }
        Cmd::Search(q) => {
            let mut tracks = Vec::new();
            for page in 0..3 {
                let v = api.get(&format!(
                    "/search?q={}&type=track,album,artist,playlist&limit=10&offset={}",
                    esc(&q),
                    page * 10
                ))?;
                let part: Vec<Track> = v
                    .pointer("/tracks/items")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(track_from).collect())
                    .unwrap_or_default();
                if part.is_empty() {
                    break;
                }
                tracks.extend(part);
            }
            let v = api.get(&format!(
                "/search?q={}&type=track,album,artist,playlist&limit=10",
                esc(&q)
            ))?;
            let albums = v
                .pointer("/albums/items")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(album_from).collect())
                .unwrap_or_default();
            let artists = v
                .pointer("/artists/items")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(artist_from).collect())
                .unwrap_or_default();
            let playlists = v
                .pointer("/playlists/items")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(playlist_from).collect())
                .unwrap_or_default();
            let _ = tx.send(Ev::Search {
                tracks,
                albums,
                artists,
                playlists,
            });
            let _ = tx.send(Ev::Done);
        }
        Cmd::PlayContext { uri, index, device } => {
            let dev = ensure_device(api, device)?;
            let body = if index > 0 {
                json!({ "context_uri": uri, "offset": { "position": index } })
            } else {
                json!({ "context_uri": uri })
            };
            api.request(
                "PUT",
                &format!("/me/player/play?device_id={dev}"),
                Some(body),
            )?;
        }
        Cmd::PlayUris {
            uris,
            index,
            device,
        } => {
            let dev = ensure_device(api, device)?;
            let body = json!({ "uris": uris, "offset": { "position": index } });
            api.request(
                "PUT",
                &format!("/me/player/play?device_id={dev}"),
                Some(body),
            )?;
        }
        Cmd::Pause => {
            api.request("PUT", "/me/player/pause", Some(json!({})))?;
        }
        Cmd::Resume => {
            api.request("PUT", "/me/player/play", Some(json!({})))?;
        }
        Cmd::Next => {
            api.request("POST", "/me/player/next", Some(json!({})))?;
        }
        Cmd::Prev => {
            api.request("POST", "/me/player/previous", Some(json!({})))?;
        }
        Cmd::Seek(ms) => {
            api.request("PUT", &format!("/me/player/seek?position_ms={ms}"), None)?;
        }
        Cmd::Volume(v) => {
            let v = v.min(100);
            api.request(
                "PUT",
                &format!("/me/player/volume?volume_percent={v}"),
                None,
            )?;
        }
        Cmd::Shuffle(on) => {
            api.request("PUT", &format!("/me/player/shuffle?state={on}"), None)?;
        }
        Cmd::Repeat(mode) => {
            api.request("PUT", &format!("/me/player/repeat?state={mode}"), None)?;
        }
        Cmd::Save { uri, on } => {
            api.request(
                if on { "PUT" } else { "DELETE" },
                &format!("/me/library?uris={}", esc(&uri)),
                None,
            )?;
        }
        Cmd::AddToPlaylist { playlist_id, uri } => {
            api.request(
                "POST",
                &format!("/playlists/{playlist_id}/items"),
                Some(json!({ "uris": [uri] })),
            )?;
        }
        Cmd::Devices => {
            let v = api.get("/me/player/devices")?;
            let devices: Vec<Device> = v["devices"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|d| Device {
                            id: d["id"].as_str().unwrap_or_default().to_string(),
                            name: d["name"].as_str().unwrap_or("Device").to_string(),
                            kind: d["type"].as_str().unwrap_or("").to_string(),
                            active: d["is_active"].as_bool().unwrap_or(false),
                            volume: d["volume_percent"].as_u64().unwrap_or(0) as u8,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let _ = tx.send(Ev::Devices(devices));
        }
        Cmd::Transfer(id) => {
            api.request(
                "PUT",
                "/me/player",
                Some(json!({ "device_ids": [id], "play": true })),
            )?;
        }
        Cmd::Poll => {
            let mut req = api
                .agent
                .request("GET", "https://api.spotify.com/v1/me/player");
            let token = api.access()?;
            req = req.set("Authorization", &format!("Bearer {token}"));
            if let Ok(resp) = req.call() {
                let v: Value = resp.into_json().unwrap_or(Value::Null);
                if !v.is_null() {
                    let _ = tx.send(Ev::Playback(parse_player(&v)));
                }
            }
        }
        Cmd::Queue => {
            let v = api.get("/me/player/queue")?;
            let tracks: Vec<Track> = v["queue"]
                .as_array()
                .map(|a| a.iter().filter_map(track_from).collect())
                .unwrap_or_default();
            let _ = tx.send(Ev::Queue(tracks));
        }
        Cmd::Enqueue(uri) => {
            api.request(
                "POST",
                &format!("/me/player/queue?uri={}", esc(&uri)),
                Some(json!({})),
            )?;
        }
    }
    Ok(())
}

fn home(api: &mut Api, tx: &Sender<Ev>) {
    if let Ok(v) = api.get("/me") {
        let name = v["display_name"].as_str().unwrap_or("you").to_string();
        let _ = tx.send(Ev::Me(name));
    }

    let mut playlists = Vec::new();
    let mut offset = 0;
    loop {
        match api.get(&format!("/me/playlists?limit=50&offset={offset}")) {
            Ok(v) => {
                let items = v["items"].as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    break;
                }
                playlists.extend(items.iter().filter_map(playlist_from));
                offset += 50;
                if v["next"].is_null() {
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(Ev::Error(e));
                break;
            }
        }
    }
    let _ = tx.send(Ev::Playlists(playlists));

    let mut tracks = Vec::new();
    let mut offset = 0;
    while let Ok(v) = api.get(&format!("/me/tracks?limit=50&offset={offset}")) {
        let items = v["items"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            break;
        }
        tracks.extend(items.iter().filter_map(track_from_wrapped));
        offset += 50;
        if v["next"].is_null() {
            break;
        }
    }
    let _ = tx.send(Ev::Tracks {
        title: "Saved songs".into(),
        tracks,
    });

    let mut albums = Vec::new();
    let mut offset = 0;
    while let Ok(v) = api.get(&format!("/me/albums?limit=50&offset={offset}")) {
        let items = v["items"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            break;
        }
        albums.extend(
            items
                .iter()
                .filter_map(|i| i.get("album").and_then(album_from)),
        );
        offset += 50;
        if v["next"].is_null() {
            break;
        }
    }
    let _ = tx.send(Ev::Albums(albums));

    if let Ok(v) = api.get("/me/following?type=artist&limit=50") {
        let artists = v
            .pointer("/artists/items")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(artist_from).collect())
            .unwrap_or_default();
        let _ = tx.send(Ev::Artists(artists));
    }
}

fn ensure_device(api: &mut Api, preferred: Option<String>) -> Result<String, String> {
    let v = api.get("/me/player/devices")?;
    let list = v["devices"].as_array().cloned().unwrap_or_default();
    let pick = |name: &str| {
        list.iter()
            .find(|d| d["name"].as_str() == Some(name))
            .and_then(|d| d["id"].as_str())
            .map(str::to_string)
    };
    let id = preferred
        .as_deref()
        .and_then(pick)
        .or_else(|| pick("Spotlite"))
        .or_else(|| {
            list.iter()
                .find(|d| d["is_active"].as_bool().unwrap_or(false))
                .and_then(|d| d["id"].as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            list.first()
                .and_then(|d| d["id"].as_str())
                .map(str::to_string)
        })
        .ok_or(
            "No Spotify device found. Start Soloist, then select \"Spotlite\" in the Spotify app.",
        )?;
    let _ = api.request(
        "PUT",
        "/me/player",
        Some(json!({ "device_ids": [id], "play": false })),
    );
    Ok(id)
}

fn parse_player(v: &Value) -> Playback {
    let track = v.get("item").and_then(track_from);
    Playback {
        playing: v["is_playing"].as_bool().unwrap_or(false),
        track,
        position_ms: v["progress_ms"].as_u64().unwrap_or(0),
        anchor_ms: now_ms(),
        volume: v["device"]["volume_percent"].as_u64().map(|x| x as u8),
        shuffle: v["shuffle_state"].as_bool(),
        repeat: v["repeat_state"].as_str().map(str::to_string),
        device: v["device"]["name"].as_str().unwrap_or("").to_string(),
        active: v["device"]["is_active"].as_bool().unwrap_or(false),
    }
}

// ---------- OAuth ----------

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let _ = getrandom::getrandom(&mut buf);
    buf
}

fn login(client_id: &str, port: u16) -> Result<Auth, String> {
    if client_id.trim().is_empty() {
        return Err("Set your Spotify Client ID first.".into());
    }
    let verifier = b64url(&random_bytes(64));
    let challenge = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
    let state = b64url(&random_bytes(16));
    let redirect = format!("http://127.0.0.1:{port}/callback");

    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("Can't listen on port {port}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let url = format!(
        "https://accounts.spotify.com/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge_method=S256&code_challenge={}",
        esc(client_id),
        esc(&redirect),
        esc(SCOPES),
        state,
        challenge
    );
    open_browser(&url);

    let deadline = Instant::now() + Duration::from_secs(300);
    let (code, got_state) = loop {
        if Instant::now() > deadline {
            return Err("Login timed out. Try again.".into());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                use std::io::{Read, Write};
                stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let first = req.lines().next().unwrap_or("");
                let query = first.split_whitespace().nth(1).unwrap_or("");
                let mut code = String::new();
                let mut got_state = String::new();
                let mut denied = false;
                if let Some(q) = query.split('?').nth(1) {
                    for pair in q.split('&') {
                        let mut it = pair.splitn(2, '=');
                        let k = it.next().unwrap_or("");
                        let val = urldecode(it.next().unwrap_or(""));
                        match k {
                            "code" => code = val,
                            "state" => got_state = val,
                            "error" => denied = true,
                            _ => {}
                        }
                    }
                }
                let body = if denied {
                    "<h1>Access denied</h1><p>Close this tab and try again.</p>"
                } else if code.is_empty() {
                    "<h1>Missing code</h1><p>Close this tab and try again.</p>"
                } else {
                    "<h1>Spotlite is connected</h1><p>You can close this tab.</p>"
                };
                let html = format!(
                    "<!doctype html><meta charset=utf-8><title>Spotlite</title><style>body{{background:#0b0b0c;color:#f2f2f2;font:16px/1.6 Segoe UI,sans-serif;display:grid;place-items:center;height:100vh;margin:0}}div{{text-align:center}}h1{{font-weight:600}}</style><div>{body}</div>"
                );
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        html.len(),
                        html
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
                if denied || code.is_empty() {
                    return Err("Login cancelled.".into());
                }
                break (code, got_state);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    };

    if got_state != state {
        return Err("State mismatch. Try again.".into());
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let body = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", client_id),
        ("code_verifier", verifier.as_str()),
    ];
    let resp = agent
        .post("https://accounts.spotify.com/api/token")
        .send_form(&body)
        .map_err(friendly)?;
    let v: Value = resp.into_json().map_err(|e| e.to_string())?;
    let access = v["access_token"]
        .as_str()
        .ok_or("Spotify did not return an access token.")?
        .to_string();
    let refresh = v["refresh_token"]
        .as_str()
        .ok_or("Spotify did not return a refresh token.")?
        .to_string();
    let expires = v["expires_in"].as_u64().unwrap_or(3600);
    Ok(Auth {
        access,
        refresh,
        expires_at: now_ms() / 1000 + expires,
    })
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}
