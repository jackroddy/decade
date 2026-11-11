use crate::server::chrome::draw::{Face, Scene, scaled};
use crate::server::chrome::frame::{CARD_RADIUS, POPOVER_PADDING};
use crate::server::chrome::menu::{Action, MenuKey, Outcome};
use crate::server::chrome::palette;

/// The fewest cells of text the field shows.
const MIN_ROOM: i32 = 12;

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

/// The text field a name turns into while the person renames it, drawn
/// where the name is drawn.
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
}

impl NameField {
    pub fn open(
        named: Named,
        tab: usize,
        name: String,
        at: (i32, i32),
        room: i32,
        cell: (i32, i32),
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

    /// The field's box: its top left and size, in window pixels.
    fn bounds(&self) -> ((i32, i32), (i32, i32)) {
        let (cell_width, cell_height) = self.cell;
        let padding = scaled(POPOVER_PADDING, cell_height);
        let cells = self.room.max(self.text.chars().count() as i32 + 1);
        let at = (self.at.0 - cell_width / 2, self.at.1 - padding);
        let size = (cells * cell_width + cell_width, cell_height + 2 * padding);
        (at, size)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        let ((left, top), (width, height)) = self.bounds();
        (left..left + width).contains(&x) && (top..top + height).contains(&y)
    }

    /// Draw the field in window pixels, with the chrome font.
    pub fn draw(&self, scene: &mut Scene) {
        let (cell_width, cell_height) = self.cell;
        let ((left, top), size) = self.bounds();
        let radius = scaled(CARD_RADIUS, cell_height);
        let fill = palette::LIGHTER_BACKGROUND;
        scene.rounded(left, top, size, radius, fill, palette::ACCENT);
        let (x, y) = self.at;
        let width = self.text.chars().count() as i32 * cell_width;
        if self.fresh && width > 0 {
            scene.rect(x, y, width, cell_height, palette::SELECTION);
        } else {
            scene.rect(
                x + width,
                y,
                scene.line_width() * 2,
                cell_height,
                palette::FOREGROUND,
            );
        }
        scene.text_px(x, y, &self.text, Face::ChromeBold, palette::FOREGROUND);
        if let Some(error) = &self.error {
            let padding = scaled(POPOVER_PADDING, cell_height);
            let below = top + size.1 + padding;
            let width = (error.chars().count() as i32 + 1) * cell_width;
            let size = (width, cell_height + 2 * padding);
            let fill = palette::DARK_RED;
            scene.rounded(left, below, size, radius, fill, palette::RED);
            let text_x = left + cell_width / 2;
            scene.text_px(
                text_x,
                below + padding,
                error,
                Face::Chrome,
                palette::LIGHT_RED,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field() -> NameField {
        NameField::open(Named::Pane(3), 0, "terminal".into(), (100, 50), 20, (8, 18))
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
    fn the_field_sits_round_the_name_and_grows_with_the_text() {
        let mut field = field();
        assert!(field.contains(100, 50));
        assert!(field.contains(100 + 20 * 8, 50));
        assert!(!field.contains(100 + 22 * 8, 50));
        for _ in 0..30 {
            field.key(MenuKey::Char('a'));
        }
        assert!(field.contains(100 + 30 * 8, 50));
    }
}
