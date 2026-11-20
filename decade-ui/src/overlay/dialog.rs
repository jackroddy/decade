use crate::overlay::{Cursor, LARGE, LARGE_COLUMNS, Layout, MenuKey, Row, Step};
use crate::{Scene, fit};

/// A choice of a dialog, with `A` the answer it gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Choice<A> {
    pub answer: A,
    pub label: &'static str,

    /// The key that gives the answer, shown in a key cap.
    pub key: char,
    pub destructive: bool,
}

/// The dialog in the middle of the window that asks the person a
/// question, with `A` the answers its choices give.
pub struct Dialog<'a, A> {
    pub question: String,

    /// The dim line under the question.
    pub detail: String,
    pub choices: &'a [Choice<A>],

    /// The answer Escape gives.
    pub escape: A,

    /// Whether the dialog has been up long enough to take an answer.
    pub armed: bool,

    /// The choice the dialog shows selected.
    pub selected: usize,

    /// The window's size in pixels.
    pub window: (i32, i32),

    /// The large font's cell.
    pub cell: (i32, i32),
}

impl<A: Copy> Dialog<'_, A> {
    fn rows(&self) -> Vec<Row<'static>> {
        (self.choices.iter())
            .map(|choice| Row {
                key: Some(choice.key),
                destructive: choice.destructive,
                available: self.armed,
                ..Row::new(choice.label)
            })
            .collect()
    }

    fn layout(&self) -> Layout {
        let width = LARGE.width(self.cell, LARGE_COLUMNS).min(self.window.0);
        (Layout::new(&LARGE, self.cell, width))
            .header(true)
            .list(self.choices.len())
            .end()
            .centred(self.window)
    }

    /// The answer `key` gives, moving `cursor` for Up and Down.
    pub fn key(&self, cursor: &mut Cursor, key: MenuKey) -> Option<A> {
        match cursor.key(key, &self.rows()) {
            Step::Run(index) => Some(self.choices[index].answer),
            Step::Close => Some(self.escape),
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

    /// The answer of the choice under pixel `x`, `y`, if any.
    pub fn hit(&self, x: i32, y: i32) -> Option<A> {
        let list = self.layout().list_area()?;
        let mut cursor = Cursor::default();
        let index = cursor.click(&list, &self.rows(), x, y)?;
        Some(self.choices[index].answer)
    }

    /// Draw the dialog in window pixels, with the large font.
    pub fn draw(&self, scene: &mut Scene) {
        let layout = self.layout();
        layout.draw_box(scene);
        let (cell_width, _) = self.cell;
        let room = ((layout.width - 2 * LARGE.text_left(self.cell.1)) / cell_width).max(0);
        let room = room as usize;
        let question = fit(&self.question, room);
        layout.draw_header(scene, &question, Some(&fit(&self.detail, room)));
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

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Answer {
        Save,
        Discard,
        Cancel,
    }

    const CHOICES: [Choice<Answer>; 3] = [
        Choice {
            answer: Answer::Save,
            label: "Save",
            key: 's',
            destructive: false,
        },
        Choice {
            answer: Answer::Discard,
            label: "Discard",
            key: 'd',
            destructive: true,
        },
        Choice {
            answer: Answer::Cancel,
            label: "Cancel",
            key: 'c',
            destructive: false,
        },
    ];

    fn dialog() -> Dialog<'static, Answer> {
        Dialog {
            question: "Save the changes?".to_string(),
            detail: "to notes.txt".to_string(),
            choices: &CHOICES,
            escape: Answer::Cancel,
            armed: true,
            selected: 0,
            window: (800, 600),
            cell: (10, 25),
        }
    }

    #[test]
    fn the_dialog_is_centred_in_the_window_and_a_press_on_a_choice_answers() {
        let dialog = dialog();
        let layout = dialog.layout();
        let ((left, top), (width, height)) = (layout.at, (layout.width, layout.height));
        assert!((left - (800 - (left + width))).abs() <= 1);
        assert!((top - (600 - (top + height))).abs() <= 1);
        let list = layout.list_area().unwrap();
        for (i, choice) in CHOICES.into_iter().enumerate() {
            assert_eq!(dialog.hit(list.left(), list.text_y(i)), Some(choice.answer));
        }
        assert_eq!(dialog.hit(left + 1, top + 1), None);
        assert_eq!(dialog.hit(left - 1, list.text_y(0)), None);
    }

    #[test]
    fn the_dialog_is_no_wider_than_the_window() {
        let dialog = Dialog {
            window: (120, 600),
            ..dialog()
        };
        let layout = dialog.layout();
        assert_eq!((layout.at.0, layout.width), (0, 120));
    }

    #[test]
    fn arrows_move_the_selection_and_enter_gives_the_selected_answer() {
        let dialog = dialog();
        let mut cursor = Cursor::default();
        assert_eq!(dialog.key(&mut cursor, MenuKey::Enter), Some(Answer::Save));
        assert_eq!(dialog.key(&mut cursor, MenuKey::Down), None);
        assert_eq!(
            dialog.key(&mut cursor, MenuKey::Enter),
            Some(Answer::Discard)
        );
        dialog.key(&mut cursor, MenuKey::Down);
        dialog.key(&mut cursor, MenuKey::Down);
        assert_eq!(
            dialog.key(&mut cursor, MenuKey::Enter),
            Some(Answer::Cancel)
        );
        dialog.key(&mut cursor, MenuKey::Up);
        dialog.key(&mut cursor, MenuKey::Up);
        dialog.key(&mut cursor, MenuKey::Up);
        assert_eq!(cursor.selected, 0);
    }

    #[test]
    fn the_key_caps_and_escape_answer_and_other_keys_do_not() {
        let dialog = Dialog {
            escape: Answer::Discard,
            ..dialog()
        };
        let mut cursor = Cursor::default();
        let key = |cursor: &mut Cursor, ch| dialog.key(cursor, MenuKey::Char(ch));
        assert_eq!(key(&mut cursor, 'c'), Some(Answer::Cancel));
        assert_eq!(key(&mut cursor, 'S'), Some(Answer::Save));
        assert_eq!(key(&mut cursor, 'x'), None);
        let escape = dialog.key(&mut cursor, MenuKey::Escape);
        assert_eq!(escape, Some(Answer::Discard));
    }

    #[test]
    fn an_unarmed_dialog_greys_its_choices_and_ignores_the_pointer() {
        let dialog = Dialog {
            armed: false,
            ..dialog()
        };
        assert!(dialog.rows().iter().all(|row| !row.available));
        let list = dialog.layout().list_area().unwrap();
        let (x, y) = (list.left(), list.text_y(1));
        let mut cursor = Cursor::default();
        assert!(!dialog.hover(&mut cursor, x, y));
        assert_eq!(dialog.hit(x, y), None);
        assert_eq!(dialog.key(&mut cursor, MenuKey::Char('s')), None);
    }

    #[test]
    fn hovering_an_armed_choice_selects_it() {
        let dialog = dialog();
        let list = dialog.layout().list_area().unwrap();
        let mut cursor = Cursor::default();
        assert!(dialog.hover(&mut cursor, list.left(), list.text_y(2)));
        assert_eq!(cursor.selected, 2);
    }
}
