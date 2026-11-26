use serde::Deserialize;

use crate::script;
use crate::wire::{BTN_LEFT, BTN_RIGHT};

const BTN_MIDDLE: u32 = 0x112;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFTSHIFT: u32 = 42;

/// The most steps a script may hold.
pub const MAX_STEPS: usize = 500;

/// The longest a script may play, in milliseconds.
pub const MAX_MS: u64 = 5 * 60 * 1000;

/// The longest a glide or a wait may take, in milliseconds.
const MAX_GLIDE_MS: u64 = 10_000;
const MAX_WAIT_MS: u64 = 60_000;

const MOVE_MS: u64 = 400;
const DRAG_MS: u64 = 600;
const TYPE_MS: u64 = 60;

/// The time between two pointer positions of a glide, about 60 a second.
const FRAME_MS: u64 = 16;

/// How long a click holds its button, and the gap between the two clicks
/// of a double click.
const CLICK_HOLD_MS: u64 = 60;
const DOUBLE_GAP_MS: u64 = 80;

/// How long a drag holds still after its press and before its release.
const DRAG_SETTLE_MS: u64 = 80;

/// How long a chord holds its key, and the pause after it.
const KEY_HOLD_MS: u64 = 30;

/// A script of input for a scratch pane, as `scratch.input` takes it.
#[derive(Debug)]
pub struct Script {
    pub cell: Option<Cell>,
    pub steps: Vec<Step>,
}

/// The cell grid of a terminal in the pane, in pane pixels.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub width: f64,
    pub height: f64,

    /// Where column 0 starts, from the pane's left edge.
    #[serde(default)]
    pub left: f64,

    /// Where row 0 starts, from the pane's top edge.
    #[serde(default)]
    pub top: f64,
}

/// A point in the pane: pixels from its top left, or a cell of `Cell`'s
/// grid, counted from 0.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct At {
    x: Option<f64>,
    y: Option<f64>,
    col: Option<u32>,
    row: Option<u32>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// One step of a script. `ms` is a glide's duration for `move`, `click`
/// and `drag`, and the time per character for `type`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "do", rename_all = "lowercase", deny_unknown_fields)]
pub enum Step {
    Move {
        to: At,
        ms: Option<u64>,
    },
    Click {
        at: Option<At>,
        button: Option<Button>,
        double: Option<bool>,
        ms: Option<u64>,
    },
    Press {
        button: Option<Button>,
        key: Option<String>,
    },
    Release {
        button: Option<Button>,
        key: Option<String>,
    },
    Drag {
        from: Option<At>,
        to: At,
        button: Option<Button>,
        ms: Option<u64>,
    },
    Key {
        key: String,
    },
    Type {
        text: String,
        ms: Option<u64>,
    },
    Wait {
        ms: u64,
    },
}

/// What decade sends the pane at one moment of a script.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// The pointer to a point in pane pixels.
    Motion(f64, f64),

    /// An evdev button, pressed or released.
    Button(u32, bool),

    /// An evdev key, pressed or released.
    Key(u32, bool),

    /// The end of a step.
    Done,
}

/// An input `at` milliseconds after the script starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    pub at: u64,
    pub input: Input,
}

/// The events that play `script` in a pane `size` pixels in size, with
/// the pointer starting at `start`, or why the script cannot play.
pub fn plan(script: &Script, size: (f64, f64), start: (f64, f64)) -> Result<Vec<Event>, String> {
    if script.steps.is_empty() {
        return Err("the script has no steps".to_string());
    }
    if script.steps.len() > MAX_STEPS {
        return Err(format!(
            "the script has {} steps, more than the {MAX_STEPS} decade plays",
            script.steps.len()
        ));
    }
    let mut timeline = Timeline {
        events: Vec::new(),
        now: 0,
        pointer: start,
        cell: script.cell,
        size,
    };
    for (index, step) in script.steps.iter().enumerate() {
        (timeline.step(step)).map_err(|error| format!("step {}: {error}", index + 1))?;
        timeline.push(0, Input::Done);
        if timeline.now > MAX_MS {
            return Err(format!(
                "the script runs past step {} for longer than {} minutes",
                index + 1,
                MAX_MS / 60_000
            ));
        }
    }
    Ok(timeline.events)
}

/// The pointer positions of a glide from `from` to `to` over `ms`, each
/// with its time from the glide's start, eased in and out.
pub fn glide(from: (f64, f64), to: (f64, f64), ms: u64) -> Vec<(u64, (f64, f64))> {
    let frames = ms.div_ceil(FRAME_MS).max(1);
    (1..=frames)
        .map(|frame| {
            let t = frame as f64 / frames as f64;

            // smoothstep, 3t² - 2t³, which starts and stops at rest
            let eased = t * t * (3.0 - 2.0 * t);
            let point = (
                from.0 + (to.0 - from.0) * eased,
                from.1 + (to.1 - from.1) * eased,
            );
            (ms * frame / frames, point)
        })
        .collect()
}

/// The point `at` names in a pane `size` pixels in size, with `cell` the
/// grid of the terminal in it.
pub fn point(at: &At, cell: Option<&Cell>, size: (f64, f64)) -> Result<(f64, f64), String> {
    let (x, y) = match (at.x, at.y, at.col, at.row) {
        (Some(x), Some(y), None, None) => (x, y),
        (None, None, Some(col), Some(row)) => {
            let cell = cell.ok_or("a point by col and row needs cell in the call")?;
            (
                cell.left + (col as f64 + 0.5) * cell.width,
                cell.top + (row as f64 + 0.5) * cell.height,
            )
        }
        _ => return Err("a point is either x and y or col and row".to_string()),
    };
    if !(0.0..size.0).contains(&x) || !(0.0..size.1).contains(&y) {
        return Err(format!(
            "the point ({x}, {y}) is outside the pane, which is {} by {} pixels",
            size.0, size.1
        ));
    }
    Ok((x, y))
}

struct Timeline {
    events: Vec<Event>,
    now: u64,
    pointer: (f64, f64),
    cell: Option<Cell>,
    size: (f64, f64),
}

impl Timeline {
    fn push(&mut self, after: u64, input: Input) {
        self.now += after;
        self.events.push(Event {
            at: self.now,
            input,
        });
    }

    fn step(&mut self, step: &Step) -> Result<(), String> {
        match step {
            Step::Move { to, ms } => {
                let to = self.point(to)?;
                self.glide(to, glide_ms(*ms, MOVE_MS)?);
            }
            Step::Click {
                at,
                button,
                double,
                ms,
            } => {
                if let Some(at) = at {
                    let at = self.point(at)?;
                    self.glide(at, glide_ms(*ms, MOVE_MS)?);
                }
                let button = code(button.unwrap_or(Button::Left));
                let clicks = if double.unwrap_or(false) { 2 } else { 1 };
                for click in 0..clicks {
                    let gap = if click == 0 { 0 } else { DOUBLE_GAP_MS };
                    self.push(gap, Input::Button(button, true));
                    self.push(CLICK_HOLD_MS, Input::Button(button, false));
                }
            }
            Step::Press { button, key } | Step::Release { button, key } => {
                let pressed = matches!(step, Step::Press { .. });
                let input = match (button, key) {
                    (Some(button), None) => Input::Button(code(*button), pressed),
                    (None, Some(key)) => Input::Key(lone_key(key)?, pressed),
                    _ => return Err("press and release take either button or key".to_string()),
                };
                self.push(0, input);
            }
            Step::Drag {
                from,
                to,
                button,
                ms,
            } => {
                if let Some(from) = from {
                    let from = self.point(from)?;
                    self.glide(from, glide_ms(None, MOVE_MS)?);
                }
                let to = self.point(to)?;
                let button = code(button.unwrap_or(Button::Left));
                self.push(0, Input::Button(button, true));
                self.now += DRAG_SETTLE_MS;
                self.glide(to, glide_ms(*ms, DRAG_MS)?);
                self.push(DRAG_SETTLE_MS, Input::Button(button, false));
            }
            Step::Key { key } => {
                let (modifiers, key) = script::chord(key)?;
                for &modifier in &modifiers {
                    self.push(0, Input::Key(modifier, true));
                }
                self.push(0, Input::Key(key, true));
                self.push(KEY_HOLD_MS, Input::Key(key, false));
                for &modifier in modifiers.iter().rev() {
                    self.push(0, Input::Key(modifier, false));
                }
                self.now += KEY_HOLD_MS;
            }
            Step::Type { text, ms } => {
                let pace = ms.unwrap_or(TYPE_MS);
                if !(10..=1000).contains(&pace) {
                    return Err(format!("type's ms is {pace}, outside 10 to 1000"));
                }
                let hold = (pace / 2).min(KEY_HOLD_MS);
                for ch in text.chars() {
                    let (key, shift) = typed_key(ch).ok_or(format!("no key types {ch:?}"))?;
                    let start = self.now;
                    if shift {
                        self.push(0, Input::Key(KEY_LEFTSHIFT, true));
                    }
                    self.push(0, Input::Key(key, true));
                    self.push(hold, Input::Key(key, false));
                    if shift {
                        self.push(0, Input::Key(KEY_LEFTSHIFT, false));
                    }
                    self.now = start + pace;
                }
            }
            Step::Wait { ms } => {
                if *ms > MAX_WAIT_MS {
                    return Err(format!("wait's ms is {ms}, more than {MAX_WAIT_MS}"));
                }
                self.now += ms;
            }
        }
        Ok(())
    }

    fn point(&self, at: &At) -> Result<(f64, f64), String> {
        point(at, self.cell.as_ref(), self.size)
    }

    fn glide(&mut self, to: (f64, f64), ms: u64) {
        let start = self.now;
        for (offset, point) in glide(self.pointer, to, ms) {
            self.now = start + offset;
            self.push(0, Input::Motion(point.0, point.1));
        }
        self.pointer = to;
    }
}

fn glide_ms(ms: Option<u64>, default: u64) -> Result<u64, String> {
    match ms.unwrap_or(default) {
        ms if ms > MAX_GLIDE_MS => Err(format!("ms is {ms}, more than {MAX_GLIDE_MS}")),
        ms => Ok(ms),
    }
}

fn code(button: Button) -> u32 {
    match button {
        Button::Left => BTN_LEFT,
        Button::Right => BTN_RIGHT,
        Button::Middle => BTN_MIDDLE,
    }
}

/// The one key `name` names, with no modifiers.
fn lone_key(name: &str) -> Result<u32, String> {
    match script::chord(name)? {
        (modifiers, key) if modifiers.is_empty() => Ok(key),
        _ => Err(format!(
            "{name} is a chord, and press and release take one key"
        )),
    }
}

/// The evdev key that types `ch` and whether it needs Shift.
fn typed_key(ch: char) -> Option<(u32, bool)> {
    match ch {
        '\n' => Some((KEY_ENTER, false)),
        '\t' => Some((KEY_TAB, false)),
        _ => script::char_key(ch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SIZE: (f64, f64) = (800.0, 600.0);

    fn steps(value: serde_json::Value) -> Result<Vec<Step>, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    fn script(value: serde_json::Value) -> Script {
        Script {
            cell: None,
            steps: steps(value).unwrap(),
        }
    }

    fn xy(x: f64, y: f64) -> At {
        At {
            x: Some(x),
            y: Some(y),
            col: None,
            row: None,
        }
    }

    fn inputs(events: &[Event]) -> Vec<Input> {
        let moving = |event: &&Event| !matches!(event.input, Input::Motion(..));
        events
            .iter()
            .filter(moving)
            .map(|event| event.input)
            .collect()
    }

    #[test]
    fn steps_are_read_by_their_do_with_their_optional_fields() {
        let read = steps(json!([
            {"do": "move", "to": {"x": 10, "y": 20}},
            {"do": "click", "at": {"col": 3, "row": 1}, "button": "right", "double": true},
            {"do": "press", "key": "shift"},
            {"do": "drag", "to": {"x": 5, "y": 6}, "ms": 900},
            {"do": "key", "key": "ctrl+c"},
            {"do": "type", "text": "hi", "ms": 40},
            {"do": "wait", "ms": 250}
        ]))
        .unwrap();
        assert_eq!(
            read[0],
            Step::Move {
                to: xy(10.0, 20.0),
                ms: None
            }
        );
        assert!(matches!(
            read[1],
            Step::Click {
                button: Some(Button::Right),
                double: Some(true),
                ..
            }
        ));
        assert_eq!(read[6], Step::Wait { ms: 250 });
    }

    #[test]
    fn an_unknown_step_or_field_is_refused() {
        assert!(steps(json!([{"do": "scroll", "by": 3}])).is_err());
        assert!(steps(json!([{"do": "wait", "ms": 5, "for": 3}])).is_err());
        assert!(steps(json!([{"do": "move", "to": {"x": 1, "y": 2, "z": 3}}])).is_err());
        assert!(steps(json!([{"do": "click", "button": "fourth"}])).is_err());
        assert!(steps(json!([{"ms": 5}])).is_err());
    }

    #[test]
    fn a_cell_turns_into_its_centre_from_the_cell_size_and_padding() {
        let cell = Cell {
            width: 10.0,
            height: 20.0,
            left: 4.0,
            top: 2.0,
        };
        let at = At {
            x: None,
            y: None,
            col: Some(3),
            row: Some(1),
        };
        assert_eq!(point(&at, Some(&cell), SIZE), Ok((39.0, 32.0)));
        assert!(point(&at, None, SIZE).unwrap_err().contains("needs cell"));
        let far = At {
            col: Some(100),
            ..at
        };
        assert!(
            point(&far, Some(&cell), SIZE)
                .unwrap_err()
                .contains("outside")
        );
    }

    #[test]
    fn a_point_takes_x_and_y_or_col_and_row_and_must_be_inside_the_pane() {
        assert_eq!(point(&xy(0.0, 599.0), None, SIZE), Ok((0.0, 599.0)));
        assert!(point(&xy(800.0, 10.0), None, SIZE).is_err());
        assert!(point(&xy(-1.0, 10.0), None, SIZE).is_err());
        let mixed = At {
            col: Some(1),
            ..xy(1.0, 1.0)
        };
        assert!(point(&mixed, None, SIZE).unwrap_err().contains("either"));
    }

    #[test]
    fn a_glide_eases_from_rest_to_rest_and_ends_on_its_target_at_its_duration() {
        let path = glide((0.0, 0.0), (100.0, 50.0), 160);
        assert_eq!(path.len(), 10);
        assert_eq!(path.last(), Some(&(160, (100.0, 50.0))));
        let steps: Vec<f64> = (path.windows(2))
            .map(|pair| pair[1].1.0 - pair[0].1.0)
            .collect();
        assert!(steps[0] < steps[4] && steps[8] < steps[4]);
        assert!(path.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(glide((5.0, 5.0), (9.0, 9.0), 0), [(0, (9.0, 9.0))]);
    }

    #[test]
    fn a_click_glides_to_its_point_then_presses_and_releases() {
        let events = plan(
            &script(json!([{"do": "click", "at": {"x": 100, "y": 100}, "ms": 32}])),
            SIZE,
            (0.0, 0.0),
        )
        .unwrap();
        assert_eq!(events[0].input, Input::Motion(50.0, 50.0));
        assert_eq!(events[1].input, Input::Motion(100.0, 100.0));
        assert_eq!(
            inputs(&events),
            [
                Input::Button(BTN_LEFT, true),
                Input::Button(BTN_LEFT, false),
                Input::Done
            ]
        );
        assert_eq!(events[3].at, 32 + CLICK_HOLD_MS);
    }

    #[test]
    fn a_drag_holds_the_button_over_its_glide() {
        let events = plan(
            &script(json!([{"do": "drag", "to": {"x": 10, "y": 0}, "button": "middle"}])),
            SIZE,
            (0.0, 0.0),
        )
        .unwrap();
        assert_eq!(events[0].input, Input::Button(BTN_MIDDLE, true));
        let release = events.len() - 2;
        assert_eq!(events[release].input, Input::Button(BTN_MIDDLE, false));
        assert_eq!(events[release - 1].input, Input::Motion(10.0, 0.0));
        assert_eq!(events[release].at, 2 * DRAG_SETTLE_MS + DRAG_MS);
    }

    #[test]
    fn a_chord_presses_its_modifiers_around_its_key_and_type_shifts_capitals() {
        let events = plan(
            &script(json!([{"do": "key", "key": "ctrl+c"}, {"do": "type", "text": "A\n"}])),
            SIZE,
            (0.0, 0.0),
        )
        .unwrap();
        let (ctrl, c, a, shift) = (29, 46, 30, KEY_LEFTSHIFT);
        assert_eq!(
            inputs(&events),
            [
                Input::Key(ctrl, true),
                Input::Key(c, true),
                Input::Key(c, false),
                Input::Key(ctrl, false),
                Input::Done,
                Input::Key(shift, true),
                Input::Key(a, true),
                Input::Key(a, false),
                Input::Key(shift, false),
                Input::Key(KEY_ENTER, true),
                Input::Key(KEY_ENTER, false),
                Input::Done,
            ]
        );
        let typed: Vec<u64> = (events.iter())
            .filter(|event| event.input == Input::Key(KEY_ENTER, true))
            .map(|event| event.at)
            .collect();
        assert_eq!(typed, [2 * KEY_HOLD_MS + TYPE_MS]);
    }

    #[test]
    fn each_step_ends_with_a_done_and_a_wait_holds_off_the_next() {
        let events = plan(
            &script(json!([{"do": "wait", "ms": 500}, {"do": "press", "button": "left"}])),
            SIZE,
            (0.0, 0.0),
        )
        .unwrap();
        assert_eq!(
            events,
            [
                Event {
                    at: 500,
                    input: Input::Done
                },
                Event {
                    at: 500,
                    input: Input::Button(BTN_LEFT, true)
                },
                Event {
                    at: 500,
                    input: Input::Done
                },
            ]
        );
    }

    #[test]
    fn bad_steps_are_refused_with_their_number() {
        let refused = |value| plan(&script(value), SIZE, (0.0, 0.0)).unwrap_err();
        assert_eq!(refused(json!([])), "the script has no steps");
        assert_eq!(
            refused(json!([{"do": "wait", "ms": 1}, {"do": "key", "key": "hyper+a"}])),
            "step 2: hyper is not a modifier"
        );
        assert_eq!(
            refused(json!([{"do": "type", "text": "é"}])),
            "step 1: no key types 'é'"
        );
        assert!(refused(json!([{"do": "press"}])).contains("either button or key"));
        assert!(refused(json!([{"do": "press", "key": "ctrl+a"}])).contains("is a chord"));
        assert!(
            refused(json!([{"do": "move", "to": {"x": 1, "y": 1}, "ms": 20000}]))
                .contains("more than")
        );
        assert!(
            refused(json!([{"do": "type", "text": "a", "ms": 5}])).contains("outside 10 to 1000")
        );
        assert!(refused(json!([{"do": "wait", "ms": 70000}])).contains("more than"));
    }

    #[test]
    fn a_script_longer_than_five_minutes_or_500_steps_is_refused() {
        let waits = vec![json!({"do": "wait", "ms": 60000}); 6];
        let refused = plan(&script(json!(waits)), SIZE, (0.0, 0.0)).unwrap_err();
        assert_eq!(
            refused,
            "the script runs past step 6 for longer than 5 minutes"
        );
        let many = vec![json!({"do": "wait", "ms": 0}); MAX_STEPS + 1];
        let refused = plan(&script(json!(many)), SIZE, (0.0, 0.0)).unwrap_err();
        assert!(refused.contains("more than the 500"));
        let most = vec![json!({"do": "wait", "ms": 0}); MAX_STEPS];
        assert!(plan(&script(json!(most)), SIZE, (0.0, 0.0)).is_ok());
    }
}
