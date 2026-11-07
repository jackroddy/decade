pub type Rgba = [f32; 4];

pub const CLEAR: Rgba = [0.0, 0.0, 0.0, 0.0];

#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgba,
    pub bg: Rgba,
}

pub struct Grid {
    pub columns: u32,
    pub rows: u32,
    pub cells: Vec<Cell>,
}

impl Grid {
    pub fn new(columns: u32, rows: u32, fg: Rgba, bg: Rgba) -> Grid {
        Grid {
            columns,
            rows,
            cells: vec![Cell { ch: ' ', fg, bg }; (columns * rows) as usize],
        }
    }

    pub fn fill(&mut self, column: u32, row: u32, columns: u32, rows: u32, bg: Rgba) {
        for r in row..(row + rows).min(self.rows) {
            for c in column..(column + columns).min(self.columns) {
                let cell = &mut self.cells[(r * self.columns + c) as usize];
                cell.ch = ' ';
                cell.bg = bg;
            }
        }
    }

    pub fn text(&mut self, column: u32, row: u32, text: &str, fg: Rgba, bg: Rgba) {
        if row >= self.rows {
            return;
        }
        for (i, ch) in text.chars().enumerate() {
            let c = column + i as u32;
            if c >= self.columns {
                return;
            }
            self.cells[(row * self.columns + c) as usize] = Cell { ch, fg, bg };
        }
    }
}
