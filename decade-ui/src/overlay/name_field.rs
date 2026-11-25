use crate::Scene;
use crate::overlay::{Layout, MenuKey, Outcome, SMALL, hint_width};

/// The fewest cells of text the field shows.
const MIN_ROOM: i32 = 12;

const HINT: [(&str, &str); 2] = [("enter", "save"), ("esc", "cancel")];

/// The box a name turns into while the person renames it, with its text
/// field where the name is drawn, and `A` the action Enter applies.
pub struct NameField<A> {
    title: String,
    pub text: String,

    /// The caret's place in the text, in characters.
    cursor: usize,

    /// Whether the text is still the old name, which the first key
    /// replaces.
    fresh: bool,

    /// Why the name last entered was refused, until the next key.
    pub error: Option<String>,
    apply: A,

    /// Where the name's text starts, in window pixels.
    at: (i32, i32),

    /// The room for the text, in cells.
    room: i32,

    /// The small font's cell.
    cell: (i32, i32),

    /// The window's size in pixels.
    window: (i32, i32),
}

impl<A: Clone> NameField<A> {
    pub fn open(
        title: String,
        name: String,
        apply: A,
        at: (i32, i32),
        room: i32,
        cell: (i32, i32),
        window: (i32, i32),
    ) -> NameField<A> {
        NameField {
            title,
            cursor: name.chars().count(),
            text: name,
            fresh: true,
            error: None,
            apply,
            at,
            room: room.max(MIN_ROOM),
            cell,
            window,
        }
    }

    pub fn key(&mut self, key: MenuKey) -> Outcome<A> {
        match key {
            MenuKey::Enter => return Outcome::Apply(self.apply.clone()),
            MenuKey::Escape => return Outcome::Close,
            MenuKey::Char(ch) => {
                if self.fresh {
                    self.text.clear();
                    self.cursor = 0;
                }
                self.text.insert(self.byte_at(self.cursor), ch);
                self.cursor += 1;
            }
            MenuKey::Backspace if self.fresh => {
                self.text.clear();
                self.cursor = 0;
            }
            MenuKey::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.byte_at(self.cursor));
                }
            }
            MenuKey::Left => self.cursor = self.cursor.saturating_sub(1),
            MenuKey::Right => self.cursor = (self.cursor + 1).min(self.text.chars().count()),
            MenuKey::Home => self.cursor = 0,
            MenuKey::End => self.cursor = self.text.chars().count(),
            MenuKey::Up | MenuKey::Down | MenuKey::Tab => return Outcome::Open,
        }
        self.fresh = false;
        self.error = None;
        Outcome::Open
    }

    fn byte_at(&self, chars: usize) -> usize {
        (self.text.char_indices())
            .nth(chars)
            .map_or(self.text.len(), |(byte, _)| byte)
    }

    /// The box, placed so the field's text starts where the name does
    /// when the window has room.
    fn layout(&self) -> Layout {
        let cell_width = self.cell.0;
        let error = self
            .error
            .as_deref()
            .map_or(0, |error| error.chars().count());
        let columns = (self.room + 2)
            .max(self.text.chars().count() as i32 + 3)
            .max(self.title.chars().count() as i32)
            .max(error as i32)
            .max(hint_width(&SMALL, &HINT, self.cell) / cell_width + 1);
        let layout = (Layout::new(&SMALL, self.cell, SMALL.width(self.cell, columns)))
            .header(0)
            .field()
            .error(self.error.is_some())
            .hint()
            .end();
        let (x, y) = layout.field_text();
        layout.inside((self.at.0 - x, self.at.1 - y), self.window)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.layout().contains(x, y)
    }

    /// Draw the box in window pixels, with the small font.
    pub fn draw(&self, scene: &mut Scene) {
        let layout = self.layout();
        layout.draw_box(scene);
        layout.draw_header(scene, &self.title, &[]);
        layout.draw_field(scene, &self.text, "name", self.fresh, self.cursor);
        if let Some(error) = &self.error {
            layout.draw_error(scene, error);
        }
        layout.draw_hint(scene, &HINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (i32, i32) = (8, 18);
    const WINDOW: (i32, i32) = (800, 600);

    fn field() -> NameField<u32> {
        NameField::open(
            "Rename pane".into(),
            "terminal".into(),
            7,
            (100, 50),
            20,
            CELL,
            WINDOW,
        )
    }

    #[test]
    fn the_first_key_replaces_the_old_name_and_later_keys_edit_it() {
        let mut field = field();
        assert_eq!(field.key(MenuKey::Char('b')), Outcome::Open);
        assert_eq!(field.text, "b");
        field.key(MenuKey::Char('x'));
        field.key(MenuKey::Backspace);
        field.key(MenuKey::Char('u'));
        assert_eq!(field.text, "bu");

        let mut field = self::field();
        field.key(MenuKey::Backspace);
        assert_eq!(field.text, "");
    }

    #[test]
    fn movement_keys_move_the_caret_and_typing_and_backspace_act_at_it() {
        let mut field = field();
        field.key(MenuKey::Home);
        assert_eq!(field.text, "terminal");
        field.key(MenuKey::Char('x'));
        assert_eq!(field.text, "xterminal");
        field.key(MenuKey::Right);
        field.key(MenuKey::Backspace);
        assert_eq!(field.text, "xerminal");
        field.key(MenuKey::Backspace);
        field.key(MenuKey::Backspace);
        assert_eq!(field.text, "erminal");
        field.key(MenuKey::End);
        field.key(MenuKey::Right);
        field.key(MenuKey::Char('é'));
        field.key(MenuKey::Left);
        field.key(MenuKey::Left);
        field.key(MenuKey::Backspace);
        assert_eq!(field.text, "erminl\u{e9}");
    }

    #[test]
    fn a_movement_key_first_keeps_the_old_name_and_left_starts_from_its_end() {
        let mut field = field();
        field.key(MenuKey::Left);
        field.key(MenuKey::Char('X'));
        assert_eq!(field.text, "terminaXl");
    }

    #[test]
    fn enter_asks_to_apply_the_name_escape_cancels_and_a_key_clears_the_error() {
        let mut field = field();
        assert_eq!(field.key(MenuKey::Enter), Outcome::Apply(7));
        field.error = Some("terminal is taken".into());
        field.key(MenuKey::Up);
        assert!(field.error.is_some());
        field.key(MenuKey::Char('2'));
        assert_eq!(field.error, None);
        assert_eq!(field.key(MenuKey::Escape), Outcome::Close);
    }

    #[test]
    fn the_fields_text_starts_where_the_name_did_and_the_box_grows_with_the_text() {
        let mut field = field();
        let layout = field.layout();
        let (x, y) = layout.field_text();
        assert_eq!((layout.at.0 + x, layout.at.1 + y), (100, 50));
        assert!(field.contains(100, 50));
        assert!(layout.width < SMALL.width(CELL, 33));
        for _ in 0..30 {
            field.key(MenuKey::Char('a'));
        }

        // the "› " before the text and the caret after it
        assert_eq!(field.layout().width, SMALL.width(CELL, 30 + 3));
    }

    #[test]
    fn an_error_adds_a_row_under_the_field_and_the_box_stays_in_the_window() {
        let mut field = field();
        let height = field.layout().height;
        field.error = Some("terminal is taken".into());
        assert_eq!(field.layout().height, height + 5 + 22);
        let corner = NameField::open("Rename tab".into(), "a".into(), 0, (10, 5), 4, CELL, WINDOW);
        assert_eq!(corner.layout().at, (0, 0));
    }
}
