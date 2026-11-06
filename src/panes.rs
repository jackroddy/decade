use crate::gpu::Rect;

pub const CELL_WIDTH: u32 = 9;
pub const CELL_HEIGHT: u32 = 18;

const BACKGROUND: [f32; 4] = [0.07, 0.07, 0.09, 1.0];
const PANE: [f32; 4] = [0.11, 0.11, 0.14, 1.0];
const TITLE: [f32; 4] = [0.20, 0.22, 0.30, 1.0];
const STATUS: [f32; 4] = [0.16, 0.18, 0.24, 1.0];

struct Grid {
    width: u32,
    height: u32,
    columns: u32,
    rows: u32,
}

impl Grid {
    fn rect(&self, column: u32, row: u32, columns: u32, rows: u32, color: [f32; 4]) -> Rect {
        let x0 = (column * CELL_WIDTH) as f32 / self.width as f32 * 2.0 - 1.0;
        let x1 = ((column + columns) * CELL_WIDTH) as f32 / self.width as f32 * 2.0 - 1.0;
        let y0 = 1.0 - (row * CELL_HEIGHT) as f32 / self.height as f32 * 2.0;
        let y1 = 1.0 - ((row + rows) * CELL_HEIGHT) as f32 / self.height as f32 * 2.0;
        Rect {
            min: [x0, y1],
            max: [x1, y0],
            color,
        }
    }

    fn pane(&self, out: &mut Vec<Rect>, column: u32, row: u32, columns: u32, rows: u32) {
        if columns < 3 || rows < 3 {
            return;
        }
        out.push(self.rect(column, row, columns, rows, PANE));
        out.push(self.rect(column, row, columns, 1, TITLE));
    }
}

pub fn layout(width: u32, height: u32) -> Vec<Rect> {
    let grid = Grid {
        width,
        height,
        columns: width / CELL_WIDTH,
        rows: height / CELL_HEIGHT,
    };
    let mut out = Vec::with_capacity(8);
    out.push(Rect {
        min: [-1.0, -1.0],
        max: [1.0, 1.0],
        color: BACKGROUND,
    });
    if grid.columns < 8 || grid.rows < 8 {
        return out;
    }

    let status_row = grid.rows - 1;
    let body_rows = status_row - 1;
    let left_columns = grid.columns / 2;
    let right_column = left_columns + 1;
    let right_columns = grid.columns - right_column;
    let top_rows = body_rows / 2;
    let bottom_row = top_rows + 1;
    let bottom_rows = body_rows - bottom_row;

    grid.pane(&mut out, 0, 0, left_columns, body_rows);
    grid.pane(&mut out, right_column, 0, right_columns, top_rows);
    grid.pane(
        &mut out,
        right_column,
        bottom_row,
        right_columns,
        bottom_rows,
    );
    out.push(grid.rect(0, status_row, grid.columns, 1, STATUS));
    out
}
