// SPDX-License-Identifier: AGPL-3.0-or-later
//! Album art loader: one worker thread, dedupe by URL, 192px thumbnails as GPU textures.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Images {
    tx: Sender<String>,
    rx: Receiver<(String, Option<egui::ColorImage>)>,
    tex: HashMap<String, Option<egui::TextureHandle>>,
    asked: HashSet<String>,
}

impl Default for Images {
    fn default() -> Self {
        Self::new()
    }
}

impl Images {
    pub fn new() -> Self {
        let (tx, job_rx) = channel::<String>();
        let (done_tx, rx) = channel::<(String, Option<egui::ColorImage>)>();
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(15))
                .build();
            while let Ok(url) = job_rx.recv() {
                let img = fetch(&agent, &url);
                if done_tx.send((url, img)).is_err() {
                    break;
                }
            }
        });
        Self {
            tx,
            rx,
            tex: HashMap::new(),
            asked: HashSet::new(),
        }
    }

    /// Returns a texture if it is already resident, otherwise queues the download.
    pub fn texture(&mut self, ctx: &egui::Context, url: &str) -> Option<egui::TextureHandle> {
        if let Some(t) = self.tex.get(url) {
            return t.clone();
        }
        if self.asked.insert(url.to_string()) {
            let _ = self.tx.send(url.to_string());
        }
        if self.tex.len() > 400 {
            self.tex.clear();
            self.asked.clear();
        }
        let _ = ctx;
        None
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((url, image)) = self.rx.try_recv() {
            let handle = image.map(|img| {
                ctx.load_texture(
                    format!("art:{}", &url[url.len().saturating_sub(24)..]),
                    img,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.tex.insert(url, handle);
        }
    }
}

fn fetch(agent: &ureq::Agent, url: &str) -> Option<egui::ColorImage> {
    let resp = agent.get(url).call().ok()?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(12 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let thumb = image::imageops::thumbnail(&img, 192, 192);
    let (w, h) = thumb.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        thumb.as_raw(),
    ))
}
