use crate::grid::{Grid, Rgba};

pub const BACKGROUND: Rgba = [0.07, 0.07, 0.09, 1.0];
pub const FOREGROUND: Rgba = [0.80, 0.80, 0.84, 1.0];
const PANE: Rgba = [0.11, 0.11, 0.14, 1.0];
const TITLE: Rgba = [0.20, 0.22, 0.30, 1.0];
const STATUS: Rgba = [0.16, 0.18, 0.24, 1.0];

fn pane(grid: &mut Grid, column: u32, row: u32, columns: u32, rows: u32, title: &str) {
    if columns < 3 || rows < 3 {
        return;
    }
    grid.fill(column, row, columns, rows, PANE);
    grid.fill(column, row, columns, 1, TITLE);
    grid.text(column + 1, row, title, FOREGROUND, TITLE);
}

pub fn layout(grid: &mut Grid) {
    if grid.columns < 8 || grid.rows < 8 {
        return;
    }
    let status_row = grid.rows - 1;
    let body_rows = status_row - 1;
    let left_columns = grid.columns / 2;
    let right_column = left_columns + 1;
    let right_columns = grid.columns - right_column;
    let top_rows = body_rows / 2;
    let bottom_row = top_rows + 1;
    let bottom_rows = body_rows - bottom_row;

    pane(grid, 0, 0, left_columns, body_rows, "chat");
    pane(grid, right_column, 0, right_columns, top_rows, "terminal");
    pane(
        grid,
        right_column,
        bottom_row,
        right_columns,
        bottom_rows,
        "empty",
    );
    grid.fill(0, status_row, grid.columns, 1, STATUS);
    let status = format!(" decade  {} x {} cells", grid.columns, grid.rows);
    grid.text(0, status_row, &status, FOREGROUND, STATUS);
}
