// No console window behind the app, except in debug builds where it shows panics.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The desktop app. This only connects the page in `ui/` to `psxvc-app`, which does
//! the work.

use psx_export::{ffmpeg, AudioCodec, ExportOptions, Preset};
use psx_mdec::Idct;
use psxvc_app::disc::{Disc, FrameSummary, StreamSummary};
use psxvc_app::queue::{export_work, JobView, Queue};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

struct App {
    discs: Mutex<Vec<Arc<Disc>>>,
    queue: Queue,
}

impl App {
    fn disc(&self, id: usize) -> Result<Arc<Disc>, String> {
        let discs = self.discs.lock().unwrap();
        discs.get(id).cloned().ok_or(format!("no disc {id}"))
    }
}

#[derive(Serialize)]
struct DiscView {
    id: usize,
    name: String,
    path: String,
    edc_errors: u32,
    streams: Vec<StreamSummary>,
}

fn disc_view(id: usize, disc: &Disc) -> DiscView {
    DiscView {
        id,
        name: disc.name(),
        path: disc.path.display().to_string(),
        edc_errors: disc.index.stats.edc_errors,
        streams: disc.streams(),
    }
}

#[derive(Clone, Serialize)]
struct ScanProgress {
    path: String,
    percent: u32,
}

#[tauri::command(async)]
fn open_disc(path: String, app: AppHandle, state: State<App>) -> Result<DiscView, String> {
    let path_ref = Path::new(&path);
    if let Some((id, disc)) = {
        let discs = state.discs.lock().unwrap();
        discs
            .iter()
            .enumerate()
            .find(|(_, d)| d.path == path_ref)
            .map(|(id, d)| (id, d.clone()))
    } {
        return Ok(disc_view(id, &disc));
    }

    let mut last_percent = u32::MAX;
    let disc = Disc::open(path_ref, |done, total| {
        let percent = (done as u64 * 100 / total.max(1) as u64) as u32;
        if percent != last_percent {
            last_percent = percent;
            let _ = app.emit(
                "scan-progress",
                ScanProgress {
                    path: path.clone(),
                    percent,
                },
            );
        }
    })
    .map_err(|e| format!("{e:#}"))?;

    let mut discs = state.discs.lock().unwrap();
    discs.push(Arc::new(disc));
    let id = discs.len() - 1;
    Ok(disc_view(id, &discs[id]))
}

#[tauri::command(async)]
fn stream_frames(
    disc: usize,
    stream: usize,
    state: State<App>,
) -> Result<Vec<FrameSummary>, String> {
    state
        .disc(disc)?
        .frames(stream)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum IdctChoice {
    Hq,
    Accurate,
}

impl From<IdctChoice> for Idct {
    fn from(c: IdctChoice) -> Idct {
        match c {
            IdctChoice::Hq => Idct::Hq,
            IdctChoice::Accurate => Idct::Accurate,
        }
    }
}

/// A decoded frame, as raw bytes because JSON would be far slower for a picture.
#[tauri::command(async)]
fn frame(
    disc: usize,
    stream: usize,
    position: usize,
    idct: IdctChoice,
    state: State<App>,
) -> Result<Response, String> {
    let image = state
        .disc(disc)?
        .frame_image(stream, position, idct.into())
        .map_err(|e| format!("{e:#}"))?;
    Ok(Response::new(image))
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PresetChoice {
    Archive,
    H264,
    Av1,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AudioChoice {
    Flac,
    Opus,
    Aac,
}

#[derive(Deserialize)]
struct Settings {
    preset: PresetChoice,
    idct: IdctChoice,
    /// `None` for the preset's own codec.
    audio: Option<AudioChoice>,
    cfr: bool,
    sar: (u32, u32),
}

impl Settings {
    fn options(&self) -> Result<ExportOptions, String> {
        if self.sar.0 == 0 || self.sar.1 == 0 {
            return Err("the pixel aspect can't have a zero in it".into());
        }
        Ok(ExportOptions {
            preset: match self.preset {
                PresetChoice::Archive => Preset::Archive,
                PresetChoice::H264 => Preset::H264,
                PresetChoice::Av1 => Preset::Av1,
            },
            idct: self.idct.into(),
            sar: self.sar,
            audio: self.audio.map(|a| match a {
                AudioChoice::Flac => AudioCodec::Flac,
                AudioChoice::Opus => AudioCodec::Opus,
                AudioChoice::Aac => AudioCodec::Aac,
            }),
            cfr: self.cfr,
            ffmpeg: ffmpeg::locate(None),
        })
    }
}

#[tauri::command]
fn enqueue(
    disc: usize,
    streams: Vec<usize>,
    settings: Settings,
    folder: PathBuf,
    state: State<App>,
) -> Result<(), String> {
    let options = settings.options()?;
    let disc = state.disc(disc)?;
    let preset = match settings.preset {
        PresetChoice::Archive => "archive",
        PresetChoice::H264 => "h264",
        PresetChoice::Av1 => "av1",
    };
    for stream in streams {
        let label = format!("{} / stream {stream} / {preset}", disc.name());
        let work = export_work(disc.clone(), stream, options.clone(), folder.clone());
        state.queue.push(label, work);
    }
    Ok(())
}

#[tauri::command]
fn jobs(state: State<App>) -> Vec<JobView> {
    state.queue.jobs()
}

#[tauri::command]
fn cancel_job(id: u64, state: State<App>) {
    state.queue.cancel(id);
}

#[tauri::command]
fn clear_finished(state: State<App>) {
    state.queue.clear_finished();
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(App {
                discs: Mutex::default(),
                queue: Queue::new(move |jobs| {
                    let _ = handle.emit("jobs", jobs);
                }),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Wait for a running export to stop, so its partial file gets cleaned up.
            // That can take a moment, so the window goes first.
            if let WindowEvent::CloseRequested { .. } = event {
                let _ = window.hide();
                window.state::<App>().queue.shutdown();
            }
        })
        .invoke_handler(tauri::generate_handler![
            open_disc,
            stream_frames,
            frame,
            enqueue,
            jobs,
            cancel_job,
            clear_finished
        ])
        .run(tauri::generate_context!())
        .expect("the app failed to start");
}
