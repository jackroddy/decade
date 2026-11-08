use smithay::utils::{Physical, Size};

use crate::draw::{Face, Scene};
use crate::menu::{
    Action, BORDER, BOX, DIM, MenuKey, Outcome, SELECTED, SELECTED_TEXT, TEXT, WIDTH,
};
use crate::settings::{KNOBS, Settings};
use crate::text::Font;

/// The width in cells of a value between its chevrons.
const VALUE_WIDTH: usize = 7;

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

    pub fn draw(&self, scene: &mut Scene, font: &Font, settings: &Settings) -> Size<i32, Physical> {
        let (cell_width, cell_height) = (font.cell_width as i32, font.cell_height as i32);
        let pad = cell_height * 2 / 3;
        let row_height = cell_height * 8 / 5;
        let width = WIDTH * cell_width + pad * 2;
        let list = pad + row_height + pad / 2;
        let height = list + row_height * (KNOBS.len() as i32 + 1) + pad;
        scene.rounded(0, 0, (width, height), pad, BOX, BORDER);
        let text_y = |top: i32| top + (row_height - cell_height) / 2;
        scene.text_px(pad, text_y(pad), "Settings", Face::Menu, BORDER);
        let line = scene.line_width();
        let rule = pad + row_height + pad / 4 - line / 2;
        scene.rect(pad, rule, width - pad * 2, line, DIM);
        for (i, knob) in KNOBS.iter().enumerate() {
            let top = list + i as i32 * row_height;
            let fg = if i == self.selected {
                let size = (width - pad, row_height);
                scene.rounded(pad / 2, top, size, pad / 2, SELECTED, SELECTED);
                SELECTED_TEXT
            } else {
                TEXT
            };
            scene.text_px(pad, text_y(top), knob.label(), Face::Menu, fg);
            let value = format!("‹ {:^VALUE_WIDTH$} ›", knob.show(settings));
            let x = width - pad - (VALUE_WIDTH as i32 + 4) * cell_width;
            scene.text_px(x, text_y(top), &value, Face::Menu, fg);
        }
        let hint = "←/→ change · esc close";
        let top = list + KNOBS.len() as i32 * row_height;
        scene.text_px(pad, text_y(top), hint, Face::Menu, DIM);
        (width, height).into()
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
