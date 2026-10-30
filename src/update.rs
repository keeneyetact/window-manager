//! Checks GitHub for a newer release and installs it in place. The running
//! exe can't be overwritten but it can be renamed, so the new one takes its
//! name and the old one is deleted on the next start.

use eframe::egui;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/keeneyetact/window-manager/releases/latest";
const ASSET: &str = "windows-manager.exe";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, PartialEq)]
pub enum State {
    Idle,
    Available { version: String, url: String, page: String },
    Downloading { version: String },
    /// The new version is running; this one should quit.
    Restart,
    Failed(String),
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn parse(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn newer(tag: &str) -> bool {
    parse(tag) > parse(VERSION)
}

fn old_path(exe: &std::path::Path) -> PathBuf {
    exe.with_extension("exe.old")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).build()
}

fn check() -> Result<Option<State>, String> {
    let r: Release = agent()
        .get(LATEST)
        .set("User-Agent", "windows-manager")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    if r.draft || r.prerelease || !newer(&r.tag_name) {
        return Ok(None);
    }
    let Some(a) = r.assets.iter().find(|a| a.name == ASSET) else {
        return Ok(None);
    };
    Ok(Some(State::Available {
        version: r.tag_name.trim_start_matches('v').to_string(),
        url: a.browser_download_url.clone(),
        page: r.html_url,
    }))
}

/// Removes the exe left behind by the last update, and checks for updates
/// now and every few hours after.
pub fn spawn_checker(state: Arc<Mutex<State>>, ctx: egui::Context) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
    std::thread::spawn(move || loop {
        // Network errors are retried quietly on the next round
        if let Ok(Some(found)) = check() {
            let mut s = state.lock().unwrap();
            if matches!(*s, State::Idle | State::Failed(_)) {
                *s = found;
                ctx.request_repaint();
            }
        }
        std::thread::sleep(CHECK_EVERY);
    });
}

fn download_and_swap(url: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let new = exe.with_extension("exe.new");
    let resp = agent()
        .get(url)
        .set("User-Agent", "windows-manager")
        .call()
        .map_err(|e| e.to_string())?;
    let mut file = std::fs::File::create(&new).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp.into_reader(), &mut file).map_err(|e| e.to_string())?;
    drop(file);
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e.to_string());
    }
    Ok(exe)
}

/// Downloads the new version in the background and starts it.
pub fn install(state: Arc<Mutex<State>>, ctx: egui::Context) {
    let (version, url) = match &*state.lock().unwrap() {
        State::Available { version, url, .. } => (version.clone(), url.clone()),
        _ => return,
    };
    *state.lock().unwrap() = State::Downloading { version };
    std::thread::spawn(move || {
        let r = download_and_swap(&url).and_then(|exe| std::process::Command::new(exe).spawn().map_err(|e| e.to_string()));
        *state.lock().unwrap() = match r {
            Ok(_) => State::Restart,
            Err(e) => State::Failed(format!("Update failed: {e}")),
        };
        ctx.request_repaint();
    });
}
