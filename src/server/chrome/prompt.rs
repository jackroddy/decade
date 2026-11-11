use crate::server::chrome::draw::{Face, Scene, scaled};
use crate::server::chrome::frame::{CARD_RADIUS, SHADOW_OFFSET};
use crate::server::chrome::palette;
use crate::server::chrome::text::fit;

// the sizes below are in pixels at 18 px rows, for `scaled`

const PADDING: i32 = 12;

/// The space above and below a choice's text, inside its row.
const ROW_PADDING: i32 = 3;
const ROW_GAP: i32 = 4;
const ROW_RADIUS: i32 = 4;

/// How the person answers decade's question whether an agent may take a
/// screenshot of a pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Once,

    /// Allow this one and turn the pane's accept screenshots mode on.
    Always,
    Deny,
}

const CHOICES: [(Answer, &str); 3] = [
    (Answer::Once, "y  Allow once"),
    (Answer::Always, "a  Allow and accept screenshots"),
    (Answer::Deny, "n  Deny"),
];

/// The answer a key gives, for y, a and n.
pub fn answer_for(key: char) -> Option<Answer> {
    match key {
        'y' => Some(Answer::Once),
        'a' => Some(Answer::Always),
        'n' => Some(Answer::Deny),
        _ => None,
    }
}

/// A box of pixels: its top left and size.
type Bounds = ((i32, i32), (i32, i32));

/// The dialog in the middle of the window that asks the person whether
/// `agent` may take a screenshot of pane `pane` in workspace `workspace`.
pub struct Prompt {
    pub agent: String,
    pub workspace: String,
    pub pane: String,

    /// Whether the dialog has been up long enough to take an answer.
    pub armed: bool,

    /// The window's size in pixels.
    pub window: (i32, i32),

    /// The chrome font's cell.
    pub cell: (i32, i32),
}

impl Prompt {
    fn lines(&self, room: usize) -> [String; 2] {
        [
            fit(&format!("{} asks for a screenshot", self.agent), room),
            fit(
                &format!("of pane {} in workspace {}", self.pane, self.workspace),
                room,
            ),
        ]
    }

    /// The dialog and each choice's row in it.
    fn layout(&self) -> (Bounds, [(Answer, Bounds); 3]) {
        let (width, height) = self.window;
        let (cell_width, cell_height) = self.cell;
        let padding = scaled(PADDING, cell_height);
        let row = cell_height + 2 * scaled(ROW_PADDING, cell_height);
        let gap = scaled(ROW_GAP, cell_height);
        let longest = (CHOICES.iter().map(|(_, label)| label.chars().count()))
            .chain(self.lines(usize::MAX).map(|line| line.chars().count()))
            .max()
            .unwrap_or(0) as i32;
        let size = (
            (2 * padding + (longest + 2) * cell_width).min(width),
            2 * padding + 2 * cell_height + 3 * gap + 3 * row + 2 * gap,
        );
        let at = ((width - size.0).max(0) / 2, (height - size.1).max(0) / 2);
        let first = at.1 + padding + 2 * cell_height + 3 * gap;
        let rows = [0, 1, 2].map(|i| {
            let (answer, _) = CHOICES[i];
            let y = first + i as i32 * (row + gap);
            (answer, ((at.0 + padding, y), (size.0 - 2 * padding, row)))
        });
        ((at, size), rows)
    }

    /// The choice under pixel `x`, `y`, if any.
    pub fn hit(&self, x: i32, y: i32) -> Option<Answer> {
        let inside = |((left, top), (width, height)): Bounds| {
            (left..left + width).contains(&x) && (top..top + height).contains(&y)
        };
        let (_, rows) = self.layout();
        (rows.into_iter()).find_map(|(answer, row)| inside(row).then_some(answer))
    }

    /// Draw the dialog in window pixels, with the chrome font.
    pub fn draw(&self, scene: &mut Scene) {
        let (((left, top), size), rows) = self.layout();
        let (cell_width, cell_height) = self.cell;
        let padding = scaled(PADDING, cell_height);
        let radius = scaled(CARD_RADIUS, cell_height);
        let offset = scaled(SHADOW_OFFSET, cell_height);
        let shadow = palette::SHADOW;
        scene.rounded(left + offset, top + offset, size, radius, shadow, shadow);
        scene.rounded(
            left,
            top,
            size,
            radius,
            palette::LIGHTER_BACKGROUND,
            palette::MAGENTA,
        );
        let room = ((size.0 - 2 * padding) / cell_width - 2).max(0) as usize;
        let [question, place] = self.lines(room);
        let x = left + padding + cell_width;
        let y = top + padding;
        scene.text_px(x, y, &question, Face::ChromeBold, palette::FOREGROUND);
        let y = y + cell_height + scaled(ROW_GAP, cell_height);
        scene.text_px(x, y, &place, Face::Chrome, palette::LIGHT_FOREGROUND);

        let row_radius = scaled(ROW_RADIUS, cell_height);
        let text = if self.armed {
            palette::FOREGROUND
        } else {
            palette::MUTED
        };
        for ((answer, ((row_left, row_top), row_size)), (_, label)) in rows.into_iter().zip(CHOICES)
        {
            let fill = match answer {
                Answer::Deny => palette::DARK_RED,
                _ => palette::SELECTION,
            };
            scene.rounded(row_left, row_top, row_size, row_radius, fill, fill);
            let text_y = row_top + scaled(ROW_PADDING, cell_height);
            let label = fit(label, room);
            scene.text_px(x, text_y, &label, Face::Chrome, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> Prompt {
        Prompt {
            agent: "coax".to_string(),
            workspace: "decade".to_string(),
            pane: "terminal".to_string(),
            armed: true,
            window: (800, 600),
            cell: (8, 18),
        }
    }

    #[test]
    fn y_a_and_n_answer_and_other_keys_do_not() {
        assert_eq!(answer_for('y'), Some(Answer::Once));
        assert_eq!(answer_for('a'), Some(Answer::Always));
        assert_eq!(answer_for('n'), Some(Answer::Deny));
        assert_eq!(answer_for('x'), None);
    }

    #[test]
    fn the_dialog_is_centred_in_the_window_and_a_press_on_a_choice_answers() {
        let prompt = prompt();
        let (((left, top), (width, height)), rows) = prompt.layout();
        assert!((left - (800 - (left + width))).abs() <= 1);
        assert!((top - (600 - (top + height))).abs() <= 1);
        for (answer, ((x, y), (w, h))) in rows {
            assert_eq!(prompt.hit(x + w / 2, y + h / 2), Some(answer));
        }
        assert_eq!(prompt.hit(left + 1, top + 1), None);
        assert_eq!(prompt.hit(left - 1, top + 1), None);
    }

    #[test]
    fn the_dialog_is_no_wider_than_the_window() {
        let prompt = Prompt {
            window: (120, 600),
            ..prompt()
        };
        let (((left, _), (width, _)), _) = prompt.layout();
        assert_eq!((left, width), (0, 120));
    }
}
