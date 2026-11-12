use crate::server::chrome::draw::Scene;
use crate::server::chrome::menu::{Action, MenuKey, Outcome};
use crate::server::chrome::overlay::{Layout, SMALL, hint_width};

/// The fewest cells of text the field shows.
const MIN_ROOM: i32 = 12;

const HINT: [(&str, &str); 2] = [("enter", "save"), ("esc", "cancel")];

/// Something the person can rename.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Named {
    Workspace(u32),

    /// A tab of the workspace on screen, by index.
    Tab(usize),

    /// A pane of the tab on screen, by id.
    Pane(usize),

    /// A group in the sidebar, by id.
    Group(u32),

    /// The group the field adds once a name is typed in it.
    NewGroup,
}

impl Named {
    fn title(self) -> &'static str {
        match self {
            Named::Workspace(_) => "Rename workspace",
            Named::Tab(_) => "Rename tab",
            Named::Pane(_) => "Rename pane",
            Named::Group(_) => "Rename group",
            Named::NewGroup => "New group",
        }
    }
}

/// The box a name turns into while the person renames it, with its text
/// field where the name is drawn.
pub struct NameField {
    pub named: Named,

    /// The tab that was on screen when the field opened.
    pub tab: usize,
    pub text: String,

    /// Whether the text is still the old name, which the first key
    /// replaces.
    fresh: bool,

    /// Why the name last entered was refused, until the next key.
    pub error: Option<String>,

    /// Where the name's text starts, in window pixels.
    at: (i32, i32),

    /// The room for the text, in cells.
    room: i32,

    /// The chrome font's cell.
    cell: (i32, i32),

    /// The window's size in pixels.
    window: (i32, i32),
}

impl NameField {
    pub fn open(
        named: Named,
        tab: usize,
        name: String,
        at: (i32, i32),
        room: i32,
        cell: (i32, i32),
        window: (i32, i32),
    ) -> NameField {
        NameField {
            named,
            tab,
            text: name,
            fresh: true,
            error: None,
            at,
            room: room.max(MIN_ROOM),
            cell,
            window,
        }
    }

    pub fn key(&mut self, key: MenuKey) -> Outcome {
        match key {
            MenuKey::Enter => return Outcome::Apply(Action::Rename),
            MenuKey::Escape => return Outcome::Close,
            MenuKey::Char(ch) => {
                if self.fresh {
                    self.text.clear();
                }
                self.text.push(ch);
            }
            MenuKey::Backspace if self.fresh => self.text.clear(),
            MenuKey::Backspace => {
                self.text.pop();
            }
            MenuKey::Up | MenuKey::Down | MenuKey::Left | MenuKey::Right | MenuKey::Tab => {
                return Outcome::Open;
            }
        }
        self.fresh = false;
        self.error = None;
        Outcome::Open
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
            .max(self.named.title().chars().count() as i32)
            .max(error as i32)
            .max(hint_width(&SMALL, &HINT, self.cell) / cell_width + 1);
        let layout = (Layout::new(&SMALL, self.cell, SMALL.width(self.cell, columns)))
            .header(false)
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

    /// Draw the box in window pixels, with the chrome font.
    pub fn draw(&self, scene: &mut Scene) {
        let layout = self.layout();
        layout.draw_box(scene);
        layout.draw_header(scene, self.named.title(), None);
        layout.draw_field(scene, &self.text, "name", self.fresh);
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

    fn field() -> NameField {
        NameField::open(
            Named::Pane(3),
            0,
            "terminal".into(),
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
    fn enter_asks_to_apply_the_name_escape_cancels_and_a_key_clears_the_error() {
        let mut field = field();
        assert_eq!(field.key(MenuKey::Enter), Outcome::Apply(Action::Rename));
        field.error = Some("terminal is taken".into());
        field.key(MenuKey::Left);
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
        let corner = NameField::open(Named::Tab(0), 0, "a".into(), (10, 5), 4, CELL, WINDOW);
        assert_eq!(corner.layout().at, (0, 0));
    }

    #[test]
    fn the_title_names_what_is_renamed() {
        assert_eq!(Named::Pane(3).title(), "Rename pane");
        assert_eq!(Named::NewGroup.title(), "New group");
    }
}
