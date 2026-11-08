use smithay::utils::{Physical, Size};

use crate::draw::{Face, Scene};
use crate::menu::{Action, MenuKey, Outcome, draw_box};
use crate::settings::{KNOBS, Settings};

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

    pub fn draw(&self, scene: &mut Scene, settings: &Settings) -> Size<i32, Physical> {
        let hint = "←/→ change · esc close";
        let (rows, size) = draw_box(scene, &["Settings"], KNOBS.len(), hint);
        let (cell_width, _) = scene.cell();
        let value_x = rows.right - (VALUE_WIDTH as i32 + 4) * cell_width;
        for (i, knob) in KNOBS.iter().enumerate() {
            let fg = rows.select(scene, i, i == self.selected);
            let y = rows.text_y(i);
            scene.text_px(rows.left, y, knob.label(), Face::Menu, fg);
            let value = format!("‹ {:^VALUE_WIDTH$} ›", knob.show(settings));
            scene.text_px(value_x, y, &value, Face::Menu, fg);
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
