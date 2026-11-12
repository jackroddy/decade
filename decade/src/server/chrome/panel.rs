use crate::server::chrome::draw::Scene;
use crate::server::chrome::menu::{Action, MenuKey, Outcome};
use crate::server::chrome::overlay::{Cursor, LARGE, LARGE_COLUMNS, Layout, Row, Step};
use crate::server::chrome::palette;
use crate::server::chrome::settings::{KNOBS, Settings};
use crate::server::chrome::text::fit;

/// The width in cells of the pill a value sits in.
const VALUE_WIDTH: i32 = 9;

/// The pill's height, in pixels at the large size's mockup cell.
const VALUE_HEIGHT: i32 = 30;

const HINT: [(&str, &str); 3] = [("←→", "change"), ("↑↓", "move"), ("esc", "close")];

pub struct Panel {
    cursor: Cursor,
}

fn rows() -> [Row<'static>; KNOBS.len()] {
    KNOBS.map(|knob| Row::new(knob.label()))
}

impl Panel {
    pub fn new() -> Panel {
        Panel {
            cursor: Cursor::default(),
        }
    }

    pub fn key(&mut self, key: MenuKey, settings: &Settings) -> Outcome {
        match self.cursor.key(key, &rows()) {
            Step::Pass(MenuKey::Left | MenuKey::Right) => {
                self.step(key == MenuKey::Right, settings)
            }
            Step::Close | Step::Pass(MenuKey::Backspace) => Outcome::Close,
            Step::Stay | Step::Run(_) | Step::Pass(_) => Outcome::Open,
        }
    }

    fn step(&self, up: bool, settings: &Settings) -> Outcome {
        let mut changed = *settings;
        KNOBS[self.cursor.selected].step(&mut changed, up);
        Outcome::Apply(Action::ChangeSettings(changed))
    }

    /// The panel's box in the middle of `window`, for the menu font's
    /// `cell`.
    fn layout(cell: (i32, i32), window: (i32, i32)) -> Layout {
        let width = LARGE.width(cell, LARGE_COLUMNS).min(window.0);
        (Layout::new(&LARGE, cell, width))
            .header(true)
            .list(KNOBS.len())
            .hint()
            .end()
            .centred(window)
    }

    /// The left edge of the value pill in a row whose text ends at `right`.
    fn pill_x(right: i32, cell_width: i32) -> i32 {
        right - 2 * cell_width - VALUE_WIDTH * cell_width
    }

    /// Select the setting under pixel `x`, `y`, and return whether the
    /// selection moved.
    pub fn hover(&mut self, (x, y): (i32, i32), cell: (i32, i32), window: (i32, i32)) -> bool {
        let Some(list) = Panel::layout(cell, window).list_area() else {
            return false;
        };
        self.cursor.hover(&list, &rows(), x, y)
    }

    /// What a left click at pixel `x`, `y` does, or `None` outside the box.
    /// A click on a chevron steps the setting's value.
    pub fn click(
        &mut self,
        (x, y): (i32, i32),
        settings: &Settings,
        cell: (i32, i32),
        window: (i32, i32),
    ) -> Option<Outcome> {
        let layout = Panel::layout(cell, window);
        if !layout.contains(x, y) {
            return None;
        }
        let list = layout.list_area()?;
        if self.cursor.click(&list, &rows(), x, y).is_none() {
            return Some(Outcome::Open);
        }
        let cell_width = cell.0;
        let pill_x = Panel::pill_x(list.right(), cell_width);
        Some(if (pill_x - 3 * cell_width..pill_x).contains(&x) {
            self.step(false, settings)
        } else if x >= pill_x + VALUE_WIDTH * cell_width {
            self.step(true, settings)
        } else {
            Outcome::Open
        })
    }

    /// Draw the panel in `window`, in the scene's pixels.
    pub fn draw(&self, scene: &mut Scene, settings: &Settings, window: (i32, i32)) {
        let (cell_width, cell_height) = scene.cell();
        let layout = Panel::layout((cell_width, cell_height), window);
        layout.draw_box(scene);
        layout.draw_header(scene, "Settings", Some("left and right change the value"));
        layout.draw_hint(scene, &HINT);
        let Some(list) = layout.list_area() else {
            return;
        };
        let pill_width = VALUE_WIDTH * cell_width;
        let pill_height = LARGE.px(VALUE_HEIGHT, cell_height);
        let pill_x = Panel::pill_x(list.right(), cell_width);
        for (i, (knob, row)) in KNOBS.iter().zip(rows()).enumerate() {
            let selected = i == self.cursor.selected;
            list.draw_row(scene, i, &row, selected);
            let y = list.text_y(i);
            let (chevron, pill) = if selected {
                (palette::ACCENT, palette::BACKGROUND)
            } else {
                (palette::MUTED, palette::DARK_BACKGROUND)
            };
            scene.text_px(pill_x - 2 * cell_width, y, "‹", LARGE.face, chevron);
            scene.text_px(list.right() - cell_width, y, "›", LARGE.face, chevron);
            let pill_y = list.centred(i, pill_height);
            let size = (pill_width, pill_height);
            scene.rounded(pill_x, pill_y, size, pill_height / 2, pill, pill);
            let value = fit(&knob.show(settings), VALUE_WIDTH as usize - 2);
            let value_x = pill_x + (pill_width - value.chars().count() as i32 * cell_width) / 2;
            scene.text_px(value_x, y, &value, LARGE.face, palette::FOREGROUND);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (i32, i32) = (10, 25);
    const WINDOW: (i32, i32) = (1200, 800);

    #[test]
    fn left_and_right_change_the_selected_setting() {
        let settings = Settings::default();
        let mut panel = Panel::new();
        panel.key(MenuKey::Down, &settings);
        let Outcome::Apply(Action::ChangeSettings(changed)) = panel.key(MenuKey::Right, &settings)
        else {
            panic!("right did not change a setting");
        };
        assert_eq!(changed.palette_size, 21.0);
        assert_eq!(panel.key(MenuKey::Enter, &changed), Outcome::Open);
        assert_eq!(panel.key(MenuKey::Escape, &changed), Outcome::Close);
    }

    #[test]
    fn hovering_selects_a_setting_and_a_click_on_a_chevron_steps_it() {
        let settings = Settings::default();
        let mut panel = Panel::new();
        let list = Panel::layout(CELL, WINDOW).list_area().unwrap();
        let y = list.text_y(2) + 1;
        assert!(panel.hover((list.left() + 1, y), CELL, WINDOW));
        assert_eq!(panel.cursor.selected, 2);
        let label = (list.left() + 1, list.text_y(3) + 1);
        assert_eq!(
            panel.click(label, &settings, CELL, WINDOW),
            Some(Outcome::Open)
        );
        assert_eq!(panel.cursor.selected, 3);
        let pill_x = Panel::pill_x(list.right(), CELL.0);
        let Some(Outcome::Apply(Action::ChangeSettings(down))) =
            panel.click((pill_x - 15, y), &settings, CELL, WINDOW)
        else {
            panic!("the left chevron did not change the setting");
        };
        assert_eq!(down.bar_hold_ms, settings.bar_hold_ms - 10);
        let right = (list.right() - 5, y);
        let Some(Outcome::Apply(Action::ChangeSettings(up))) =
            panel.click(right, &settings, CELL, WINDOW)
        else {
            panic!("the right chevron did not change the setting");
        };
        assert_eq!(up.bar_hold_ms, settings.bar_hold_ms + 10);
        assert_eq!(panel.click((0, 0), &settings, CELL, WINDOW), None);
    }
}
