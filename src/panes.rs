use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::grid::{Grid, Rgba};

pub const BACKGROUND: Rgba = [0.07, 0.07, 0.09, 1.0];
pub const FOREGROUND: Rgba = [0.80, 0.80, 0.84, 1.0];
const PANE: Rgba = [0.11, 0.11, 0.14, 1.0];
const TITLE: Rgba = [0.20, 0.22, 0.30, 1.0];
const FOCUSED_TITLE: Rgba = [0.32, 0.40, 0.60, 1.0];
const STATUS: Rgba = [0.16, 0.18, 0.24, 1.0];

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    pub column: u32,
    pub row: u32,
    pub columns: u32,
    pub rows: u32,
}

impl CellRect {
    fn contains(&self, column: u32, row: u32) -> bool {
        column >= self.column
            && column < self.column + self.columns
            && row >= self.row
            && row < self.row + self.rows
    }

    fn center(&self) -> (i64, i64) {
        (
            (self.column * 2 + self.columns) as i64,
            (self.row * 2 + self.rows) as i64,
        )
    }
}

pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

pub struct Pane {
    pub title: &'static str,
    pub app_id: Option<&'static str>,
    pub rect: CellRect,
    pub toplevel: Option<ToplevelSurface>,
}

impl Pane {
    pub fn client_rect(&self) -> CellRect {
        CellRect {
            column: self.rect.column,
            row: self.rect.row + 1,
            columns: self.rect.columns,
            rows: self.rect.rows.saturating_sub(1),
        }
    }
}

pub struct Layout {
    pub columns: u32,
    pub rows: u32,
    pub panes: Vec<Pane>,
    pub focused: usize,
}

impl Layout {
    pub fn new(columns: u32, rows: u32) -> Layout {
        let empty = CellRect {
            column: 0,
            row: 0,
            columns: 0,
            rows: 0,
        };
        let mut layout = Layout {
            columns,
            rows,
            panes: vec![
                Pane {
                    title: "chat",
                    app_id: Some("decade.chat"),
                    rect: empty,
                    toplevel: None,
                },
                Pane {
                    title: "terminal",
                    app_id: Some("decade.terminal"),
                    rect: empty,
                    toplevel: None,
                },
                Pane {
                    title: "code",
                    app_id: None,
                    rect: empty,
                    toplevel: None,
                },
            ],
            focused: 1,
        };
        layout.resize(columns, rows);
        layout
    }

    pub fn resize(&mut self, columns: u32, rows: u32) {
        self.columns = columns;
        self.rows = rows;
        if columns < 8 || rows < 8 {
            for pane in &mut self.panes {
                pane.rect.columns = 0;
                pane.rect.rows = 0;
            }
            return;
        }
        let body_rows = rows - 2;
        let left_columns = columns / 2;
        let right_column = left_columns + 1;
        let right_columns = columns - right_column;
        let top_rows = body_rows / 2;
        let bottom_row = top_rows + 1;
        let bottom_rows = body_rows - bottom_row;
        self.panes[0].rect = CellRect {
            column: 0,
            row: 0,
            columns: left_columns,
            rows: body_rows,
        };
        self.panes[1].rect = CellRect {
            column: right_column,
            row: 0,
            columns: right_columns,
            rows: top_rows,
        };
        self.panes[2].rect = CellRect {
            column: right_column,
            row: bottom_row,
            columns: right_columns,
            rows: bottom_rows,
        };
    }

    pub fn pane_at(&self, column: u32, row: u32) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.rect.contains(column, row))
    }

    pub fn place(&self, app_id: Option<&str>) -> Option<usize> {
        let free = |pane: &&Pane| pane.toplevel.is_none();
        let wanted = self
            .panes
            .iter()
            .position(|pane| free(&pane) && pane.app_id.is_some() && pane.app_id == app_id);
        wanted.or_else(|| {
            self.panes
                .iter()
                .position(|pane| free(&pane) && pane.app_id.is_none())
        })
    }

    pub fn pane_of(&self, toplevel: &ToplevelSurface) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.toplevel.as_ref() == Some(toplevel))
    }

    pub fn neighbour(&self, direction: Direction) -> Option<usize> {
        let (x, y) = self.panes[self.focused].rect.center();
        let mut best: Option<(i64, usize)> = None;
        for (i, pane) in self.panes.iter().enumerate() {
            if i == self.focused || pane.rect.columns == 0 {
                continue;
            }
            let (px, py) = pane.rect.center();
            let (ahead, across) = match direction {
                Direction::Left => (x - px, (y - py).abs()),
                Direction::Right => (px - x, (y - py).abs()),
                Direction::Up => (y - py, (x - px).abs()),
                Direction::Down => (py - y, (x - px).abs()),
            };
            if ahead <= 0 {
                continue;
            }
            let distance = ahead + across * 2;
            if best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, i));
            }
        }
        best.map(|(_, i)| i)
    }

    pub fn draw(&self, grid: &mut Grid) {
        for (i, pane) in self.panes.iter().enumerate() {
            let rect = pane.rect;
            if rect.columns < 3 || rect.rows < 3 {
                continue;
            }
            let title = if i == self.focused {
                FOCUSED_TITLE
            } else {
                TITLE
            };
            grid.fill(rect.column, rect.row, rect.columns, rect.rows, PANE);
            grid.fill(rect.column, rect.row, rect.columns, 1, title);
            grid.text(rect.column + 1, rect.row, pane.title, FOREGROUND, title);
        }
        if self.rows == 0 {
            return;
        }
        let status_row = self.rows - 1;
        grid.fill(0, status_row, self.columns, 1, STATUS);
        let status = format!(
            " decade  {} x {} cells  focus {}",
            self.columns, self.rows, self.panes[self.focused].title
        );
        grid.text(0, status_row, &status, FOREGROUND, STATUS);
    }
}
