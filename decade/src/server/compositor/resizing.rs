use crate::server::mcp;

use super::State;
use super::screenshot::Asker;

/// A resize of a scratch pane waiting on the person's answer.
pub(super) struct Resize {
    pub reply: mcp::Reply,

    /// The size of the pane's client area asked for, in pixels.
    pub size: (u32, u32),

    /// The size of the pane's client area when the agent asked, in pixels.
    pub from: (i32, i32),
}

impl State {
    /// Ask the person whether the scratch pane `pane` names may be
    /// resized to `size` pixels, and resize it if they allow it.
    pub(super) fn scratch_resize(
        &mut self,
        workspace: usize,
        pane: Option<&str>,
        size: (u32, u32),
        caller: Option<&str>,
        reply: mcp::Reply,
    ) {
        if size.0 == 0 || size.1 == 0 {
            let reason = format!(
                "width and height are pixels above 0, not {}x{}",
                size.0, size.1
            );
            let _ = reply.send(Err(reason));
            return;
        }
        match self.resize_target(workspace, pane) {
            Ok((name, from)) => {
                let resize = Resize { reply, size, from };
                self.ask(workspace, name, caller, Asker::Resize(resize));
            }
            Err(reason) => {
                let _ = reply.send(Err(reason));
            }
        }
    }

    /// The scratch pane's own name and the size of its client area in
    /// pixels, or why it cannot be resized.
    fn resize_target(
        &self,
        workspace: usize,
        pane: Option<&str>,
    ) -> Result<(String, (i32, i32)), String> {
        if self.asking.is_some() {
            return Err("decade is already asking the person about another capture".to_string());
        }
        let shown = &self.workspaces[workspace];
        let index = shown.find_scratch(pane)?;
        let (name, rect) = self.client_px(workspace, &shown.scratch[index].pane)?;
        if shown.layout().zoom.is_some() {
            return Err(format!(
                "a pane is zoomed in pane {name}'s tab, so decade cannot resize it"
            ));
        }
        Ok((name, (rect.size.w, rect.size.h)))
    }

    /// Resize pane `pane`, which the person allowed, and answer with the
    /// size it reached.
    pub(super) fn resize_scratch(&mut self, workspace: usize, pane: &str, resize: Resize) {
        let answer = self.set_pane_size(workspace, pane, resize.size);
        let _ = resize.reply.send(answer.map(mcp::Answer::Text));
    }

    /// Resize pane `pane` to `size` pixels, or as close as the layout
    /// allows, and say the size it reached.
    fn set_pane_size(
        &mut self,
        workspace: usize,
        pane: &str,
        size: (u32, u32),
    ) -> Result<String, String> {
        let (name, _) = self.resize_target(workspace, Some(pane))?;
        let font = &self.text.fonts.chrome;
        let cell = (font.cell_width, font.cell_height);
        let layout = self.workspaces[workspace].layout_mut();
        let id = layout
            .by_name(&name)
            .ok_or_else(|| format!("no pane named {name}"))?;
        let Some(found) = layout.pane(id) else {
            return Err(format!("no pane named {name}"));
        };
        let (rect, client) = (found.rect, found.client_rect());
        let border = (rect.columns - client.columns, rect.rows - client.rows);
        layout.resize_to(id, pane_cells(size, cell, border));
        self.relayout_workspace(workspace);
        let (_, reached) = self.client_px(workspace, &name)?;
        let (width, height) = (reached.size.w, reached.size.h);
        if (width as u32, height as u32) == size {
            return Ok(format!("pane {name} is {width}x{height} pixels"));
        }
        Ok(format!(
            "pane {name} is {width}x{height} pixels, as close to {}x{} as the layout allows",
            size.0, size.1
        ))
    }
}

/// The columns and rows of a pane whose client area is closest to `size`
/// pixels, on a grid of `cell` pixels, when the pane's border and tab
/// strip take `border` columns and rows.
fn pane_cells(size: (u32, u32), cell: (u32, u32), border: (u32, u32)) -> (u32, u32) {
    let cells = |pixels: u32, cell: u32| ((pixels + cell / 2) / cell).max(1);
    (
        cells(size.0, cell.0) + border.0,
        cells(size.1, cell.1) + border.1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_in_pixels_rounds_to_the_nearest_cells_and_adds_the_border() {
        assert_eq!(pane_cells((800, 600), (8, 18), (2, 2)), (102, 35));
        assert_eq!(pane_cells((803, 608), (8, 18), (2, 2)), (102, 36));
        assert_eq!(pane_cells((804, 609), (8, 18), (2, 3)), (103, 37));
        assert_eq!(pane_cells((1, 1), (8, 18), (2, 2)), (3, 3));
    }
}
