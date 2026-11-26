use std::collections::VecDeque;
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

use crate::files;
use crate::record::{format_for, summary};
use crate::server::{mcp, scratch, workspace};

use super::State;
use super::screenshot::{Asker, crop};

/// The longest a recording runs.
pub(super) const LONGEST: Duration = Duration::from_secs(5 * 60);

const MP4_FPS: u32 = 30;
const GIF_FPS: u32 = 15;
const MAX_FPS: u32 = 60;
const QUALITY: u32 = 23;

/// The highest crf libx264 takes.
const WORST_QUALITY: u32 = 51;
const MAX_WIDTH: u32 = 7680;

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
const ARROW: [&str; 17] = [
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

/// Where the answer to a `record` goes.
pub(super) enum Requester {
    Mcp(mcp::Reply),

    /// Socket client `client`; `message` is its line, for the error reply.
    Socket {
        client: usize,
        message: Value,
    },
}

/// A recording's settings, checked and with their defaults filled in.
#[derive(Debug, PartialEq)]
pub(super) struct Plan {
    pub file: PathBuf,
    pub format: Format,
    pub fps: u32,

    /// How long to record, at most `LONGEST`, or `None` to record until
    /// stopped or `LONGEST` has passed.
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
                Some(Duration::from_secs_f64(seconds).min(LONGEST))
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
}

/// A recording, from its first frame until decade answers for it.
pub(super) struct Recording {
    id: u64,
    workspace: u32,
    pane: String,
    plan: Plan,

    to: Requester,
    started: Instant,

    /// The size of every frame, the pane's client area when it started.
    size: (i32, i32),

    /// The queue to the thread that feeds ffmpeg, until the recording
    /// stops.
    frames: Option<SyncSender<Vec<u8>>>,
    sent: u64,
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
}

impl State {
    /// Record pane `pane` by `record`, at once when the pane accepts
    /// captures or else once the person allows it, and answer `to` when
    /// the file is written.
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

    /// The index of the running recording of the pane that `pane` names in
    /// workspace `workspace`.
    fn recording_of(&self, workspace: usize, pane: &str) -> Option<usize> {
        let shown = &self.workspaces[workspace];
        let target = shown.by_name(pane)?;
        self.recordings.iter().position(|recording| {
            recording.running()
                && recording.workspace == shown.id
                && shown.by_name(&recording.pane) == Some(target)
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
                let args = ffmpeg_args(&plan, size);
                let ffmpeg = Command::new("ffmpeg")
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|error| format!("starting ffmpeg failed: {error}"))?;
                Ok((size, ffmpeg))
            });
        let (size, ffmpeg) = match started {
            Ok(started) => started,
            Err(reason) => {
                self.answer_record(to, Err(reason));
                return;
            }
        };
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
        let shown = &self.workspaces[workspace];
        let on_screen = self.on_screen(shown.id);
        self.recordings.push(Recording {
            id,
            workspace: shown.id,
            pane,
            plan,
            to,
            started: now,
            size,
            frames: Some(frames),
            sent: 0,
            dropped: 0,
            timer: timer.ok(),
            next: now,
            shown: on_screen,
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
        let frame = self.record_frame(index);
        let recording = &mut self.recordings[index];
        let queued = match (frame, &recording.frames) {
            (Ok(frame), Some(frames)) => frames.try_send(frame),
            _ => Err(TrySendError::Full(Vec::new())),
        };
        match queued {
            Ok(()) => recording.sent += 1,
            Err(TrySendError::Full(_)) => recording.dropped += 1,

            // the feeding thread has exited, and
            // recording_written answers once its result arrives
            Err(TrySendError::Disconnected(_)) => {
                recording.timer = None;
                return TimeoutAction::Drop;
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
        let pane = &recording.pane;
        let gone = match &recording.to {
            Requester::Mcp(reply) => reply.is_closed(),
            Requester::Socket { client, .. } => !self.sockets.contains_key(client),
        };
        if gone {
            return Some("the requester went away".to_string());
        }
        let Some(workspace) = self.workspace_index(recording.workspace) else {
            return Some(format!("workspace {} closed", recording.workspace));
        };
        let shown = &self.workspaces[workspace];
        if shown.asleep.is_some() {
            return Some(format!("workspace {} went to sleep", shown.name));
        }
        let Some((tab, _)) = shown.by_name(pane) else {
            return Some(format!("pane {pane} closed"));
        };
        if tab != shown.active_tab {
            return Some(format!("the tab of pane {pane} left the screen"));
        }
        let on_screen = self.on_screen(recording.workspace);
        if recording.shown && !on_screen {
            return Some(format!("workspace {} left the screen", shown.name));
        }
        let elapsed = recording.started.elapsed();
        let ended = match recording.plan.duration {
            Some(duration) if elapsed >= duration => Some("it ran for its duration".to_string()),
            None if elapsed >= LONGEST => Some(format!(
                "it reached the limit of {} minutes",
                LONGEST.as_secs() / 60
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
        let (pane, size, pointer) = (
            recording.pane.clone(),
            recording.size,
            recording.plan.pointer,
        );
        let workspace = (self.workspace_index(recording.workspace))
            .ok_or_else(|| format!("workspace {} is closed", recording.workspace))?;
        let (_, rect) = self.client_px(workspace, &pane)?;
        self.send_frames_to(workspace, &pane);
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
            frame = fit(&frame, (width, height), (size.0 as usize, size.1 as usize));
        }
        Ok(frame)
    }

    /// Let the windows of pane `pane` draw their next frame, which no
    /// viewer asks for while none shows the workspace.
    fn send_frames_to(&self, workspace: usize, pane: &str) {
        let shown = &self.workspaces[workspace];
        let Some((tab, id)) = shown.by_name(pane) else {
            return;
        };
        if let Some(toplevel) = shown.tabs[tab].toplevel(id) {
            let surface = toplevel.wl_surface();
            let time = self.now();
            super::send_frames_surface_tree(surface, time);
            for (popup, _) in super::popups_over(surface) {
                super::send_frames_surface_tree(popup.wl_surface(), time);
            }
        }
    }

    /// End the recording of the pane `pane` names, for a stop call.
    pub(super) fn stop_record(&mut self, workspace: usize, pane: &str) -> Result<String, String> {
        let index = (self.recording_of(workspace, pane))
            .ok_or_else(|| format!("pane {pane} is not being recorded"))?;
        self.stop_recording(index, "a stop call ended it".to_string());
        Ok(format!(
            "stopped the recording of pane {pane}; the record call answers once the file is written"
        ))
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

    /// Answer for recording `id`, whose ffmpeg has exited with `result`.
    pub(super) fn recording_written(&mut self, id: u64, result: Result<(), String>) {
        let Some(index) = self.recordings.iter().position(|r| r.id == id) else {
            return;
        };
        if self.recordings[index].running() {
            self.stop_recording(index, "ffmpeg exited".to_string());
        }
        let recording = self.recordings.remove(index);
        let (ended, seconds) = recording.ended.unwrap_or_default();
        let answer = result.map(|()| Recorded {
            file: recording.plan.file.display().to_string(),
            frames: recording.sent,
            dropped: recording.dropped,
            seconds: (seconds * 10.0).round() / 10.0,
            ended,
        });
        self.answer_record(recording.to, answer);
    }

    /// Send `to` the finished recording, or the reason there is none.
    pub(super) fn answer_record(&mut self, to: Requester, answer: Result<Recorded, String>) {
        match to {
            Requester::Mcp(reply) => {
                let text = answer.map(|recorded| mcp::Answer::Text(summary(&recorded)));
                let _ = reply.send(text);
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
        let layout = shown.layout();
        let running = (self.recordings.iter())
            .filter(|recording| recording.running() && recording.workspace == shown.id)
            .filter_map(|recording| Some((layout.by_name(&recording.pane)?, recording.started)));
        let mut panes = Vec::new();
        for (pane, started) in running {
            let elapsed = now.saturating_duration_since(started);
            times.push(clock(elapsed));
            panes.push((pane, elapsed));
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

/// `elapsed` as minutes and seconds, `m:ss`.
fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The arguments that make ffmpeg read RGBA frames `size` pixels in size
/// on its stdin and write them to `plan`'s file.
pub(super) fn ffmpeg_args(plan: &Plan, size: (i32, i32)) -> Vec<String> {
    let (width, height) = size;
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
        "-r",
        &plan.fps.to_string(),
        "-i",
        "-",
    ]
    .map(str::to_string)
    .into();
    let scaled = plan.width.unwrap_or(width as u32);
    let output: Vec<String> = match plan.format {
        // yuv420p, which players expect, needs an even width
        // and height
        Format::Mp4 => [
            "-vf",
            &format!("scale={}:-2:flags=lanczos", (scaled / 2 * 2).max(2)),
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            &plan.quality.to_string(),
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ]
        .map(str::to_string)
        .into(),
        // the palette is made from every frame, so ffmpeg holds
        // them all until the input ends
        Format::Gif => [
            "-vf",
            &format!(
                "scale={scaled}:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse=diff_mode=rectangle"
            ),
            "-loop",
            if plan.forever { "0" } else { "-1" },
        ]
        .map(str::to_string)
        .into(),
    };
    args.extend(output);
    args.extend(["-n".to_string(), plan.file.display().to_string()]);
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

/// Draw the arrow pointer into an RGBA frame `size` pixels in size, its
/// tip at `tip`, each of its pixels `scale` pixels square. The parts off
/// the frame are left out.
fn draw_pointer(frame: &mut [u8], size: (usize, usize), tip: (i32, i32), scale: usize) {
    let (width, height) = (size.0 as i32, size.1 as i32);
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
                    if (0..width).contains(&x) && (0..height).contains(&y) {
                        let at = (y * width + x) as usize * 4;
                        frame[at..at + 4].copy_from_slice(&color);
                    }
                }
            }
        }
    }
}

/// An RGBA frame `from` pixels in size scaled to fit `to`, keeping its
/// aspect ratio, nearest pixel first, and centred on black.
fn fit(frame: &[u8], from: (usize, usize), to: (usize, usize)) -> Vec<u8> {
    let mut fitted = [0, 0, 0, 255].repeat(to.0 * to.1);
    if from.0 == 0 || from.1 == 0 {
        return fitted;
    }

    // the scale is to.0 / from.0 or to.1 / from.1, whichever is
    // smaller, kept as a fraction so the sizes stay exact
    let (num, den) = if to.0 * from.1 <= to.1 * from.0 {
        (to.0, from.0)
    } else {
        (to.1, from.1)
    };
    let (width, height) = (
        (from.0 * num / den).clamp(1, to.0),
        (from.1 * num / den).clamp(1, to.1),
    );
    let (left, top) = ((to.0 - width) / 2, (to.1 - height) / 2);
    for y in 0..height {
        let source_y = y * from.1 / height;
        for x in 0..width {
            let source_x = x * from.0 / width;
            let source = (source_y * from.0 + source_x) * 4;
            let target = ((top + y) * to.0 + left + x) * 4;
            fitted[target..target + 4].copy_from_slice(&frame[source..source + 4]);
        }
    }
    fitted
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
    fn a_duration_past_five_minutes_is_cut_to_five_minutes() {
        let long = plan(Record {
            duration: Some(600.0),
            ..file("/work/demo.mp4")
        });
        assert_eq!(long.unwrap().duration, Some(LONGEST));
        let short = plan(Record {
            duration: Some(2.5),
            ..file("/work/demo.mp4")
        });
        assert_eq!(short.unwrap().duration, Some(Duration::from_millis(2500)));
    }

    #[test]
    fn an_mp4_is_h264_in_yuv420p_at_an_even_width_with_its_crf() {
        let plan = Plan {
            width: Some(641),
            quality: 18,
            ..plan(file("/work/demo.mp4")).unwrap()
        };
        assert_eq!(
            ffmpeg_args(&plan, (657, 829)).join(" "),
            "-hide_banner -loglevel error -f rawvideo -pix_fmt rgb0 -s 657x829 -r 30 -i - \
             -vf scale=640:-2:flags=lanczos -c:v libx264 -preset medium -crf 18 \
             -pix_fmt yuv420p -movflags +faststart -n /work/demo.mp4"
        );
    }

    #[test]
    fn a_gif_makes_its_palette_from_its_frames_and_loops_unless_told_not_to() {
        let gif = plan(file("/work/demo.gif")).unwrap();
        assert_eq!(
            ffmpeg_args(&gif, (656, 828)).join(" "),
            "-hide_banner -loglevel error -f rawvideo -pix_fmt rgb0 -s 656x828 -r 15 -i - \
             -vf scale=656:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse=diff_mode=rectangle \
             -loop 0 -n /work/demo.gif"
        );
        let once = Plan {
            forever: false,
            ..gif
        };
        let args = ffmpeg_args(&once, (656, 828));
        let at = args.iter().position(|arg| arg == "-loop").unwrap();
        assert_eq!(args[at + 1], "-1");
    }

    #[test]
    fn the_clock_shows_minutes_and_two_digit_seconds() {
        assert_eq!(clock(Duration::from_millis(7400)), "0:07");
        assert_eq!(clock(Duration::from_secs(65)), "1:05");
        assert_eq!(clock(LONGEST), "5:00");
    }

    #[test]
    fn a_wider_frame_fits_by_its_width_centred_between_black_bars() {
        // a 4 by 2 frame into 4 by 4: it keeps its size and sits
        // on rows 1 and 2
        let frame: Vec<u8> = (0..8u8).flat_map(|i| [i, i, i, 255]).collect();
        let fitted = fit(&frame, (4, 2), (4, 4));
        let rows: Vec<Vec<u8>> = fitted
            .as_chunks::<16>()
            .0
            .iter()
            .map(|row| row.as_chunks::<4>().0.iter().map(|p| p[0]).collect())
            .collect();
        assert_eq!(
            rows,
            [[0, 0, 0, 0], [0, 1, 2, 3], [4, 5, 6, 7], [0, 0, 0, 0]]
        );
        assert!(fitted.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }

    #[test]
    fn a_larger_frame_shrinks_to_fit_nearest_pixel_first() {
        // 4 by 4 into 2 by 2 keeps every other pixel of every
        // other row
        let frame: Vec<u8> = (0..16u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let fitted = fit(&frame, (4, 4), (2, 2));
        let pixels: Vec<u8> = fitted.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert_eq!(pixels, [0, 2, 8, 10]);
    }

    #[test]
    fn the_pointer_is_drawn_at_its_tip_and_clipped_at_the_frame_edge() {
        let mut frame = vec![9; 4 * 4 * 4];
        draw_pointer(&mut frame, (4, 4), (1, 1), 1);
        let pixel = |frame: &[u8], x: usize, y: usize| frame[(y * 4 + x) * 4];
        assert_eq!(pixel(&frame, 0, 0), 9);
        assert_eq!(pixel(&frame, 1, 1), 0);
        assert_eq!(pixel(&frame, 2, 3), 255);
        assert_eq!(pixel(&frame, 3, 1), 9);
        let mut frame = vec![9; 4 * 4 * 4];
        draw_pointer(&mut frame, (4, 4), (-1, -40), 2);
        assert!(frame.iter().all(|&byte| byte == 9));
    }
}
