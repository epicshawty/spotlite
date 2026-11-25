// SPDX-License-Identifier: AGPL-3.0-or-later
//! The whole UI. Playback is Spotify Connect (Soloist); the Web API is only used
//! to browse the library, so nothing here touches audio.

use crate::api::{self, Cmd, Ev};
use crate::config::{self, Config};
use crate::images::Images;
use crate::model::*;
use crate::soloist;
use eframe::egui;
use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Margin, Rect, RichText,
    Sense, Stroke, Vec2,
};
use serde_json::json;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

const BG: Color32 = Color32::from_rgb(10, 10, 11);
const SIDEBAR: Color32 = Color32::from_rgb(15, 15, 17);
const PANEL: Color32 = Color32::from_rgb(19, 19, 22);
const HOVER: Color32 = Color32::from_rgb(30, 30, 34);
const LINE: Color32 = Color32::from_rgb(38, 38, 43);
const TEXT: Color32 = Color32::from_rgb(240, 240, 243);
const MUTED: Color32 = Color32::from_rgb(150, 150, 158);
const ACCENT: Color32 = Color32::from_rgb(30, 215, 96);

#[derive(Clone, PartialEq)]
enum View {
    Home,
    Search,
    Playlist(String),
    Album(String),
    Artist(String),
    Settings,
}

enum Act {
    Play(usize),
    Queue(String),
    AddTo(String, String),
    Save(String, bool),
}

pub struct Spotlite {
    cfg: Config,
    draft: Config,
    authed: bool,
    api_tx: Sender<Cmd>,
    api_rx: Receiver<Ev>,
    ws_tx: Sender<soloist::WsCmd>,
    ws_rx: Receiver<soloist::WsEv>,
    ws_addr: String,
    ws_connected: bool,
    ws_logged_in: bool,
    ws_active: bool,
    soloist: soloist::Proc,
    images: Images,
    view: View,
    user: String,
    playlists: Vec<Playlist>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
    tracks: Vec<Track>,
    list_title: String,
    context_uri: Option<String>,
    search_q: String,
    search_tracks: Vec<Track>,
    search_albums: Vec<Album>,
    search_artists: Vec<Artist>,
    search_playlists: Vec<Playlist>,
    devices: Vec<Device>,
    device_id: Option<String>,
    playback: Playback,
    queue: Vec<Track>,
    show_queue: bool,
    seek_drag: Option<f32>,
    vol_drag: Option<f32>,
    loading: bool,
    login_waiting: bool,
    error: Option<String>,
    toast: Option<(String, Instant)>,
    last_poll: Instant,
    last_queue: Instant,
    booted: bool,
}

impl Spotlite {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme(&cc.egui_ctx);
        let cfg = Config::load();
        let auth = if cfg.refresh_token.is_empty() {
            None
        } else {
            Some(api::Auth {
                access: cfg.access_token.clone(),
                refresh: cfg.refresh_token.clone(),
                expires_at: cfg.expires_at,
            })
        };
        let (api_tx, api_rx) = api::spawn(auth, cfg.client_id.clone());
        let ws_addr = format!("127.0.0.1:{}", cfg.ws_port);
        let (ev_tx, ws_rx) = std::sync::mpsc::channel();
        let ws_tx = soloist::spawn_ws(ws_addr.clone(), ev_tx);
        Self {
            draft: cfg.clone(),
            authed: !cfg.refresh_token.is_empty(),
            cfg,
            api_tx,
            api_rx,
            ws_tx,
            ws_rx,
            ws_addr,
            ws_connected: false,
            ws_logged_in: false,
            ws_active: false,
            soloist: soloist::Proc::default(),
            images: Images::new(),
            view: View::Home,
            user: String::new(),
            playlists: vec![],
            albums: vec![],
            artists: vec![],
            tracks: vec![],
            list_title: "Saved songs".into(),
            context_uri: None,
            search_q: String::new(),
            search_tracks: vec![],
            search_albums: vec![],
            search_artists: vec![],
            search_playlists: vec![],
            devices: vec![],
            device_id: None,
            playback: Playback::default(),
            queue: vec![],
            show_queue: false,
            seek_drag: None,
            vol_drag: None,
            loading: false,
            login_waiting: false,
            error: None,
            toast: None,
            last_poll: Instant::now() - Duration::from_secs(30),
            last_queue: Instant::now() - Duration::from_secs(30),
            booted: false,
        }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.api_tx.send(cmd);
    }

    fn ws(&self, v: serde_json::Value) {
        let _ = self.ws_tx.send(soloist::WsCmd::Send(v.to_string()));
    }

    fn load_home(&mut self) {
        self.view = View::Home;
        self.context_uri = None;
        self.list_title = "Saved songs".into();
        self.loading = true;
        self.send(Cmd::Home);
    }

    fn load_playlist(&mut self, p: &Playlist) {
        self.view = View::Playlist(p.id.clone());
        self.context_uri = Some(p.uri.clone());
        self.list_title = p.name.clone();
        self.tracks.clear();
        self.loading = true;
        self.send(Cmd::Playlist(p.id.clone()));
    }

    fn load_album(&mut self, a: &Album) {
        self.view = View::Album(a.id.clone());
        self.context_uri = Some(a.uri.clone());
        self.list_title = a.name.clone();
        self.tracks.clear();
        self.loading = true;
        self.send(Cmd::Album(a.id.clone()));
    }

    fn load_artist(&mut self, a: &Artist) {
        self.view = View::Artist(a.id.clone());
        self.context_uri = None;
        self.list_title = a.name.clone();
        self.albums.clear();
        self.loading = true;
        self.send(Cmd::Artist(a.id.clone()));
    }

    fn do_search(&mut self) {
        let q = self.search_q.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.view = View::Search;
        self.loading = true;
        self.send(Cmd::Search(q));
    }

    fn play_index(&mut self, i: usize) {
        let Some(track) = self.tracks.get(i).cloned() else {
            return;
        };
        if self.ws_connected && self.ws_logged_in && i == 0 {
            let uri = self.context_uri.clone().unwrap_or(track.uri.clone());
            self.ws(json!({"type":"command","command":"play","uri":uri}));
            if !self.ws_active {
                self.ws(json!({"type":"command","command":"activate"}));
            }
            return;
        }
        match self.context_uri.clone() {
            Some(ctx) if matches!(self.view, View::Playlist(_) | View::Album(_)) => {
                self.send(Cmd::PlayContext {
                    uri: ctx,
                    index: i,
                    device: self.device_id.clone(),
                })
            }
            _ => {
                let uris = self.tracks.iter().map(|t| t.uri.clone()).collect();
                self.send(Cmd::PlayUris {
                    uris,
                    index: i,
                    device: self.device_id.clone(),
                });
            }
        }
    }

    fn play_uris(&mut self, uris: Vec<String>, index: usize) {
        if uris.is_empty() {
            return;
        }
        self.send(Cmd::PlayUris {
            uris,
            index,
            device: self.device_id.clone(),
        });
    }

    fn toggle_play(&mut self) {
        if self.ws_connected && self.ws_logged_in {
            let cmd = if self.playback.playing {
                "pause"
            } else {
                "play"
            };
            self.ws(json!({"type":"command","command":cmd}));
            if !self.ws_active {
                self.ws(json!({"type":"command","command":"activate"}));
            }
            self.playback.playing = !self.playback.playing;
            self.playback.anchor_ms = now_ms();
            return;
        }
        if self.playback.playing {
            self.send(Cmd::Pause);
        } else {
            self.send(Cmd::Resume);
        }
        self.playback.playing = !self.playback.playing;
        self.playback.anchor_ms = now_ms();
    }

    fn seek(&mut self, ms: u64) {
        if self.ws_connected && self.ws_logged_in {
            self.ws(json!({"type":"command","command":"seek","position_ms":ms}));
        } else {
            self.send(Cmd::Seek(ms));
        }
        self.playback.position_ms = ms;
        self.playback.anchor_ms = now_ms();
    }

    fn set_volume(&mut self, v: u8) {
        if self.ws_connected && self.ws_logged_in {
            self.ws(json!({"type":"command","command":"set_volume","volume":v}));
        } else {
            self.send(Cmd::Volume(v));
        }
        self.playback.volume = Some(v);
    }

    fn shuffle(&mut self) {
        let on = !self.playback.shuffle.unwrap_or(false);
        if self.ws_connected && self.ws_logged_in {
            self.ws(json!({"type":"command","command":"set_shuffle","enabled":on}));
        } else {
            self.send(Cmd::Shuffle(on));
        }
        self.playback.shuffle = Some(on);
    }

    fn repeat(&mut self) {
        let next = match self.playback.repeat.as_deref() {
            Some("off") | None => "context",
            Some("context") => "track",
            _ => "off",
        }
        .to_string();
        self.send(Cmd::Repeat(next.clone()));
        self.playback.repeat = Some(next);
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.api_rx.try_recv() {
            match ev {
                Ev::LoggedIn(auth) => {
                    self.authed = true;
                    self.login_waiting = false;
                    self.cfg.access_token = auth.access;
                    self.cfg.expires_at = auth.expires_at;
                    self.cfg.refresh_token = auth.refresh;
                    let _ = self.cfg.save();
                    self.draft = self.cfg.clone();
                    self.toast = Some(("Connected to Spotify".into(), Instant::now()));
                }
                Ev::Me(name) => self.user = name,
                Ev::Playlists(v) => self.playlists = v,
                Ev::Tracks { title, tracks } => {
                    self.list_title = title;
                    self.tracks = tracks;
                    self.loading = false;
                }
                Ev::Albums(v) => {
                    self.albums = v;
                    self.loading = false;
                }
                Ev::Artists(v) => self.artists = v,
                Ev::Search {
                    tracks,
                    albums,
                    artists,
                    playlists,
                } => {
                    self.search_tracks = tracks;
                    self.search_albums = albums;
                    self.search_artists = artists;
                    self.search_playlists = playlists;
                    self.loading = false;
                }
                Ev::Devices(v) => {
                    if let Some(d) = v
                        .iter()
                        .find(|d| d.name == self.cfg.device_name)
                        .or_else(|| v.iter().find(|d| d.active))
                        .or_else(|| v.first())
                    {
                        self.device_id = Some(d.id.clone());
                    }
                    self.devices = v;
                }
                Ev::Playback(p) => {
                    if !self.ws_connected {
                        self.playback = p;
                    }
                }
                Ev::Queue(v) => self.queue = v,
                Ev::Error(e) => {
                    self.error = Some(e);
                    self.loading = false;
                    self.login_waiting = false;
                }
                Ev::Done => self.loading = false,
            }
        }

        while let Ok(ev) = self.ws_rx.try_recv() {
            match ev {
                soloist::WsEv::Connected => {
                    self.ws_connected = true;
                    self.send(Cmd::Devices);
                }
                soloist::WsEv::Disconnected => self.ws_connected = false,
                soloist::WsEv::Auth {
                    logged_in,
                    active,
                    device,
                } => {
                    self.ws_logged_in = logged_in;
                    self.ws_active = active;
                    if !device.is_empty() {
                        self.playback.device = device;
                    }
                }
                soloist::WsEv::State(p) => self.playback = p,
                soloist::WsEv::Track(t) => {
                    self.playback.track = Some(t);
                    self.playback.position_ms = 0;
                    self.playback.anchor_ms = now_ms();
                }
                soloist::WsEv::Playing(on) => {
                    if on && !self.playback.playing {
                        self.playback.position_ms = self.playback.position_now();
                        self.playback.anchor_ms = now_ms();
                    }
                    self.playback.playing = on;
                }
                soloist::WsEv::Position { ms, anchor_ms } => {
                    self.playback.position_ms = ms;
                    self.playback.anchor_ms = anchor_ms;
                }
                soloist::WsEv::Volume(v) => self.playback.volume = Some(v),
                soloist::WsEv::Options { shuffle, repeat } => {
                    if shuffle.is_some() {
                        self.playback.shuffle = shuffle;
                    }
                    if repeat.is_some() {
                        self.playback.repeat = repeat;
                    }
                }
                soloist::WsEv::Queue(v) => self.queue = v,
                soloist::WsEv::Error(e) => self.error = Some(e),
            }
        }

        self.images.poll(ctx);

        if let Some((_, at)) = &self.toast {
            if at.elapsed() > Duration::from_secs(3) {
                self.toast = None;
            }
        }
        if self.authed && !self.booted {
            self.booted = true;
            self.load_home();
        }
    }

    fn apply(&mut self, acts: Vec<Act>) {
        for act in acts {
            match act {
                Act::Play(i) => self.play_index(i),
                Act::Queue(uri) => {
                    if self.ws_connected && self.ws_logged_in {
                        self.ws(json!({"type":"command","command":"add_to_queue","uri":uri}));
                        self.toast = Some(("Added to queue".into(), Instant::now()));
                    } else {
                        self.send(Cmd::Enqueue(uri));
                    }
                }
                Act::AddTo(playlist, uri) => {
                    self.send(Cmd::AddToPlaylist {
                        playlist_id: playlist,
                        uri,
                    });
                    self.toast = Some(("Added to playlist".into(), Instant::now()));
                }
                Act::Save(uri, on) => {
                    self.send(Cmd::Save { uri, on });
                    self.toast = Some((
                        if on {
                            "Saved to library"
                        } else {
                            "Removed from library"
                        }
                        .into(),
                        Instant::now(),
                    ));
                }
            }
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if self.authed && !self.ws_connected && self.last_poll.elapsed() > Duration::from_secs(5) {
            self.last_poll = Instant::now();
            self.send(Cmd::Poll);
        }
        if self.ws_connected && self.last_queue.elapsed() > Duration::from_secs(20) {
            self.last_queue = Instant::now();
            self.ws(json!({"type":"command","command":"get_queue","limit":40}));
        }
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.toggle_play();
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    // ---------- chrome ----------

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("SPOTLITE").font(font_bold(15.0)).color(TEXT));
        ui.label(RichText::new("spotify connect").size(10.5).color(MUTED));
        ui.add_space(10.0);

        let search = ui.add(
            egui::TextEdit::singleline(&mut self.search_q)
                .hint_text("Search")
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(10, 7)),
        );
        if search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.do_search();
        }
        ui.add_space(6.0);

        let height = (ui.available_height() - 74.0).max(80.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(height)
            .show(ui, |ui| {
                if nav(ui, self.view == View::Home, "Saved songs") {
                    self.load_home();
                }
                if nav(ui, self.view == View::Search, "Search") {
                    self.view = View::Search;
                }
                ui.add_space(14.0);
                ui.label(RichText::new("PLAYLISTS").size(9.5).color(MUTED));
                ui.add_space(2.0);
                let playlists = self.playlists.clone();
                for p in &playlists {
                    let active = matches!(&self.view, View::Playlist(id) if id == &p.id);
                    if playlist_row(ui, active, &p.name) {
                        self.load_playlist(p);
                    }
                }
                if playlists.is_empty() {
                    ui.label(RichText::new("No playlists").size(12.0).color(MUTED));
                }
            });

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            if nav(ui, self.view == View::Settings, "Settings") {
                self.view = View::Settings;
                self.draft = self.cfg.clone();
            }
            ui.add_space(4.0);
            let (dot, label) = if self.ws_connected && self.ws_logged_in {
                (ACCENT, "Soloist paired")
            } else if self.soloist.running() {
                (Color32::from_rgb(240, 190, 60), "Soloist starting")
            } else {
                (Color32::from_rgb(110, 110, 118), "Soloist off")
            };
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.5, dot);
                ui.label(RichText::new(label).size(11.0).color(MUTED));
            });
        });
    }

    fn onboarding(&mut self, ui: &mut egui::Ui) {
        ui.add_space(56.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("SPOTLITE").font(font_bold(30.0)).color(TEXT));
            ui.label(
                RichText::new("A tiny Windows frontend. Spotify plays through its own engine.")
                    .size(13.0)
                    .color(MUTED),
            );
        });
        ui.add_space(22.0);

        let card_w = 560.0;
        let left = ((ui.available_width() - card_w) / 2.0).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(left);
            ui.vertical(|ui| {
                ui.set_width(card_w);
                egui::Frame::NONE
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0, LINE))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(22))
                    .show(ui, |ui| {
                        ui.set_width(card_w - 44.0);
                        ui.label(
                            RichText::new("1. Create a Spotify app")
                                .font(font_bold(14.0))
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new(
                                "Developer dashboard → Create app. Add this exact redirect URI, then copy the Client ID.",
                            )
                            .size(12.0)
                            .color(MUTED),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let uri =
                                format!("http://127.0.0.1:{}/callback", self.draft.redirect_port);
                            ui.code(uri.clone());
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text(uri);
                            }
                            ui.hyperlink_to("Dashboard", "https://developer.spotify.com/dashboard");
                        });
                        ui.add_space(14.0);

                        ui.label(
                            RichText::new("2. Paste the Client ID")
                                .font(font_bold(14.0))
                                .color(TEXT),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.client_id)
                                .hint_text("Client ID")
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(10, 8)),
                        );
                        ui.add_space(10.0);

                        ui.label(
                            RichText::new("3. Connect")
                                .font(font_bold(14.0))
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new(
                                "A browser opens for Spotify's login. Nothing is stored on a server.",
                            )
                            .size(12.0)
                            .color(MUTED),
                        );
                        ui.add_space(10.0);
                        if self.login_waiting {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(RichText::new("Waiting for your browser…").color(MUTED));
                            });
                        } else if ui
                            .add_sized(
                                [180.0, 34.0],
                                egui::Button::new(
                                    RichText::new("Connect Spotify").color(Color32::BLACK),
                                )
                                .fill(ACCENT),
                            )
                            .clicked()
                        {
                            let id = self.draft.client_id.trim().to_string();
                            if id.is_empty() {
                                self.error = Some("Paste a Client ID first.".into());
                            } else {
                                self.cfg.client_id = id.clone();
                                self.cfg.redirect_port = self.draft.redirect_port;
                                let _ = self.cfg.save();
                                self.login_waiting = true;
                                self.send(Cmd::Login {
                                    client_id: id,
                                    port: self.cfg.redirect_port,
                                });
                            }
                        }
                    });

                if let Some(e) = &self.error {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(e)
                            .size(12.0)
                            .color(Color32::from_rgb(240, 120, 120)),
                    );
                }
            });
        });
    }

    fn main(&mut self, ui: &mut egui::Ui) {
        if self.view == View::Settings {
            self.settings(ui);
            return;
        }

        ui.horizontal(|ui| {
            let (title, sub) = match &self.view {
                View::Home => (
                    self.list_title.clone(),
                    if self.user.is_empty() {
                        format!("{} tracks", self.tracks.len())
                    } else {
                        format!("{} tracks · {}", self.tracks.len(), self.user)
                    },
                ),
                View::Search => ("Search".to_string(), self.search_q.clone()),
                View::Playlist(_) => (
                    self.list_title.clone(),
                    format!("{} tracks", self.tracks.len()),
                ),
                View::Album(_) => (
                    self.list_title.clone(),
                    format!("{} tracks", self.tracks.len()),
                ),
                View::Artist(_) => (self.list_title.clone(), "Albums".to_string()),
                View::Settings => ("Settings".into(), String::new()),
            };
            ui.vertical(|ui| {
                ui.label(RichText::new(title).font(font_bold(26.0)).color(TEXT));
                ui.label(RichText::new(sub).size(11.5).color(MUTED));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.loading {
                    ui.spinner();
                }
                let can_play = !self.tracks.is_empty() && self.view != View::Search;
                if ui
                    .add_enabled(
                        can_play,
                        egui::Button::new(RichText::new("Play").color(Color32::BLACK)).fill(ACCENT),
                    )
                    .clicked()
                {
                    self.play_index(0);
                }
                if ui.button("Refresh").clicked() {
                    match &self.view {
                        View::Search => self.do_search(),
                        _ => self.load_home(),
                    }
                }
            });
        });

        ui.add_space(6.0);
        if let Some(e) = self.error.clone() {
            egui::Frame::NONE
                .fill(Color32::from_rgb(48, 22, 24))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(e)
                                .size(12.0)
                                .color(Color32::from_rgb(245, 180, 180)),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                self.error = None;
                            }
                        });
                    });
                });
            ui.add_space(6.0);
        }

        match self.view.clone() {
            View::Search => self.search_results(ui),
            View::Artist(_) => {
                let albums = self.albums.clone();
                if albums.is_empty() {
                    ui.label(RichText::new("No albums.").color(MUTED));
                }
                let mut picked = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for a in &albums {
                                if card(
                                    ui,
                                    &mut self.images,
                                    &a.name,
                                    &a.artists,
                                    a.image.as_deref(),
                                ) {
                                    picked = Some(a.clone());
                                }
                            }
                        });
                    });
                if let Some(a) = picked {
                    self.load_album(&a);
                }
            }
            _ => {
                let playlists: Vec<(String, String)> = self
                    .playlists
                    .iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect();
                let mut acts = track_list(
                    ui,
                    &mut self.images,
                    &self.tracks,
                    &self.playback,
                    &playlists,
                );
                if self.tracks.is_empty() && !self.loading {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Nothing here yet.").size(13.0).color(MUTED));
                    });
                }
                if self.view == View::Home {
                    ui.add_space(14.0);
                    let albums = self.albums.clone();
                    if !albums.is_empty() {
                        ui.label(RichText::new("SAVED ALBUMS").size(9.5).color(MUTED));
                        ui.add_space(4.0);
                        let mut picked = None;
                        ui.horizontal_wrapped(|ui| {
                            for a in &albums {
                                if card(
                                    ui,
                                    &mut self.images,
                                    &a.name,
                                    &a.artists,
                                    a.image.as_deref(),
                                ) {
                                    picked = Some(a.clone());
                                }
                            }
                        });
                        if let Some(a) = picked {
                            self.load_album(&a);
                            acts.clear();
                        }
                    }
                }
                self.apply(acts);
            }
        }
    }

    fn search_results(&mut self, ui: &mut egui::Ui) {
        if self.search_tracks.is_empty()
            && self.search_albums.is_empty()
            && self.search_artists.is_empty()
        {
            ui.label(RichText::new("Type a query and press Enter.").color(MUTED));
            return;
        }
        let playlists: Vec<(String, String)> = self
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let mut picked_album = None;
        let mut picked_artist = None;
        let mut picked_playlist = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !self.search_artists.is_empty() {
                    ui.label(RichText::new("ARTISTS").size(9.5).color(MUTED));
                    ui.add_space(2.0);
                    let artists = self.search_artists.clone();
                    ui.horizontal_wrapped(|ui| {
                        for a in &artists {
                            if card(ui, &mut self.images, &a.name, "Artist", a.image.as_deref()) {
                                picked_artist = Some(a.clone());
                            }
                        }
                    });
                    ui.add_space(10.0);
                }
                if !self.search_albums.is_empty() {
                    ui.label(RichText::new("ALBUMS").size(9.5).color(MUTED));
                    ui.add_space(2.0);
                    let albums = self.search_albums.clone();
                    ui.horizontal_wrapped(|ui| {
                        for a in &albums {
                            if card(
                                ui,
                                &mut self.images,
                                &a.name,
                                &a.artists,
                                a.image.as_deref(),
                            ) {
                                picked_album = Some(a.clone());
                            }
                        }
                    });
                    ui.add_space(10.0);
                }
                if !self.search_playlists.is_empty() {
                    ui.label(RichText::new("PLAYLISTS").size(9.5).color(MUTED));
                    ui.add_space(2.0);
                    let lists = self.search_playlists.clone();
                    for p in &lists {
                        if playlist_row(ui, false, &p.name) {
                            picked_playlist = Some(p.clone());
                        }
                    }
                    ui.add_space(10.0);
                }
                if !self.search_tracks.is_empty() {
                    ui.label(RichText::new("TRACKS").size(9.5).color(MUTED));
                    ui.add_space(2.0);
                    let mut acts = track_list(
                        ui,
                        &mut self.images,
                        &self.search_tracks,
                        &self.playback,
                        &playlists,
                    );
                    let uris: Vec<String> =
                        self.search_tracks.iter().map(|t| t.uri.clone()).collect();
                    for a in &acts {
                        if let Act::Play(i) = a {
                            self.send(Cmd::PlayUris {
                                uris: uris.clone(),
                                index: *i,
                                device: self.device_id.clone(),
                            });
                        }
                    }
                    acts.retain(|a| !matches!(a, Act::Play(_)));
                    self.apply(acts);
                }
            });
        if let Some(a) = picked_artist {
            self.load_artist(&a);
        }
        if let Some(a) = picked_album {
            self.load_album(&a);
        }
        if let Some(p) = picked_playlist {
            self.load_playlist(&p);
        }
    }

    fn player(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_width();
        let left_w = 280.0_f32.min(full * 0.3);
        let right_w = 330.0_f32.min(full * 0.35);
        let center_w = (full - left_w - right_w).max(240.0);

        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(left_w, 76.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(left_w);
                    let track = self.playback.track.clone().unwrap_or_default();
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
                    let mut drew = false;
                    if let Some(url) = &track.image {
                        if let Some(t) = self.images.texture(ui.ctx(), url) {
                            ui.painter().image(
                                t.id(),
                                rect,
                                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                            drew = true;
                        }
                    }
                    if !drew {
                        ui.painter().rect_filled(rect, CornerRadius::same(6), LINE);
                    }
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.add_space(9.0);
                        ui.label(
                            RichText::new(if track.uri.is_empty() {
                                "Nothing playing"
                            } else {
                                track.title()
                            })
                            .font(font_bold(13.5))
                            .color(TEXT),
                        );
                        ui.label(
                            RichText::new(if track.artists.is_empty() {
                                self.playback.device.clone()
                            } else {
                                track.artists.clone()
                            })
                            .size(11.5)
                            .color(MUTED),
                        );
                        if !track.uri.is_empty() {
                            let saved = heart_button(ui).on_hover_text("Save to library");
                            if saved.clicked() {
                                self.send(Cmd::Save {
                                    uri: track.uri.clone(),
                                    on: true,
                                });
                            }
                        }
                    });
                },
            );

            ui.allocate_ui_with_layout(
                Vec2::new(center_w, 76.0),
                Layout::top_down(Align::Center),
                |ui| {
                    ui.set_min_width(center_w);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        let pad = ((center_w - (34.0 * 2.0 + 38.0)) / 2.0).max(0.0);
                        ui.add_space(pad);
                        if icon_skip(ui, false).clicked() {
                            if self.ws_connected && self.ws_logged_in {
                                self.ws(json!({"type":"command","command":"skip_prev"}));
                            } else {
                                self.send(Cmd::Prev);
                            }
                        }
                        if icon_play(ui, self.playback.playing).clicked() {
                            self.toggle_play();
                        }
                        if icon_skip(ui, true).clicked() {
                            if self.ws_connected && self.ws_logged_in {
                                self.ws(json!({"type":"command","command":"skip_next"}));
                            } else {
                                self.send(Cmd::Next);
                            }
                        }
                    });
                    let dur = self
                        .playback
                        .track
                        .as_ref()
                        .map(|t| t.duration_ms)
                        .unwrap_or(0) as f32;
                    let now = self.playback.position_now() as f32;
                    let mut val = self.seek_drag.unwrap_or(now).min(dur.max(1.0));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(fmt_time(val as u64)).size(10.0).color(MUTED));
                        ui.spacing_mut().slider_width = (center_w - 108.0).max(80.0);
                        let resp = ui
                            .add(egui::Slider::new(&mut val, 0.0..=dur.max(1.0)).show_value(false));
                        if resp.drag_started() || resp.dragged() {
                            self.seek_drag = Some(val);
                        }
                        if resp.drag_stopped() {
                            self.seek_drag = None;
                            self.seek(val as u64);
                        }
                        ui.label(RichText::new(fmt_time(dur as u64)).size(10.0).color(MUTED));
                    });
                },
            );

            ui.allocate_ui_with_layout(
                Vec2::new(right_w, 76.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(right_w);
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let sh = self.playback.shuffle.unwrap_or(false);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("SHUFFLE").size(9.5).color(if sh {
                                ACCENT
                            } else {
                                MUTED
                            }))
                            .frame(false),
                        )
                        .clicked()
                    {
                        self.shuffle();
                    }
                    let rep = self.playback.repeat.clone().unwrap_or_else(|| "off".into());
                    let label = match rep.as_str() {
                        "track" => "REPEAT 1",
                        "context" => "REPEAT ALL",
                        _ => "REPEAT",
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(label).size(9.5).color(if rep == "off" {
                                    MUTED
                                } else {
                                    ACCENT
                                }),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        self.repeat();
                    }
                    ui.label(RichText::new("VOL").size(9.0).color(MUTED));
                    let vol = self
                        .vol_drag
                        .or(self.playback.volume.map(|v| v as f32))
                        .unwrap_or(70.0);
                    let mut v = vol;
                    ui.spacing_mut().slider_width = 64.0;
                    let resp = ui.add(egui::Slider::new(&mut v, 0.0..=100.0).show_value(false));
                    if resp.dragged() {
                        self.vol_drag = Some(v);
                    }
                    if resp.drag_stopped() {
                        self.vol_drag = None;
                        self.set_volume(v as u8);
                    }
                    if icon_queue(ui, self.show_queue).clicked() {
                        self.show_queue = !self.show_queue;
                    }
                    let name = if self.playback.device.is_empty() {
                        "Connect".to_string()
                    } else {
                        self.playback.device.chars().take(14).collect()
                    };
                    ui.menu_button(RichText::new(name).size(10.5).color(MUTED), |ui| {
                        ui.set_min_width(200.0);
                        let devices = self.devices.clone();
                        if devices.is_empty() {
                            ui.label(RichText::new("No devices").color(MUTED));
                        }
                        for d in devices {
                            let mark = if d.active { "● " } else { "   " };
                            if ui.button(format!("{mark}{}", d.name)).clicked() {
                                self.send(Cmd::Transfer(d.id.clone()));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Refresh devices").clicked() {
                            self.send(Cmd::Devices);
                            ui.close();
                        }
                    });
                },
            );
        });
    }

    fn queue_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Queue").font(font_bold(15.0)).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("x").clicked() {
                    self.show_queue = false;
                }
            });
        });
        ui.add_space(4.0);
        if self.queue.is_empty() {
            ui.label(RichText::new("Queue is empty.").size(12.0).color(MUTED));
            return;
        }
        let queue = self.queue.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, t) in queue.iter().enumerate() {
                    if playlist_row(ui, false, t.title()) {
                        let uris: Vec<String> = queue.iter().map(|t| t.uri.clone()).collect();
                        self.play_uris(uris, i);
                    }
                }
            });
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Settings").font(font_bold(26.0)).color(TEXT));
        ui.label(
            RichText::new("Stored in %APPDATA%\\spotlite\\config.json — tokens included.")
                .size(11.5)
                .color(MUTED),
        );
        ui.add_space(12.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::NONE
                    .fill(PANEL)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Spotify app").font(font_bold(14.0)).color(TEXT));
                        ui.label(
                            RichText::new("Redirect URI must match exactly. Developer mode allows 5 users and needs Premium.")
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new("Client ID").size(11.0).color(MUTED));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.client_id)
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(9, 7)),
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new("Redirect URI").size(11.0).color(MUTED));
                        let uri = format!("http://127.0.0.1:{}/callback", self.draft.redirect_port);
                        ui.horizontal(|ui| {
                            ui.code(uri.clone());
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text(uri);
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("Callback port").size(11.0).color(MUTED));
                        ui.add(
                            egui::DragValue::new(&mut self.draft.redirect_port).range(1024..=65535),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Connect / re-login").color(Color32::BLACK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                self.cfg = self.draft.clone();
                                let _ = self.cfg.save();
                                self.login_waiting = true;
                                self.send(Cmd::Login {
                                    client_id: self.cfg.client_id.clone(),
                                    port: self.cfg.redirect_port,
                                });
                            }
                            if ui.button("Log out").clicked() {
                                self.cfg.refresh_token.clear();
                                self.cfg.access_token.clear();
                                self.cfg.expires_at = 0;
                                let _ = self.cfg.save();
                                self.authed = false;
                                self.send(Cmd::Logout);
                            }
                            if self.login_waiting {
                                ui.spinner();
                            }
                        });
                    });

                ui.add_space(12.0);

                egui::Frame::NONE
                    .fill(PANEL)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Spotify Soloist").font(font_bold(14.0)).color(TEXT));
                        ui.label(
                            RichText::new(
                                "Official headless player. Lossless, Jam and autoplay come from it. Builds expire after 90 days — download a fresh soloist.exe when it stops.",
                            )
                            .size(11.0)
                            .color(MUTED),
                        );
                        ui.add_space(4.0);
                        ui.hyperlink_to(
                            "Download Soloist",
                            "https://developer.spotify.com/documentation/soloist/reference/downloads-and-updates",
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new("soloist.exe path").size(11.0).color(MUTED));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.soloist_path)
                                    .hint_text(soloist::default_path().display().to_string())
                                    .desired_width(ui.available_width() - 74.0)
                                    .margin(Margin::symmetric(9, 7)),
                            );
                            if ui.button("Detect").clicked() {
                                if soloist::default_path().exists() {
                                    self.draft.soloist_path =
                                        soloist::default_path().display().to_string();
                                } else {
                                    self.error = Some(
                                        "soloist.exe isn't next to spotlite.exe. Put it there or paste a path."
                                            .into(),
                                    );
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("API key").size(11.0).color(MUTED));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.soloist_key)
                                .password(true)
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(9, 7)),
                        );
                        ui.label(
                            RichText::new("Each user generates their own key; do not share it.")
                                .size(10.5)
                                .color(MUTED),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Device name").size(11.0).color(MUTED));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.device_name)
                                    .desired_width(140.0),
                            );
                            ui.label(RichText::new("WS port").size(11.0).color(MUTED));
                            ui.add(egui::DragValue::new(&mut self.draft.ws_port).range(1024..=65535));
                        });
                        ui.add_space(10.0);
                        let running = self.soloist.running();
                        ui.horizontal(|ui| {
                            if running {
                                if ui.button("Stop Soloist").clicked() {
                                    self.soloist.stop();
                                }
                            } else if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Start Soloist").color(Color32::BLACK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                self.cfg = self.draft.clone();
                                let _ = self.cfg.save();
                                if let Err(e) = self.soloist.start(
                                    &self.cfg.soloist_path,
                                    &self.cfg.soloist_key,
                                    &self.cfg.device_name,
                                    self.cfg.ws_port,
                                ) {
                                    self.error = Some(e);
                                }
                            }
                            if ui.button("Open data folder").clicked() {
                                let dir = config::data_dir();
                                let _ = std::fs::create_dir_all(&dir);
                                let _ = std::process::Command::new("explorer").arg(dir).spawn();
                            }
                            ui.label(
                                RichText::new(if running { "running" } else { "stopped" })
                                    .size(11.0)
                                    .color(if running { ACCENT } else { MUTED }),
                            );
                        });
                        if !self.ws_connected {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    "Pairing: start Soloist, open the Spotify app, pick the device, then return here.",
                                )
                                .size(11.0)
                                .color(Color32::from_rgb(230, 190, 90)),
                            );
                        }
                    });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("Save").color(Color32::BLACK)).fill(ACCENT))
                        .clicked()
                    {
                        let old_ws = self.cfg.ws_port;
                        self.cfg = self.draft.clone();
                        match self.cfg.save() {
                            Ok(_) => {
                                self.toast = Some(("Saved".into(), Instant::now()));
                                if old_ws != self.cfg.ws_port {
                                    self.ws_addr = format!("127.0.0.1:{}", self.cfg.ws_port);
                                    let (ev_tx, ws_rx) = std::sync::mpsc::channel();
                                    self.ws_tx = soloist::spawn_ws(self.ws_addr.clone(), ev_tx);
                                    self.ws_rx = ws_rx;
                                    self.ws_connected = false;
                                }
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                    if let Some((msg, _)) = &self.toast {
                        ui.label(RichText::new(msg).size(12.0).color(ACCENT));
                    }
                });
                ui.add_space(20.0);
                ui.label(
                    RichText::new("Spotlite is not affiliated with Spotify. AGPL-3.0-or-later.")
                        .size(10.5)
                        .color(MUTED),
                );
            });
    }
}

impl eframe::App for Spotlite {
    #[allow(deprecated)]
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        self.tick(&ctx);

        if !self.authed || self.cfg.client_id.trim().is_empty() {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(BG)
                        .inner_margin(Margin::symmetric(24, 18)),
                )
                .show(ui, |ui| self.onboarding(ui));
            return;
        }

        egui::Panel::left("sidebar")
            .exact_size(236.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(SIDEBAR).inner_margin(Margin {
                left: 14,
                right: 14,
                top: 14,
                bottom: 12,
            }))
            .show(ui, |ui| self.sidebar(ui));

        egui::Panel::bottom("player")
            .exact_size(94.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(PANEL).inner_margin(Margin {
                left: 16,
                right: 16,
                top: 8,
                bottom: 8,
            }))
            .show(ui, |ui| self.player(ui));

        if self.show_queue {
            egui::Panel::right("queue")
                .exact_size(296.0)
                .resizable(false)
                .frame(egui::Frame::NONE.fill(SIDEBAR).inner_margin(Margin {
                    left: 14,
                    right: 14,
                    top: 14,
                    bottom: 12,
                }))
                .show(ui, |ui| self.queue_panel(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin {
                left: 24,
                right: 24,
                top: 16,
                bottom: 8,
            }))
            .show(ui, |ui| self.main(ui));

        if let Some((msg, _)) = &self.toast {
            let screen = ctx.content_rect();
            egui::Area::new(egui::Id::new("toast"))
                .fixed_pos(egui::pos2(
                    screen.center().x - 90.0,
                    screen.bottom() - 108.0,
                ))
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(28, 28, 32))
                        .stroke(Stroke::new(1.0, LINE))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.label(RichText::new(msg).size(12.0).color(TEXT));
                        });
                });
        }
    }
}

// ---------- widgets ----------

fn font_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn nav(ui: &mut egui::Ui, active: bool, label: &str) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
    if active {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), Color32::from_rgb(30, 30, 34));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(7), HOVER);
    }
    let p = ui.painter().with_clip_rect(rect);
    p.text(
        egui::pos2(rect.left() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.5),
        if active { TEXT } else { MUTED },
    );
    resp.clicked()
}

fn playlist_row(ui: &mut egui::Ui, active: bool, label: &str) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    if active {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(30, 30, 34));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), HOVER);
    }
    let p = ui.painter().with_clip_rect(rect);
    p.text(
        egui::pos2(rect.left() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        if active { TEXT } else { MUTED },
    );
    resp.clicked()
}

fn heart_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(20.0, 18.0), Sense::click());
    let c = rect.center();
    let col = if resp.hovered() { TEXT } else { MUTED };
    let r = 4.0;
    let p = ui.painter();
    p.circle_filled(egui::pos2(c.x - r * 0.8, c.y - 2.0), r, col);
    p.circle_filled(egui::pos2(c.x + r * 0.8, c.y - 2.0), r, col);
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(c.x - r * 1.62, c.y - 1.2),
            egui::pos2(c.x + r * 1.62, c.y - 1.2),
            egui::pos2(c.x, c.y + r * 1.9),
        ],
        col,
        Stroke::NONE,
    ));
    resp
}

fn icon_play(ui: &mut egui::Ui, playing: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
    let p = ui.painter();
    let c = rect.center();
    p.circle_filled(c, 19.0, if resp.hovered() { Color32::WHITE } else { TEXT });
    if playing {
        p.rect_filled(
            Rect::from_center_size(c - Vec2::new(4.0, 0.0), Vec2::new(4.0, 12.0)),
            CornerRadius::same(1),
            BG,
        );
        p.rect_filled(
            Rect::from_center_size(c + Vec2::new(4.0, 0.0), Vec2::new(4.0, 12.0)),
            CornerRadius::same(1),
            BG,
        );
    } else {
        p.add(egui::Shape::convex_polygon(
            vec![
                c + Vec2::new(-4.0, -7.0),
                c + Vec2::new(-4.0, 7.0),
                c + Vec2::new(8.0, 0.0),
            ],
            BG,
            Stroke::NONE,
        ));
    }
    resp
}

fn icon_skip(ui: &mut egui::Ui, forward: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
    let p = ui.painter();
    let c = rect.center();
    let color = if resp.hovered() { TEXT } else { MUTED };
    let d = if forward { 1.0 } else { -1.0 };
    p.add(egui::Shape::convex_polygon(
        vec![
            c + Vec2::new(-6.0 * d, -6.0),
            c + Vec2::new(-6.0 * d, 6.0),
            c + Vec2::new(3.0 * d, 0.0),
        ],
        color,
        Stroke::NONE,
    ));
    p.rect_filled(
        Rect::from_center_size(c + Vec2::new(6.0 * d, 0.0), Vec2::new(2.0, 12.0)),
        CornerRadius::same(1),
        color,
    );
    resp
}

fn icon_queue(ui: &mut egui::Ui, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let p = ui.painter();
    let color = if active {
        ACCENT
    } else if resp.hovered() {
        TEXT
    } else {
        MUTED
    };
    for i in 0..3 {
        let y = rect.center().y - 4.0 + i as f32 * 4.0;
        p.line_segment(
            [
                egui::pos2(rect.center().x - 7.0, y),
                egui::pos2(rect.center().x + 7.0, y),
            ],
            Stroke::new(1.5, color),
        );
    }
    resp
}

fn card(
    ui: &mut egui::Ui,
    images: &mut Images,
    title: &str,
    sub: &str,
    image: Option<&str>,
) -> bool {
    let w = 148.0;
    let h = 190.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let art = Rect::from_min_size(rect.min, Vec2::splat(w));
    let mut drew = false;
    if let Some(url) = image {
        if let Some(t) = images.texture(ui.ctx(), url) {
            ui.painter().image(
                t.id(),
                art,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            drew = true;
        }
    }
    if !drew {
        ui.painter().rect_filled(art, CornerRadius::ZERO, LINE);
    }
    if resp.hovered() {
        ui.painter().rect_stroke(
            art,
            CornerRadius::ZERO,
            Stroke::new(1.0, Color32::from_white_alpha(40)),
            egui::StrokeKind::Inside,
        );
    }
    let p = ui.painter().with_clip_rect(rect);
    p.text(
        egui::pos2(rect.left() + 2.0, art.bottom() + 10.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(12.5),
        TEXT,
    );
    p.text(
        egui::pos2(rect.left() + 2.0, art.bottom() + 28.0),
        Align2::LEFT_TOP,
        sub,
        FontId::proportional(10.5),
        MUTED,
    );
    resp.clicked()
}

fn track_list(
    ui: &mut egui::Ui,
    images: &mut Images,
    tracks: &[Track],
    playback: &Playback,
    playlists: &[(String, String)],
) -> Vec<Act> {
    let mut acts = Vec::new();
    if tracks.is_empty() {
        return acts;
    }
    let row_h = 46.0;
    let current = playback
        .track
        .as_ref()
        .map(|t| t.uri.clone())
        .unwrap_or_default();
    egui::ScrollArea::vertical()
        .id_salt("tracks")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, tracks.len(), |ui, range| {
            for i in range {
                let t = &tracks[i];
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                let p = ui.painter().with_clip_rect(rect);
                if resp.hovered() {
                    p.rect_filled(rect, CornerRadius::same(7), HOVER);
                }
                let y = rect.center().y;
                let is_current = !current.is_empty() && t.uri == current;
                if is_current {
                    for k in 0..3 {
                        let h = 4.0 + ((k * 3) % 6) as f32;
                        p.rect_filled(
                            Rect::from_min_size(
                                egui::pos2(rect.left() + 14.0 + k as f32 * 3.0, y - h / 2.0),
                                Vec2::new(2.0, h),
                            ),
                            CornerRadius::ZERO,
                            ACCENT,
                        );
                    }
                } else {
                    p.text(
                        egui::pos2(rect.left() + 16.0, y),
                        Align2::LEFT_CENTER,
                        format!("{}", i + 1),
                        FontId::proportional(11.5),
                        MUTED,
                    );
                }

                let art =
                    Rect::from_center_size(egui::pos2(rect.left() + 52.0, y), Vec2::splat(32.0));
                let mut drew = false;
                if let Some(url) = &t.image {
                    if let Some(tex) = images.texture(ui.ctx(), url) {
                        p.image(
                            tex.id(),
                            art,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        drew = true;
                    }
                }
                if !drew {
                    p.rect_filled(art, CornerRadius::same(4), LINE);
                }

                let text_clip = Rect::from_min_max(
                    egui::pos2(rect.left() + 76.0, rect.top()),
                    egui::pos2(rect.right() - 126.0, rect.bottom()),
                );
                let tp = ui.painter().with_clip_rect(text_clip);
                tp.text(
                    egui::pos2(rect.left() + 76.0, y - 2.0),
                    Align2::LEFT_BOTTOM,
                    t.title(),
                    FontId::proportional(13.5),
                    if is_current { ACCENT } else { TEXT },
                );
                tp.text(
                    egui::pos2(rect.left() + 76.0, y + 3.0),
                    Align2::LEFT_TOP,
                    &t.artists,
                    FontId::proportional(11.0),
                    MUTED,
                );

                p.text(
                    egui::pos2(rect.right() - 14.0, y),
                    Align2::RIGHT_CENTER,
                    fmt_time(t.duration_ms),
                    FontId::proportional(11.5),
                    MUTED,
                );

                if resp.clicked() {
                    acts.push(Act::Play(i));
                }
                resp.context_menu(|ui| {
                    if ui.button("Play now").clicked() {
                        acts.push(Act::Play(i));
                        ui.close();
                    }
                    if ui.button("Add to queue").clicked() {
                        acts.push(Act::Queue(t.uri.clone()));
                        ui.close();
                    }
                    if ui.button("Save to library").clicked() {
                        acts.push(Act::Save(t.uri.clone(), true));
                        ui.close();
                    }
                    if ui.button("Remove from library").clicked() {
                        acts.push(Act::Save(t.uri.clone(), false));
                        ui.close();
                    }
                    ui.menu_button("Add to playlist", |ui| {
                        ui.set_min_width(180.0);
                        if playlists.is_empty() {
                            ui.label(RichText::new("No playlists").color(MUTED));
                        }
                        for (id, name) in playlists {
                            if ui.button(name).clicked() {
                                acts.push(Act::AddTo(id.clone(), t.uri.clone()));
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Copy track URI").clicked() {
                        ui.ctx().copy_text(t.uri.clone());
                        ui.close();
                    }
                });
            }
        });
    acts
}

fn theme(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);

    let mut fonts = egui::FontDefinitions::default();
    let prop = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut regular = prop.clone();
    let mut bold_stack: Vec<String> = Vec::new();
    for path in [
        r"C:\Windows\Fonts\seguisb.ttf",
        r"C:\Windows\Fonts\segoeuib.ttf",
    ] {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(
                "segoe-bold".to_string(),
                std::sync::Arc::new(egui::FontData::from_owned(data)),
            );
            bold_stack.push("segoe-bold".to_string());
            break;
        }
    }
    if let Ok(data) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert(
            "segoe".to_string(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
        regular.insert(0, "segoe".to_string());
    }
    bold_stack.extend(regular.clone());
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts
        .families
        .insert(FontFamily::Name("bold".into()), bold_stack);
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(24, 24, 27);
    v.faint_bg_color = PANEL;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.hyperlink_color = ACCENT;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(26, 26, 30);
    v.widgets.inactive.bg_fill = Color32::from_rgb(48, 48, 54);
    v.widgets.inactive.corner_radius = CornerRadius::same(7);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(40, 40, 45);
    v.widgets.hovered.bg_fill = Color32::from_rgb(58, 58, 65);
    v.widgets.hovered.corner_radius = CornerRadius::same(7);
    v.widgets.active.corner_radius = CornerRadius::same(7);

    ctx.all_styles_mut(|s| {
        s.visuals = v.clone();
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 6.0);
        s.spacing.slider_width = 120.0;
    });
}
