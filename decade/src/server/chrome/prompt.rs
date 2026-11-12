use crate::server::chrome::draw::Scene;
use crate::server::chrome::menu::MenuKey;
use crate::server::chrome::overlay::{Cursor, LARGE, LARGE_COLUMNS, Layout, Row, Step};
use crate::server::chrome::text::fit;

/// How the person answers decade's question whether an agent may take a
/// screenshot of a pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Once,

    /// Allow this one and turn the pane's accept screenshots mode on.
    Always,
    Deny,
}

const CHOICES: [(Answer, &str, char); 3] = [
    (Answer::Once, "Allow once", 'y'),
    (Answer::Always, "Allow and accept screenshots", 'a'),
    (Answer::Deny, "Deny", 'n'),
];

/// The answer a key gives, for y, a and n.
pub fn answer_for(key: char) -> Option<Answer> {
    (CHOICES.iter())
        .find(|&&(_, _, choice)| choice == key)
        .map(|&(answer, _, _)| answer)
}

/// The dialog in the middle of the window that asks the person whether
/// `agent` may take a screenshot of pane `pane` in workspace `workspace`.
pub struct Prompt {
    pub agent: String,
    pub workspace: String,
    pub pane: String,

    /// Whether the dialog has been up long enough to take an answer.
    pub armed: bool,

    /// The choice the dialog shows selected.
    pub selected: usize,

    /// The window's size in pixels.
    pub window: (i32, i32),

    /// The palette font's cell.
    pub cell: (i32, i32),
}

impl Prompt {
    fn rows(&self) -> [Row<'static>; 3] {
        CHOICES.map(|(answer, label, key)| Row {
            key: Some(key),
            destructive: answer == Answer::Deny,
            available: self.armed,
            ..Row::new(label)
        })
    }

    fn layout(&self) -> Layout {
        let width = LARGE.width(self.cell, LARGE_COLUMNS).min(self.window.0);
        (Layout::new(&LARGE, self.cell, width))
            .header(true)
            .list(CHOICES.len())
            .end()
            .centred(self.window)
    }

    /// The answer `key` gives, moving `cursor` for Up and Down. Escape
    /// denies.
    pub fn key(&self, cursor: &mut Cursor, key: MenuKey) -> Option<Answer> {
        match cursor.key(key, &self.rows()) {
            Step::Run(index) => Some(CHOICES[index].0),
            Step::Close => Some(Answer::Deny),
            Step::Stay | Step::Pass(_) => None,
        }
    }

    /// Select the choice under pixel `x`, `y`, and return whether the
    /// selection moved.
    pub fn hover(&self, cursor: &mut Cursor, x: i32, y: i32) -> bool {
        let Some(list) = self.layout().list_area() else {
            return false;
        };
        cursor.hover(&list, &self.rows(), x, y)
    }

    /// The choice under pixel `x`, `y`, if any.
    pub fn hit(&self, x: i32, y: i32) -> Option<Answer> {
        let list = self.layout().list_area()?;
        let mut cursor = Cursor::default();
        let index = cursor.click(&list, &self.rows(), x, y)?;
        Some(CHOICES[index].0)
    }

    /// Draw the dialog in window pixels, with the palette font.
    pub fn draw(&self, scene: &mut Scene) {
        let layout = self.layout();
        layout.draw_box(scene);
        let (cell_width, _) = self.cell;
        let room = ((layout.width - 2 * LARGE.text_left(self.cell.1)) / cell_width).max(0);
        let room = room as usize;
        let question = fit(&format!("{} asks for a screenshot", self.agent), room);
        let place = format!("of pane {} in workspace {}", self.pane, self.workspace);
        layout.draw_header(scene, &question, Some(&fit(&place, room)));
        let Some(list) = layout.list_area() else {
            return;
        };
        for (i, row) in self.rows().iter().enumerate() {
            let label = fit(row.label, room.saturating_sub(4));
            let row = Row {
                label: &label,
                ..*row
            };
            list.draw_row(scene, i, &row, i == self.selected);
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
            selected: 0,
            window: (800, 600),
            cell: (10, 25),
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
        let layout = prompt.layout();
        let ((left, top), (width, height)) = (layout.at, (layout.width, layout.height));
        assert!((left - (800 - (left + width))).abs() <= 1);
        assert!((top - (600 - (top + height))).abs() <= 1);
        let list = layout.list_area().unwrap();
        for (i, (answer, _, _)) in CHOICES.into_iter().enumerate() {
            assert_eq!(prompt.hit(list.left(), list.text_y(i)), Some(answer));
        }
        assert_eq!(prompt.hit(left + 1, top + 1), None);
        assert_eq!(prompt.hit(left - 1, list.text_y(0)), None);
    }

    #[test]
    fn the_dialog_is_no_wider_than_the_window() {
        let prompt = Prompt {
            window: (120, 600),
            ..prompt()
        };
        let layout = prompt.layout();
        assert_eq!((layout.at.0, layout.width), (0, 120));
    }

    #[test]
    fn arrows_move_the_selection_and_enter_gives_the_selected_answer() {
        let prompt = prompt();
        let mut cursor = Cursor::default();
        assert_eq!(prompt.key(&mut cursor, MenuKey::Enter), Some(Answer::Once));
        assert_eq!(prompt.key(&mut cursor, MenuKey::Down), None);
        assert_eq!(
            prompt.key(&mut cursor, MenuKey::Enter),
            Some(Answer::Always)
        );
        prompt.key(&mut cursor, MenuKey::Down);
        prompt.key(&mut cursor, MenuKey::Down);
        assert_eq!(prompt.key(&mut cursor, MenuKey::Enter), Some(Answer::Deny));
        prompt.key(&mut cursor, MenuKey::Up);
        prompt.key(&mut cursor, MenuKey::Up);
        prompt.key(&mut cursor, MenuKey::Up);
        assert_eq!(cursor.selected, 0);
    }

    #[test]
    fn the_key_caps_and_escape_answer_and_other_keys_do_not() {
        let prompt = prompt();
        let mut cursor = Cursor::default();
        let key = |cursor: &mut Cursor, ch| prompt.key(cursor, MenuKey::Char(ch));
        assert_eq!(key(&mut cursor, 'a'), Some(Answer::Always));
        assert_eq!(key(&mut cursor, 'n'), Some(Answer::Deny));
        assert_eq!(key(&mut cursor, 'x'), None);
        let escape = prompt.key(&mut cursor, MenuKey::Escape);
        assert_eq!(escape, Some(Answer::Deny));
    }

    #[test]
    fn an_unarmed_dialog_greys_its_choices_and_ignores_the_pointer() {
        let prompt = Prompt {
            armed: false,
            ..prompt()
        };
        assert!(prompt.rows().iter().all(|row| !row.available));
        let list = prompt.layout().list_area().unwrap();
        let (x, y) = (list.left(), list.text_y(1));
        let mut cursor = Cursor::default();
        assert!(!prompt.hover(&mut cursor, x, y));
        assert_eq!(prompt.hit(x, y), None);
        assert_eq!(prompt.key(&mut cursor, MenuKey::Char('y')), None);
    }

    #[test]
    fn hovering_an_armed_choice_selects_it() {
        let prompt = prompt();
        let list = prompt.layout().list_area().unwrap();
        let mut cursor = Cursor::default();
        assert!(prompt.hover(&mut cursor, list.left(), list.text_y(2)));
        assert_eq!(cursor.selected, 2);
    }
}
