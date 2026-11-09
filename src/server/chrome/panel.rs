use smithay::utils::{Physical, Size};

use crate::server::chrome::draw::{Face, Scene};
use crate::server::chrome::menu::{Action, MenuKey, Outcome, draw_box, scaled};
use crate::server::chrome::palette;
use crate::server::chrome::settings::{KNOBS, Settings};
use crate::server::chrome::text::fit;

/// The width in cells of the pill a value sits in.
const VALUE_WIDTH: i32 = 9;

// the sizes below are in pixels at the palette's mockup cell, for `scaled`
const VALUE_HEIGHT: i32 = 30;
const VALUE_RADIUS: i32 = 6;

pub struct Panel {
    selected: usize,
}

impl Panel {
    pub fn new() -> Panel {
        Panel { selected: 0 }
    }

    pub fn key(&mut self, key: MenuKey, settings: &Settings) -> Outcome {
        match key {
            MenuKey::Up => self.selected = self.selected.saturating_sub(1),
            MenuKey::Down => self.selected = (self.selected + 1).min(KNOBS.len() - 1),
            MenuKey::Left | MenuKey::Right => {
                let mut changed = *settings;
                KNOBS[self.selected].step(&mut changed, key == MenuKey::Right);
                return Outcome::Apply(Action::ChangeSettings(changed));
            }
            MenuKey::Escape | MenuKey::Backspace => return Outcome::Close,
            MenuKey::Enter | MenuKey::Tab | MenuKey::Char(_) => {}
        }
        Outcome::Open
    }

    pub fn draw(&self, scene: &mut Scene, settings: &Settings) -> Size<i32, Physical> {
        let hint = [("← →", "change"), ("esc", "close")];
        let (rows, size) = draw_box(scene, &["Settings"], None, KNOBS.len(), &hint);
        let (cell_width, cell_height) = scene.cell();
        let pill_width = VALUE_WIDTH * cell_width;
        let pill_height = scaled(VALUE_HEIGHT, cell_height);
        let radius = scaled(VALUE_RADIUS, cell_height);
        let pill_x = rows.right - 2 * cell_width - pill_width;
        let pill = palette::RAISED_BACKGROUND;
        for (i, knob) in KNOBS.iter().enumerate() {
            let selected = i == self.selected;
            let style = rows.select(scene, i, selected, false);
            let y = rows.text_y(i);
            scene.text_px(rows.left, y, knob.label(), style.face, style.text);
            let chevron = if selected {
                palette::ACCENT
            } else {
                palette::MUTED
            };
            scene.text_px(pill_x - 2 * cell_width, y, "‹", Face::Menu, chevron);
            scene.text_px(rows.right - cell_width, y, "›", Face::Menu, chevron);
            let pill_y = rows.centred(i, pill_height);
            scene.rounded(
                pill_x,
                pill_y,
                (pill_width, pill_height),
                radius,
                pill,
                pill,
            );
            let value = fit(&knob.show(settings), VALUE_WIDTH as usize - 2);
            let value_x = pill_x + (pill_width - value.chars().count() as i32 * cell_width) / 2;
            scene.text_px(value_x, y, &value, Face::Menu, style.text);
        }
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(panel.key(MenuKey::Escape, &changed), Outcome::Close);
    }
}
