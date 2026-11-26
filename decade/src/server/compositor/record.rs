use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use decade_channel::{Error, Format, Record, Recorded, Reply};
use decade_ui::Recording as Marks;
use serde_json::Value;
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Physical, Rectangle};

use crate::files;
use crate::record::{format_for, summary};
use crate::server::{mcp, scratch, workspace};

use super::State;
use super::screenshot::{Asker, crop};

/// The longest an mp4 recording runs.
const MP4_LIMIT: Duration = Duration::from_secs(5 * 60);

/// The longest a GIF recording runs, as ffmpeg holds every frame of a
/// GIF until it ends.
const GIF_LIMIT: Duration = Duration::from_secs(60);

const MP4_FPS: u32 = 30;
const GIF_FPS: u32 = 15;
const MAX_FPS: u32 = 60;
const QUALITY: u32 = 23;

/// The highest crf libx264 takes.
const WORST_QUALITY: u32 = 51;
const MAX_WIDTH: u32 = 7680;

/// ffmpeg's memory for a GIF besides its frames, and the bytes it holds
/// for every two pixels of each frame.
//
// fitted to ffmpeg 9's peak RSS for the GIF arguments below:
// 656x828 at 75, 150 and 300 frames, 400x505 scaled at 150
// and 300, and 1328x828 at 60 frames from decade itself, all
// within 10% of 90 MiB + 4.5 bytes a pixel of each frame
const GIF_BASE: u64 = 90 << 20;
const GIF_TWO_PIXELS: u64 = 9;

/// The frames waiting for ffmpeg; a frame that finds the queue full is
/// dropped.
const QUEUE: usize = 2;

/// The lines of ffmpeg's stderr an error carries.
const STDERR_LINES: usize = 20;

/// The glyph of the recording tag, `md-record_rec` in the Nerd Font.
const ICON: char = '\u{f044b}';

/// How long the recording icon stays lit, and then dark.
const BLINK: Duration = Duration::from_millis(500);

/// The pointer drawn into the frames at scale 1, its tip at the top left:
/// `#` is its black edge, `.` its white fill.
pub(super) const ARROW: [&str; 17] = [
    "#",
    "##",
    "#.#",
    "#..#",
    "#...#",
    "#....#",
    "#.....#",
    "#......#",
    "#.......#",
    "#........#",
    "#.....#####",
    "#..#..#",
    "#.# #..#",
    "##  #..#",
    "#    #..#",
    "     #..#",
    "      ##",
];

/// Who asked for a recording.
pub(super) enum Requester {
    /// An MCP `record` call, answered once the recording starts.
    Mcp(mcp::Reply),

    /// Socket client `client`, answered once the file is written;
    /// `message` is its line, for the error reply.
    Socket { client: usize, message: Value },
}

/// The longest a recording in `format` runs.
pub(super) fn limit(format: Format) -> Duration {
    match format {
        Format::Mp4 => MP4_LIMIT,
        Format::Gif => GIF_LIMIT,
    }
}

/// A recording's settings, checked and with their defaults filled in.
#[derive(Debug, PartialEq)]
pub(super) struct Plan {
    pub file: PathBuf,
    pub format: Format,
    pub fps: u32,

    /// How long to record, at most the format's limit, or `None` to
    /// record until stopped or the limit has passed.
    pub duration: Option<Duration>,
    pub width: Option<u32>,
    pub quality: u32,
    pub pointer: bool,

    /// Whether a GIF plays in a loop.
    pub forever: bool,
}

impl Plan {
    /// The settings `record` asks for, or why decade refuses them.
    pub(super) fn new(record: &Record) -> Result<Plan, String> {
        let file = PathBuf::from(&record.file);
        if !file.is_absolute() {
            return Err(format!("{} is not an absolute path", record.file));
        }
        let format = format_for(&file, record.format)?;
        let mp4 = format == Format::Mp4;
        if !mp4 && record.quality.is_some() {
            return Err("quality is for an mp4, not a gif".to_string());
        }
        if mp4 && record.r#loop.is_some() {
            return Err("loop is for a gif, not an mp4".to_string());
        }
        let fps = record.fps.unwrap_or(if mp4 { MP4_FPS } else { GIF_FPS });
        if !(1..=MAX_FPS).contains(&fps) {
            return Err(format!("fps is 1 to {MAX_FPS}, not {fps}"));
        }
        let duration = match record.duration {
            Some(seconds) if seconds > 0.0 && seconds.is_finite() => {
                Some(Duration::from_secs_f64(seconds).min(limit(format)))
            }
            Some(seconds) => {
                return Err(format!(
                    "duration is a number of seconds above 0, not {seconds}"
                ));
            }
            None => None,
        };
        if let Some(width) = record
            .width
            .filter(|width| !(2..=MAX_WIDTH).contains(width))
        {
            return Err(format!("width is 2 to {MAX_WIDTH} pixels, not {width}"));
        }
        let quality = record.quality.unwrap_or(QUALITY);
        if quality > WORST_QUALITY {
            return Err(format!("quality is 0 to {WORST_QUALITY}, not {quality}"));
        }
        Ok(Plan {
            file,
            format,
            fps,
            duration,
            width: record.width,
            quality,
            pointer: record.pointer.unwrap_or(true),
            forever: record.r#loop.unwrap_or(true),
        })
    }

    /// How long the recording runs unless something ends it sooner.
    fn longest(&self) -> Duration {
        self.duration.unwrap_or(limit(self.format))
    }

    /// The frames in a recording that runs for `longest`.
    fn frames(&self) -> u64 {
        (self.longest().as_secs_f64() * self.fps as f64).ceil() as u64
    }

    /// The size of the video for frames `size` pixels in size.
    fn output_size(&self, size: (i32, i32)) -> (u32, u32) {
        let (width, height) = (size.0 as u32, size.1 as u32);
        let Some(scaled) = self.width else {
            return match self.format {
                Format::Mp4 => (width / 2 * 2, height / 2 * 2),
                Format::Gif => (width, height),
            };
        };
        // the height ffmpeg's scale gives for -1, and for -2 the
        // nearest even one
        let ratio = height as f64 / width as f64;
        match self.format {
            Format::Mp4 => {
                let scaled = scaled / 2 * 2;
                let height = (scaled as f64 * ratio / 2.0).round() as u32 * 2;
                (scaled, height.max(2))
            }
            Format::Gif => (scaled, ((scaled as f64 * ratio).round() as u32).max(1)),
        }
    }

    /// ffmpeg's peak memory in bytes for frames `size` pixels in size, or
    /// `None` for an mp4, whose encoder holds a fixed number of frames.
    fn peak_memory(&self, size: (i32, i32)) -> Option<u64> {
        if self.format == Format::Mp4 {
            return None;
        }
        let (width, height) = self.output_size(size);
        let pixels = self.frames() * width as u64 * height as u64;
        Some(GIF_BASE + pixels * GIF_TWO_PIXELS / 2)
    }
}

/// The `MemAvailable` of `/proc/meminfo`'s text `meminfo`, in bytes.
fn available(meminfo: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?;
    let kib: u64 = line.trim().strip_suffix("kB")?.trim().parse().ok()?;
    Some(kib * 1024)
}

/// A recording, from its first frame until ffmpeg has written its file.
pub(super) struct Recording {
    id: u64,
    workspace: u32,

    /// The tab and id of the pane, which keep the recording on the pane
    /// through a rename.
    tab: usize,
    pane_id: usize,

    /// The pane's name when the recording started.
    pane: String,
    plan: Plan,

    /// The socket client waiting for the file, and its line.
    client: Option<(usize, Value)>,

    /// The MCP `record.stop` calls waiting for the file.
    stops: Vec<mcp::Reply>,
    started: Instant,

    /// The size of every frame, the pane's client area when it started.
    size: (i32, i32),

    /// The queue to the thread that feeds ffmpeg, until the recording
    /// stops.
    frames: Option<SyncSender<Vec<u8>>>,
    sent: u64,

    /// The frames left out because the queue to ffmpeg was full.
    dropped: u64,
    timer: Option<RegistrationToken>,

    /// When the next frame is due.
    next: Instant,

    /// Whether a viewer showed the pane's workspace at the last frame.
    shown: bool,

    /// What ended the recording, and how many seconds it ran.
    ended: Option<(String, f64)>,
}

impl Recording {
    fn running(&self) -> bool {
        self.frames.is_some()
    }

    fn is(&self, workspace: u32, (tab, pane_id): (usize, usize)) -> bool {
        (self.workspace, self.tab, self.pane_id) == (workspace, tab, pane_id)
    }
}

/// The result of a pane's last recording, kept until the next recording
/// of the pane starts.
pub(super) struct Finished {
    workspace: u32,
    tab: usize,
    pane_id: usize,

    /// The pane's name, which finds the result once the pane has gone.
    pane: String,
    result: Result<Recorded, String>,
}

impl State {
    /// Record pane `pane` by `record`, at once when the pane accepts
    /// captures or else once the person allows it.
    pub(super) fn record(
        &mut self,
        workspace: usize,
        record: &Record,
        caller: Option<&str>,
        to: Requester,
    ) {
        match self.record_checks(workspace, record) {
            Ok((name, plan, true)) => self.start_recording(workspace, name, to, plan),
            Ok((name, plan, false)) => {
                self.ask(workspace, name, caller, Asker::Record { to, plan })
            }
            Err(reason) => self.answer_record(to, Err(reason)),
        }
    }

    /// The pane's own name, the plan, and whether the pane accepts
    /// captures, or why decade refuses the recording.
    fn record_checks(
        &self,
        workspace: usize,
        record: &Record,
    ) -> Result<(String, Plan, bool), String> {
        let plan = Plan::new(record)?;
        let (name, info) = self.screenshot_target(workspace, &record.pane)?;
        let accepted = info.captures;
        self.check_free(workspace, &name, &plan)?;
        let (_, rect) = self.client_px(workspace, &name)?;
        let size = (rect.size.w, rect.size.h);
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        if let (Some(needed), Some(available)) = (plan.peak_memory(size), available(&meminfo))
            && needed > available / 2
        {
            let (width, height) = plan.output_size(size);
            return Err(format!(
                "a GIF of {} frames at {width}x{height} needs about {} MiB in ffmpeg, more than half of the {} MiB available",
                plan.frames(),
                needed >> 20,
                available >> 20
            ));
        }
        Ok((name, plan, accepted))
    }

    /// Refuse a recording of pane `pane` into `plan`'s file while the pane
    /// records, without ffmpeg, or when the file cannot be new.
    fn check_free(&self, workspace: usize, pane: &str, plan: &Plan) -> Result<(), String> {
        if self.recording_of(workspace, pane).is_some() {
            return Err(format!("pane {pane} is already being recorded"));
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        scratch::resolve_program("ffmpeg", &path, &self.workspaces[workspace].cwd)?;
        files::check_new(&plan.file)
    }

    /// The index of the recording of the pane that `pane` names, now or
    /// formerly, in workspace `workspace`, until ffmpeg has written it.
    fn recording_of(&self, workspace: usize, pane: &str) -> Option<usize> {
        let shown = &self.workspaces[workspace];
        let target = shown.by_name(pane)?;
        (self.recordings.iter()).position(|recording| recording.is(shown.id, target))
    }

    /// The last finished recording of the pane that `pane` names, or named
    /// when the recording ended.
    fn finished_of(&self, workspace: usize, pane: &str) -> Option<&Finished> {
        let shown = &self.workspaces[workspace];
        let target = shown.by_name(pane);
        self.finished.iter().find(|finished| match target {
            Some(target) => {
                (finished.workspace, finished.tab, finished.pane_id)
                    == (shown.id, target.0, target.1)
            }
            None => finished.workspace == shown.id && finished.pane == pane,
        })
    }

    /// Start ffmpeg and the frames for a recording of pane `pane`.
    pub(super) fn start_recording(
        &mut self,
        workspace: usize,
        pane: String,
        to: Requester,
        plan: Plan,
    ) {
        let started = (self.check_free(workspace, &pane, &plan))
            .and_then(|()| self.client_px(workspace, &pane))
            .and_then(|(_, rect)| {
                let size = (rect.size.w, rect.size.h);
                Ok((size, spawn_ffmpeg(&plan, size)?))
            });
        let (size, ffmpeg) = match started {
            Ok(started) => started,
            Err(reason) => {
                self.answer_record(to, Err(reason));
                return;
            }
        };
        let shown = &self.workspaces[workspace];
        let Some((tab, pane_id)) = shown.by_name(&pane) else {
            return;
        };
        let workspace_id = shown.id;
        self.finished.retain(|finished| {
            (finished.workspace, finished.tab, finished.pane_id) != (workspace_id, tab, pane_id)
        });
        let id = self.next_recording;
        self.next_recording += 1;
        let (frames, queued) = mpsc::sync_channel(QUEUE);
        let done = self.recorded.clone();
        std::thread::spawn(move || {
            let _ = done.send((id, feed(ffmpeg, queued)));
        });
        let now = Instant::now();
        let timer = self
            .loop_handle
            .insert_source(Timer::immediate(), move |_, _, state| state.record_tick(id));
        let client = match to {
            Requester::Mcp(reply) => {
                let _ = reply.send(Ok(mcp::Answer::Text(running(&pane, &plan, size))));
                None
            }
            Requester::Socket { client, message } => Some((client, message)),
        };
        let shown = self.on_screen(workspace_id);
        self.recordings.push(Recording {
            id,
            workspace: workspace_id,
            tab,
            pane_id,
            pane,
            plan,
            client,
            stops: Vec::new(),
            started: now,
            size,
            frames: Some(frames),
            sent: 0,
            dropped: 0,
            timer: timer.ok(),
            next: now,
            shown,
            ended: None,
        });
        self.start_pulse();
        self.request_redraw();
    }

    /// Whether a viewer shows workspace `id`.
    fn on_screen(&self, id: u32) -> bool {
        (self.viewers.values()).any(|viewer| viewer.welcomed && viewer.workspace == id)
    }

    /// Hand ffmpeg the next frame of recording `id`, or stop it when
    /// something ended it.
    fn record_tick(&mut self, id: u64) -> TimeoutAction {
        let Some(index) = self.recordings.iter().position(|r| r.id == id) else {
            return TimeoutAction::Drop;
        };
        if let Some(ended) = self.recording_ends(index) {
            self.recordings[index].timer = None;
            self.stop_recording(index, ended);
            return TimeoutAction::Drop;
        }

        // a frame that fails to render is left out, and
        // ffmpeg repeats the one before it in its place
        let frame = self.record_frame(index);
        let recording = &mut self.recordings[index];
        if let (Ok(frame), Some(frames)) = (frame, &recording.frames) {
            match frames.try_send(frame) {
                Ok(()) => recording.sent += 1,
                Err(TrySendError::Full(_)) => recording.dropped += 1,

                // the feeding thread has exited, and
                // recording_written answers once its result arrives
                Err(TrySendError::Disconnected(_)) => {
                    recording.timer = None;
                    return TimeoutAction::Drop;
                }
            }
        }
        let period = Duration::from_secs(1) / recording.plan.fps;
        recording.next += period;

        // a frame that took longer than a period starts the
        // schedule again from now, so slow frames cannot keep
        // the event loop busy catching up
        let now = Instant::now();
        if recording.next < now {
            recording.next = now + period;
        }
        TimeoutAction::ToInstant(recording.next)
    }

    /// What ends recording `index` now, if anything.
    fn recording_ends(&mut self, index: usize) -> Option<String> {
        let recording = &self.recordings[index];
        if let Some((client, _)) = recording.client
            && !self.sockets.contains_key(&client)
        {
            return Some("the requester went away".to_string());
        }
        let Some(workspace) = self.workspace_index(recording.workspace) else {
            return Some(format!("workspace {} closed", recording.workspace));
        };
        let shown = &self.workspaces[workspace];
        if shown.asleep.is_some() {
            return Some(format!("workspace {} went to sleep", shown.name));
        }
        let pane = (shown.tabs.get(recording.tab))
            .and_then(|tab| tab.layout.pane(recording.pane_id))
            .map(|pane| pane.name.as_str());
        let Some(pane) = pane else {
            return Some(format!("pane {} closed", recording.pane));
        };
        if recording.tab != shown.active_tab {
            return Some(format!("the tab of pane {pane} left the screen"));
        }
        let on_screen = self.on_screen(recording.workspace);
        if recording.shown && !on_screen {
            return Some(format!("workspace {} left the screen", shown.name));
        }
        let elapsed = recording.started.elapsed();
        let ended = match recording.plan.duration {
            Some(duration) if elapsed >= duration => Some("it ran for its duration".to_string()),
            None if elapsed >= limit(recording.plan.format) => Some(format!(
                "it reached the limit of {}",
                span(limit(recording.plan.format))
            )),
            _ => None,
        };
        self.recordings[index].shown = on_screen;
        ended
    }

    /// The frame of recording `index`: its pane rendered as a screenshot
    /// would be, with the pointer drawn in, at the recording's size.
    fn record_frame(&mut self, index: usize) -> Result<Vec<u8>, String> {
        let recording = &self.recordings[index];
        let (tab, pane_id, size) = (recording.tab, recording.pane_id, recording.size);
        let pointer = recording.plan.pointer;
        let workspace = (self.workspace_index(recording.workspace))
            .ok_or_else(|| format!("workspace {} is closed", recording.workspace))?;
        let rect = self
            .pane_px(workspace, tab, pane_id)
            .ok_or("the pane has no room to draw")?;
        self.send_frames_to(workspace, tab, pane_id);
        let rgba = self.render_plain(workspace)?;
        let mut frame = crop(&rgba, self.size.w, rect);
        let (width, height) = (rect.size.w as usize, rect.size.h as usize);
        let shows = (self.active.and_then(|id| self.viewers.get(&id)))
            .is_some_and(|viewer| viewer.workspace == self.workspaces[workspace].id);
        if pointer && shows {
            let at = self.pointer.to_physical(self.scale).to_i32_round::<i32>();
            let tip = (at.x - rect.loc.x, at.y - rect.loc.y);
            let scale = self.scale.round().max(1.0) as usize;
            draw_pointer(&mut frame, (width, height), tip, scale);
        }
        if (rect.size.w, rect.size.h) != size {
            frame = place(&frame, (width, height), (size.0 as usize, size.1 as usize));
        }
        Ok(frame)
    }

    /// The client area of pane `id` of tab `tab` in pixels on screen,
    /// while it has room to draw.
    fn pane_px(&self, workspace: usize, tab: usize, id: usize) -> Option<Rectangle<i32, Physical>> {
        let pane = self.workspaces[workspace].tabs.get(tab)?.layout.pane(id)?;
        let rect = self.text.px_rect(pane.client_rect());
        rect.intersection(Rectangle::from_size(self.size))
            .filter(|rect| !rect.is_empty())
    }

    /// Let the windows of pane `id` of tab `tab` draw their next frame,
    /// which no viewer asks for while none shows the workspace.
    fn send_frames_to(&self, workspace: usize, tab: usize, id: usize) {
        let Some(toplevel) =
            (self.workspaces[workspace].tabs.get(tab)).and_then(|tab| tab.toplevel(id))
        else {
            return;
        };
        let surface = toplevel.wl_surface();
        let time = self.now();
        super::send_frames_surface_tree(surface, time);
        for (popup, _) in super::popups_over(surface) {
            super::send_frames_surface_tree(popup.wl_surface(), time);
        }
    }

    /// End the recording of the pane `pane` names, and hand `waiting` the
    /// finished result once ffmpeg has written the file, or the reason
    /// there is none.
    pub(super) fn stop_record(
        &mut self,
        workspace: usize,
        pane: &str,
        waiting: Option<mcp::Reply>,
    ) -> Result<(), String> {
        let Some(index) = self.recording_of(workspace, pane) else {
            let finished =
                (self.finished_of(workspace, pane)).map(|finished| finished.result.clone());
            return match (finished, waiting) {
                (Some(result), Some(waiting)) => {
                    let _ = waiting.send(finished_text(pane, result));
                    Ok(())
                }
                (_, waiting) => {
                    let reason = format!("pane {pane} is not being recorded");
                    if let Some(waiting) = waiting {
                        let _ = waiting.send(Err(reason.clone()));
                    }
                    Err(reason)
                }
            };
        };
        self.recordings[index].stops.extend(waiting);
        if self.recordings[index].running() {
            self.stop_recording(index, "a stop call ended it".to_string());
        }
        Ok(())
    }

    /// What the recording of the pane `pane` names has done so far, or
    /// its finished result.
    pub(super) fn record_status(&self, workspace: usize, pane: &str) -> Result<String, String> {
        if let Some(index) = self.recording_of(workspace, pane) {
            let recording = &self.recordings[index];
            let Some((ended, _)) = &recording.ended else {
                return Ok(format!(
                    "recording pane {pane} into {} for {}: {} frames, {} dropped",
                    recording.plan.file.display(),
                    clock(recording.started.elapsed()),
                    recording.sent,
                    recording.dropped
                ));
            };
            return Ok(format!(
                "the recording of pane {pane} ended, as {ended}, and ffmpeg is finishing {}",
                recording.plan.file.display()
            ));
        }
        let finished = self
            .finished_of(workspace, pane)
            .ok_or_else(|| format!("pane {pane} has not been recorded"))?;
        match finished_text(pane, finished.result.clone()) {
            Ok(mcp::Answer::Text(text)) => Ok(text),
            Ok(_) => unreachable!("a finished recording answers with text"),
            Err(reason) => Err(reason),
        }
    }

    /// Stop handing frames to recording `index` for the reason `ended`,
    /// and let ffmpeg finish the file.
    fn stop_recording(&mut self, index: usize, ended: String) {
        let recording = &mut self.recordings[index];
        if let Some(timer) = recording.timer.take() {
            self.loop_handle.remove(timer);
        }
        recording.frames = None;
        let seconds = recording.started.elapsed().as_secs_f64();
        recording.ended.get_or_insert((ended, seconds));
        self.request_redraw();
    }

    /// Answer for recording `id`, whose ffmpeg has exited with `result`,
    /// and keep the answer for `record.status`.
    pub(super) fn recording_written(&mut self, id: u64, result: Result<(), String>) {
        let Some(index) = self.recordings.iter().position(|r| r.id == id) else {
            return;
        };
        if self.recordings[index].running() {
            self.stop_recording(index, "ffmpeg exited".to_string());
        }
        let recording = self.recordings.remove(index);
        let (ended, seconds) = recording.ended.clone().unwrap_or_default();
        let file = recording.plan.file.display().to_string();
        let result = outcome(
            &recording.pane,
            &file,
            recording.sent,
            recording.dropped,
            seconds,
            ended,
            result,
        );
        if recording.sent == 0 {
            let _ = std::fs::remove_file(&recording.plan.file);
        }
        if let Some((client, message)) = recording.client {
            let to = Requester::Socket { client, message };
            self.answer_record(to, result.clone());
        }
        for stop in recording.stops {
            let _ = stop.send(finished_text(&recording.pane, result.clone()));
        }
        self.finished.push(Finished {
            workspace: recording.workspace,
            tab: recording.tab,
            pane_id: recording.pane_id,
            pane: recording.pane,
            result,
        });
    }

    /// Send `to` the finished recording, or the reason there is none.
    pub(super) fn answer_record(&mut self, to: Requester, answer: Result<Recorded, String>) {
        match to {
            Requester::Mcp(reply) => {
                let _ = reply.send(answer.map(|recorded| mcp::Answer::Text(summary(&recorded))));
            }
            Requester::Socket { client, message } => {
                let reply = match answer {
                    Ok(recorded) => Reply::Record(recorded),
                    Err(reason) => Reply::Error(Error { reason, message }),
                };
                self.channel_send(client, &serde_json::to_string(&reply).unwrap());
            }
        }
    }

    /// Whether a recording runs, which keeps the pulse timer drawing.
    pub(super) fn recording(&self) -> bool {
        self.recordings.iter().any(Recording::running)
    }

    /// The marks of the running recordings of workspace `workspace`'s
    /// shown tab at `now`, with each one's elapsed time in `times`.
    pub(super) fn recording_marks<'a>(
        &self,
        workspace: usize,
        now: Instant,
        times: &'a mut Vec<String>,
    ) -> Vec<Marks<'a>> {
        let shown = &self.workspaces[workspace];
        let running = (self.recordings.iter()).filter(|recording| {
            recording.running()
                && recording.workspace == shown.id
                && recording.tab == shown.active_tab
        });
        let mut panes = Vec::new();
        for recording in running {
            let elapsed = now.saturating_duration_since(recording.started);
            times.push(clock(elapsed));
            panes.push((recording.pane_id, elapsed));
        }
        (panes.into_iter().zip(times.iter()))
            .map(|((pane, elapsed), time)| Marks {
                pane,
                strength: workspace::pulse(elapsed),
                icon: ICON,
                lit: (elapsed.as_millis() / BLINK.as_millis()).is_multiple_of(2),
                elapsed: time,
            })
            .collect()
    }
}

/// The answer to a recording of pane `pane` into `file` whose ffmpeg
/// exited with `result`, after `sent` frames went to ffmpeg.
fn outcome(
    pane: &str,
    file: &str,
    sent: u64,
    dropped: u64,
    seconds: f64,
    ended: String,
    result: Result<(), String>,
) -> Result<Recorded, String> {
    if sent == 0 {
        return Err(format!(
            "the recording of pane {pane} ended before its first frame, as {ended}, so decade wrote no file"
        ));
    }
    result.map(|()| Recorded {
        file: file.to_string(),
        frames: sent,
        dropped,
        seconds: (seconds * 10.0).round() / 10.0,
        ended,
    })
}

/// The MCP answer for the finished recording of pane `pane`.
fn finished_text(pane: &str, result: Result<Recorded, String>) -> Result<mcp::Answer, String> {
    match result {
        Ok(recorded) => Ok(mcp::Answer::Text(summary(&recorded))),
        Err(reason) => Err(format!("the recording of pane {pane} failed: {reason}")),
    }
}

/// The MCP answer to a `record` whose recording has started.
fn running(pane: &str, plan: &Plan, size: (i32, i32)) -> String {
    let (width, height) = plan.output_size(size);
    let memory = match plan.peak_memory(size) {
        Some(bytes) => format!("ffmpeg needs about {} MiB", bytes >> 20),
        None => "ffmpeg's memory does not grow with its length".to_string(),
    };
    format!(
        "recording pane {pane} into {}: {width}x{height} at {} fps, at most {} frames over {}; {memory}. record.status reports on it and record.stop ends it",
        plan.file.display(),
        plan.fps,
        plan.frames(),
        span(plan.longest()),
    )
}

/// `duration` in words, as whole minutes when it is some.
pub(super) fn span(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    match duration.as_secs() {
        60 => "1 minute".to_string(),
        whole if whole as f64 == seconds && whole % 60 == 0 => format!("{} minutes", whole / 60),
        _ => format!("{seconds} seconds"),
    }
}

/// `elapsed` as minutes and seconds, `m:ss`.
fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Create `plan`'s file, so that no other file can take its path, and
/// start ffmpeg writing frames `size` pixels in size into it.
fn spawn_ffmpeg(plan: &Plan, size: (i32, i32)) -> Result<Child, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&plan.file)
        .map_err(|error| format!("{}: {error}", plan.file.display()))?;
    Command::new("ffmpeg")
        .args(ffmpeg_args(plan, size))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            let _ = std::fs::remove_file(&plan.file);
            format!("starting ffmpeg failed: {error}")
        })
}

/// The arguments that make ffmpeg read RGBA frames `size` pixels in size
/// on its stdin and write them to `plan`'s file.
pub(super) fn ffmpeg_args(plan: &Plan, size: (i32, i32)) -> Vec<String> {
    let (width, height) = size;
    let fps = plan.fps.to_string();
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        // rgb0 rather than rgba, so ffmpeg ignores alpha:
        // paletteuse turns a pixel with low alpha clear
        "-pix_fmt",
        "rgb0",
        "-s",
        &format!("{width}x{height}"),
        // each frame is stamped when ffmpeg reads it, and cfr
        // repeats or drops frames to the output rate, so a frame
        // left out or late keeps the video as long as the
        // recording ran
        "-use_wallclock_as_timestamps",
        "1",
        "-i",
        "-",
        "-fps_mode",
        "cfr",
        "-r",
        &fps,
    ]
    .map(str::to_string)
    .into();
    let filter = match (plan.format, plan.width) {
        // yuv420p, which players expect, needs an even width
        // and height
        (Format::Mp4, None) => "crop=trunc(iw/2)*2:trunc(ih/2)*2:0:0".to_string(),
        (Format::Mp4, Some(scaled)) => format!("scale={}:-2:flags=lanczos", scaled / 2 * 2),

        // the palette is made from every frame, so ffmpeg holds
        // them all until the input ends
        (Format::Gif, scaled) => {
            let scale = scaled.map_or(String::new(), |scaled| {
                format!("scale={scaled}:-1:flags=lanczos,")
            });
            format!("{scale}split[a][b];[a]palettegen[p];[b][p]paletteuse=diff_mode=rectangle")
        }
    };
    args.extend(["-vf".to_string(), filter]);
    let quality = plan.quality.to_string();
    let codec = match plan.format {
        Format::Mp4 => vec![
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            &quality,
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ],
        Format::Gif => vec!["-loop", if plan.forever { "0" } else { "-1" }],
    };
    args.extend(codec.into_iter().map(str::to_string));

    // decade created the file, empty, so that nothing else
    // takes the path, and ffmpeg replaces it
    args.extend(["-y".to_string(), plan.file.display().to_string()]);
    args
}

/// Write each frame from `frames` to ffmpeg's stdin until the queue
/// closes or ffmpeg stops reading, then close stdin and wait for ffmpeg
/// to finish the file.
fn feed(mut ffmpeg: Child, frames: Receiver<Vec<u8>>) -> Result<(), String> {
    let stderr = ffmpeg.stderr.take().map(last_lines);
    let mut stdin = ffmpeg.stdin.take();
    for frame in frames {
        let Some(pipe) = &mut stdin else { break };
        if pipe.write_all(&frame).is_err() {
            break;
        }
    }
    drop(stdin);
    let status = ffmpeg
        .wait()
        .map_err(|error| format!("waiting for ffmpeg failed: {error}"))?;
    let lines = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    Err(format!("ffmpeg failed, {status}:\n{}", lines.join("\n")))
}

/// A thread that reads `stderr` to its end and returns its last lines.
fn last_lines(stderr: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut lines = VecDeque::with_capacity(STDERR_LINES);
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.len() == STDERR_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
        lines.into()
    })
}

/// Draw the arrow pointer into an RGBA frame `size` pixels in size when
/// its tip `tip` is inside the frame, each of its pixels `scale` pixels
/// square. The parts past the frame's edge are left out.
fn draw_pointer(frame: &mut [u8], size: (usize, usize), tip: (i32, i32), scale: usize) {
    let (width, height) = (size.0 as i32, size.1 as i32);
    if !(0..width).contains(&tip.0) || !(0..height).contains(&tip.1) {
        return;
    }
    for (row, line) in ARROW.iter().enumerate() {
        for (column, mark) in line.bytes().enumerate() {
            let color = match mark {
                b'#' => [0, 0, 0, 255],
                b'.' => [255, 255, 255, 255],
                _ => continue,
            };
            for dy in 0..scale {
                for dx in 0..scale {
                    let x = tip.0 + (column * scale + dx) as i32;
                    let y = tip.1 + (row * scale + dy) as i32;
                    if x < width && y < height {
                        let at = (y * width + x) as usize * 4;
                        frame[at..at + 4].copy_from_slice(&color);
                    }
                }
            }
        }
    }
}

/// An RGBA frame `from` pixels in size placed at the top left of a black
/// frame `to` pixels in size, its rows and columns past `to` cut off.
fn place(frame: &[u8], from: (usize, usize), to: (usize, usize)) -> Vec<u8> {
    let mut placed = [0, 0, 0, 255].repeat(to.0 * to.1);
    let columns = from.0.min(to.0) * 4;
    for (y, row) in frame.chunks_exact(from.0 * 4).take(to.1).enumerate() {
        let start = y * to.0 * 4;
        placed[start..start + columns].copy_from_slice(&row[..columns]);
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(record: Record) -> Result<Plan, String> {
        Plan::new(&record)
    }

    fn file(file: &str) -> Record {
        Record {
            pane: "code".to_string(),
            file: file.to_string(),
            ..Record::default()
        }
    }

    #[test]
    fn a_plan_takes_its_format_from_the_extension_and_the_formats_defaults() {
        let mp4 = plan(file("/work/demo.mp4")).unwrap();
        assert_eq!(
            mp4,
            Plan {
                file: PathBuf::from("/work/demo.mp4"),
                format: Format::Mp4,
                fps: 30,
                duration: None,
                width: None,
                quality: 23,
                pointer: true,
                forever: true,
            }
        );
        let gif = plan(file("/work/demo.gif")).unwrap();
        assert_eq!((gif.format, gif.fps), (Format::Gif, 15));
    }

    #[test]
    fn a_plan_refuses_a_relative_path_settings_for_the_other_format_and_values_out_of_range() {
        let refused = |record: Record| plan(record).unwrap_err();
        assert_eq!(
            refused(file("demo.gif")),
            "demo.gif is not an absolute path"
        );
        assert_eq!(
            refused(Record {
                format: Some(Format::Mp4),
                ..file("/work/demo.gif")
            }),
            "/work/demo.gif does not end in .mp4, as format mp4 needs"
        );
        assert_eq!(
            refused(Record {
                quality: Some(20),
                ..file("/work/demo.gif")
            }),
            "quality is for an mp4, not a gif"
        );
        assert_eq!(
            refused(Record {
                r#loop: Some(false),
                ..file("/work/demo.mp4")
            }),
            "loop is for a gif, not an mp4"
        );
        assert_eq!(
            refused(Record {
                fps: Some(0),
                ..file("/work/demo.mp4")
            }),
            "fps is 1 to 60, not 0"
        );
        assert_eq!(
            refused(Record {
                duration: Some(-1.0),
                ..file("/work/demo.mp4")
            }),
            "duration is a number of seconds above 0, not -1"
        );
        assert_eq!(
            refused(Record {
                width: Some(1),
                ..file("/work/demo.mp4")
            }),
            "width is 2 to 7680 pixels, not 1"
        );
        assert_eq!(
            refused(Record {
                quality: Some(52),
                ..file("/work/demo.mp4")
            }),
            "quality is 0 to 51, not 52"
        );
    }

    #[test]
    fn a_duration_past_the_limit_is_cut_to_five_minutes_for_an_mp4_and_one_for_a_gif() {
        let long = |name: &str| {
            plan(Record {
                duration: Some(600.0),
                ..file(name)
            })
            .unwrap()
        };
        assert_eq!(long("/work/demo.mp4").duration, Some(MP4_LIMIT));
        assert_eq!(long("/work/demo.gif").duration, Some(GIF_LIMIT));
        let short = plan(Record {
            duration: Some(2.5),
            ..file("/work/demo.gif")
        });
        assert_eq!(short.unwrap().duration, Some(Duration::from_millis(2500)));
        assert_eq!(plan(file("/work/demo.gif")).unwrap().longest(), GIF_LIMIT);
    }

    #[test]
    fn an_mp4_at_its_own_width_is_cropped_to_even_sides_and_one_with_a_width_is_scaled() {
        let mp4 = plan(file("/work/demo.mp4")).unwrap();
        assert_eq!(
            ffmpeg_args(&mp4, (657, 829)).join(" "),
            "-hide_banner -loglevel error -f rawvideo -pix_fmt rgb0 -s 657x829 \
             -use_wallclock_as_timestamps 1 -i - -fps_mode cfr -r 30 \
             -vf crop=trunc(iw/2)*2:trunc(ih/2)*2:0:0 -c:v libx264 -preset medium -crf 23 \
             -pix_fmt yuv420p -movflags +faststart -y /work/demo.mp4"
        );
        assert_eq!(mp4.output_size((657, 829)), (656, 828));
        let scaled = Plan {
            width: Some(641),
            quality: 18,
            ..mp4
        };
        let args = ffmpeg_args(&scaled, (657, 829)).join(" ");
        assert!(
            args.contains("-vf scale=640:-2:flags=lanczos -c:v libx264 -preset medium -crf 18")
        );
        assert_eq!(scaled.output_size((657, 829)), (640, 808));
    }

    #[test]
    fn a_gif_makes_its_palette_from_its_frames_and_loops_unless_told_not_to() {
        let gif = plan(file("/work/demo.gif")).unwrap();
        assert_eq!(
            ffmpeg_args(&gif, (656, 828)).join(" "),
            "-hide_banner -loglevel error -f rawvideo -pix_fmt rgb0 -s 656x828 \
             -use_wallclock_as_timestamps 1 -i - -fps_mode cfr -r 15 \
             -vf split[a][b];[a]palettegen[p];[b][p]paletteuse=diff_mode=rectangle \
             -loop 0 -y /work/demo.gif"
        );
        let once = Plan {
            forever: false,
            width: Some(400),
            ..gif
        };
        let args = ffmpeg_args(&once, (656, 828));
        let at = args.iter().position(|arg| arg == "-loop").unwrap();
        assert_eq!(args[at + 1], "-1");
        assert!(
            args.join(" ")
                .contains("-vf scale=400:-1:flags=lanczos,split[a][b]")
        );
        assert_eq!(once.output_size((656, 828)), (400, 505));
    }

    #[test]
    fn a_gifs_memory_grows_with_its_frames_and_pixels_and_an_mp4_has_no_estimate() {
        let gif = Plan {
            duration: Some(Duration::from_secs(10)),
            ..plan(file("/work/demo.gif")).unwrap()
        };
        assert_eq!(gif.frames(), 150);
        assert_eq!(
            gif.peak_memory((656, 828)),
            Some(GIF_BASE + 150 * 656 * 828 * 9 / 2)
        );
        assert_eq!(plan(file("/work/demo.gif")).unwrap().frames(), 900);
        assert_eq!(
            plan(file("/work/demo.mp4"))
                .unwrap()
                .peak_memory((656, 828)),
            None
        );
    }

    #[test]
    fn mem_available_is_read_in_bytes_from_meminfo() {
        let meminfo =
            "MemTotal:       65000000 kB\nMemFree:  1000 kB\nMemAvailable:   43121836 kB\n";
        assert_eq!(available(meminfo), Some(43121836 * 1024));
        assert_eq!(available("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn a_recording_without_frames_is_an_error_and_one_with_frames_gives_its_counts() {
        let empty = outcome(
            "code",
            "/work/a.gif",
            0,
            0,
            0.0,
            "pane code closed".to_string(),
            Ok(()),
        );
        assert_eq!(
            empty.unwrap_err(),
            "the recording of pane code ended before its first frame, as pane code closed, so decade wrote no file"
        );
        let done = outcome("code", "/work/a.gif", 30, 2, 2.04, "x".to_string(), Ok(()));
        assert_eq!(
            done.unwrap(),
            Recorded {
                file: "/work/a.gif".to_string(),
                frames: 30,
                dropped: 2,
                seconds: 2.0,
                ended: "x".to_string(),
            }
        );
        let failed = outcome(
            "code",
            "/work/a.gif",
            30,
            0,
            2.0,
            "x".to_string(),
            Err("ffmpeg failed".to_string()),
        );
        assert_eq!(failed.unwrap_err(), "ffmpeg failed");
    }

    #[test]
    fn a_span_reads_in_whole_minutes_or_else_seconds() {
        assert_eq!(span(GIF_LIMIT), "1 minute");
        assert_eq!(span(MP4_LIMIT), "5 minutes");
        assert_eq!(span(Duration::from_millis(4500)), "4.5 seconds");
        assert_eq!(span(Duration::from_secs(90)), "90 seconds");
    }

    #[test]
    fn the_clock_shows_minutes_and_two_digit_seconds() {
        assert_eq!(clock(Duration::from_millis(7400)), "0:07");
        assert_eq!(clock(Duration::from_secs(65)), "1:05");
        assert_eq!(clock(MP4_LIMIT), "5:00");
    }

    #[test]
    fn a_larger_frame_is_cropped_from_its_top_left() {
        // 4 by 4 into 2 by 2 keeps columns 0 and 1 of rows 0 and 1
        let frame: Vec<u8> = (0..16u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let placed = place(&frame, (4, 4), (2, 2));
        let pixels: Vec<u8> = placed.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert_eq!(pixels, [0, 1, 4, 5]);
    }

    #[test]
    fn a_smaller_frame_is_padded_with_black_on_the_right_and_bottom() {
        // 2 by 1 into 3 by 2
        let frame = [7, 7, 7, 255, 8, 8, 8, 255];
        let placed = place(&frame, (2, 1), (3, 2));
        let pixels: Vec<u8> = placed.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert_eq!(pixels, [7, 8, 0, 0, 0, 0]);
        assert!(placed.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }

    #[test]
    fn the_pointer_is_drawn_only_with_its_tip_inside_the_frame_and_clipped_at_the_edge() {
        let pixel = |frame: &[u8], x: usize, y: usize| frame[(y * 4 + x) * 4];
        let mut frame = vec![9; 4 * 4 * 4];
        draw_pointer(&mut frame, (4, 4), (1, 1), 1);
        assert_eq!(pixel(&frame, 0, 0), 9);
        assert_eq!(pixel(&frame, 1, 1), 0);
        assert_eq!(pixel(&frame, 2, 3), 255);
        assert_eq!(pixel(&frame, 3, 1), 9);
        for tip in [(-1, 1), (1, -1), (4, 0), (0, 4)] {
            let mut frame = vec![9; 4 * 4 * 4];
            draw_pointer(&mut frame, (4, 4), tip, 2);
            assert!(frame.iter().all(|&byte| byte == 9), "tip {tip:?}");
        }
    }
}
