use crate::palette::Rgba;
use crate::panes::ContentId;
use crate::text::Font;

/// The row height, in pixels, that chrome sizes given to `scaled` are
/// given at.
const MOCKUP_ROW: i32 = 18;

/// `px`, a length given at `MOCKUP_ROW` rows, scaled to `cell_height`.
pub fn scaled(px: i32, cell_height: i32) -> i32 {
    (px * cell_height + MOCKUP_ROW / 2) / MOCKUP_ROW
}

/// A position in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// A width and height in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

/// A rectangle of whole cells on the grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellRect {
    pub column: u32,
    pub row: u32,
    pub columns: u32,
    pub rows: u32,
}

impl CellRect {
    pub const EMPTY: CellRect = CellRect {
        column: 0,
        row: 0,
        columns: 0,
        rows: 0,
    };

    pub fn contains(&self, column: u32, row: u32) -> bool {
        column >= self.column
            && column < self.column + self.columns
            && row >= self.row
            && row < self.row + self.rows
    }

    /// The centre, in half cells, so it is always whole.
    pub fn center(&self) -> (i64, i64) {
        (
            (self.column * 2 + self.columns) as i64,
            (self.row * 2 + self.rows) as i64,
        )
    }

    /// The cells one in from every side, or `EMPTY` when that leaves none.
    pub fn inner(&self) -> CellRect {
        if self.columns < 3 || self.rows < 3 {
            return CellRect::EMPTY;
        }
        CellRect {
            column: self.column + 1,
            row: self.row + 1,
            columns: self.columns - 2,
            rows: self.rows - 2,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.columns == 0 || self.rows == 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    Chrome,
    ChromeBold,
    Menu,
    MenuBold,

    /// A single chrome glyph, sized to the height every icon shares and
    /// centred in one chrome cell.
    Icon,
}

pub enum Shape {
    Rect {
        at: Point,
        size: Size,
        color: Rgba,
    },
    Text {
        at: Point,
        text: String,
        face: Face,
        color: Rgba,
    },
    Rounded {
        at: Point,
        size: Size,
        radius: i32,
        fill: Rgba,
        border: Rgba,
        border_width: i32,
    },

    /// Where the program draws the content `id`.
    Content {
        id: ContentId,
        at: Point,
        size: Size,
    },
}

pub struct Scene {
    cell_width: i32,
    cell_height: i32,
    pub shapes: Vec<Shape>,
}

impl Scene {
    pub fn new(font: &Font) -> Scene {
        Scene {
            cell_width: font.cell_width as i32,
            cell_height: font.cell_height as i32,
            shapes: Vec::new(),
        }
    }

    pub fn cell(&self) -> (i32, i32) {
        (self.cell_width, self.cell_height)
    }

    /// `px`, a length given at `MOCKUP_ROW` rows, scaled to the cell height.
    pub fn scaled(&self, px: i32) -> i32 {
        scaled(px, self.cell_height)
    }

    pub fn line_width(&self) -> i32 {
        (self.cell_width / 8).max(1)
    }

    pub fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: Rgba) {
        if width > 0 && height > 0 {
            self.shapes.push(Shape::Rect {
                at: Point { x, y },
                size: Size { width, height },
                color,
            });
        }
    }

    pub fn rounded(
        &mut self,
        x: i32,
        y: i32,
        (width, height): (i32, i32),
        radius: i32,
        fill: Rgba,
        border: Rgba,
    ) {
        self.shapes.push(Shape::Rounded {
            at: Point { x, y },
            size: Size { width, height },
            radius,
            fill,
            border,
            border_width: self.line_width(),
        });
    }

    pub fn text_px(&mut self, x: i32, y: i32, text: &str, face: Face, color: Rgba) {
        if !text.is_empty() {
            self.shapes.push(Shape::Text {
                at: Point { x, y },
                text: text.to_string(),
                face,
                color,
            });
        }
    }

    pub fn fill(&mut self, rect: CellRect, color: Rgba) {
        let (x, y) = self.px(rect.column, rect.row);
        let (width, height) = self.px(rect.columns, rect.rows);
        self.rect(x, y, width, height, color);
    }

    pub fn text(&mut self, column: u32, row: u32, text: &str, color: Rgba) {
        let (x, y) = self.px(column, row);
        self.text_px(x, y, text, Face::Chrome, color);
    }

    pub fn icon(&mut self, column: u32, row: u32, icon: char, color: Rgba) {
        let (x, y) = self.px(column, row);
        self.text_px(x, y, &icon.to_string(), Face::Icon, color);
    }

    /// A box one stroke wide inside `rect`, `inset` pixels in from its
    /// left, top, right and bottom sides.
    pub fn outline(&mut self, rect: CellRect, inset: [i32; 4], color: Rgba) {
        if rect.columns < 2 || rect.rows < 2 {
            return;
        }
        let [left, top, right, bottom] = inset;
        let (x, y) = self.px(rect.column, rect.row);
        let (width, height) = self.px(rect.columns, rect.rows);
        self.box_lines(
            x + left,
            y + top,
            width - left - right,
            height - top - bottom,
            color,
        );
    }

    pub fn frame(&mut self, rect: CellRect, color: Rgba) {
        let (x, y) = self.px(rect.column, rect.row);
        let (width, height) = self.px(rect.columns, rect.rows);
        self.box_lines(x, y, width, height, color);
    }

    /// A line one stroke wide down the right edge of `rect`.
    pub fn right_edge(&mut self, rect: CellRect, color: Rgba) {
        let (x, y) = self.px(rect.column + rect.columns, rect.row);
        let (_, height) = self.px(0, rect.rows);
        let line = self.line_width();
        self.rect(x - line, y, line, height, color);
    }

    pub fn box_lines(&mut self, x: i32, y: i32, width: i32, height: i32, color: Rgba) {
        let line = self.line_width();
        self.rect(x, y, width, line, color);
        self.rect(x, y + height - line, width, line, color);
        self.rect(x, y, line, height, color);
        self.rect(x + width - line, y, line, height, color);
    }

    pub fn px(&self, column: u32, row: u32) -> (i32, i32) {
        (
            column as i32 * self.cell_width,
            row as i32 * self.cell_height,
        )
    }
}
