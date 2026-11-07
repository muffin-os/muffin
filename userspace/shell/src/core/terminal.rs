//! A grid of text cells with a scrollback history. It draws itself into
//! `0xAARRGGBB` pixels with the 8x8 `font8x8` glyphs at two times their size.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::ops::Range;

use font8x8::legacy::BASIC_LEGACY;

pub const BACKGROUND: u32 = 0xFF1E_2430;
pub const FOREGROUND: u32 = 0xFFE6_E6E6;

const GLYPH_PIXELS: usize = 8;
const SCALE: usize = 2;
/// The width and the height of one cell in pixels.
pub const CELL_PIXELS: usize = GLYPH_PIXELS * SCALE;
/// The blank border between the window edge and the first cell, in pixels.
pub const MARGIN_PIXELS: usize = 8;
/// The terminal keeps at most this number of text rows, the visible rows
/// included. A new row beyond it removes the oldest row.
pub const MAX_HISTORY_ROWS: usize = 1000;

/// The text rows of a window, the rows above the window, and the cursor.
/// The cursor is the cell that the next character goes into.
///
/// The view shows `rows` consecutive text rows. Scrolling moves the view up
/// into the history. A change of the text moves the view back to the bottom.
pub struct Terminal {
    cols: NonZeroUsize,
    rows: NonZeroUsize,
    /// The text rows, oldest first, each `cols` ASCII bytes. There are at
    /// least `rows` text rows.
    lines: VecDeque<Box<[u8]>>,
    /// The index in `lines` of the cursor row.
    line: usize,
    /// `col == cols` means that the row is full. The next character goes to
    /// the start of the next row.
    col: usize,
    /// The number of text rows between the bottom of the view and the last
    /// text row.
    scroll: usize,
    /// The view rows that changed since the last [`Terminal::take_dirty`].
    dirty: Option<Range<usize>>,
}

impl Terminal {
    /// Makes an empty terminal with as many cells as fit into a window of
    /// `width` by `height` pixels. Returns `None` when not one cell fits.
    #[must_use]
    pub fn for_window(width: usize, height: usize) -> Option<Self> {
        let cells_in = |pixels: usize| {
            NonZeroUsize::new(pixels.saturating_sub(2 * MARGIN_PIXELS) / CELL_PIXELS)
        };
        Some(Self::new(cells_in(width)?, cells_in(height)?))
    }

    /// Makes an empty terminal. Every view row is dirty.
    #[must_use]
    pub fn new(cols: NonZeroUsize, rows: NonZeroUsize) -> Self {
        Self {
            cols,
            rows,
            lines: (0..rows.get()).map(|_| blank_line(cols)).collect(),
            line: 0,
            col: 0,
            scroll: 0,
            dirty: Some(0..rows.get()),
        }
    }

    #[must_use]
    pub fn cols(&self) -> usize {
        self.cols.get()
    }

    /// Returns the number of rows in the view.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows.get()
    }

    /// Returns the text of view row `row` without the trailing blanks.
    #[must_use]
    pub fn row_text(&self, row: usize) -> &str {
        std::str::from_utf8(&self.lines[self.top() + row])
            .expect("the cells hold ASCII")
            .trim_end()
    }

    /// Returns the view row and the column of the cursor. Returns `None`
    /// when the view is scrolled away from the cursor.
    #[must_use]
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.view_row(self.line).map(|row| (row, self.col))
    }

    /// Writes each character of `text` with [`Terminal::put`].
    pub fn write_str(&mut self, text: &str) {
        text.chars().for_each(|c| self.put(c));
    }

    /// Writes `c` at the cursor and moves the cursor one cell on. `'\n'`
    /// starts a new row. A character that is not printable ASCII shows as
    /// `'?'`. A new row after the last row moves the view down by one.
    pub fn put(&mut self, c: char) {
        self.follow_cursor();
        if c == '\n' {
            self.new_row();
            return;
        }
        if self.col == self.cols() {
            self.new_row();
        }
        let byte = if c.is_ascii_graphic() || c == ' ' {
            c as u8
        } else {
            b'?'
        };
        self.lines[self.line][self.col] = byte;
        self.col += 1;
        self.mark_line(self.line);
    }

    /// Moves the cursor one cell back and blanks that cell. At the start of
    /// a row, the cursor goes to the last cell of the row above. At the
    /// top-left cell of the history, nothing changes.
    pub fn backspace(&mut self) {
        self.follow_cursor();
        if self.col > 0 {
            self.col -= 1;
        } else if self.line > 0 {
            self.mark_line(self.line);
            self.line -= 1;
            self.col = self.cols() - 1;
        } else {
            return;
        }
        self.lines[self.line][self.col] = b' ';
        self.mark_line(self.line);
    }

    /// Moves the view by `rows` text rows. A positive value moves it up to
    /// older rows. The view stops at the oldest row and at the last row.
    pub fn scroll(&mut self, rows: isize) {
        let max_scroll = self.lines.len() - self.rows();
        let scroll = self.scroll.saturating_add_signed(rows).min(max_scroll);
        if scroll != self.scroll {
            self.scroll = scroll;
            self.mark_view();
        }
    }

    /// Returns the view rows that changed since the last call, and marks all
    /// rows clean.
    pub fn take_dirty(&mut self) -> Option<Range<usize>> {
        self.dirty.take()
    }

    /// Returns the pixel rows of the window that the view rows `rows` cover.
    #[must_use]
    pub fn pixel_rows(rows: &Range<usize>) -> Range<usize> {
        MARGIN_PIXELS + rows.start * CELL_PIXELS..MARGIN_PIXELS + rows.end * CELL_PIXELS
    }

    /// Draws the view rows `rows` into `pixels`, a window with `width`
    /// pixels per row. The cursor shows as a cell with swapped colours.
    ///
    /// # Panics
    ///
    /// Panics when the window is smaller than the terminal.
    pub fn draw(&self, rows: Range<usize>, pixels: &mut [u32], width: usize) {
        let cursor = self.cursor();
        for row in rows {
            let line = &self.lines[self.top() + row];
            for (col, &byte) in line.iter().enumerate() {
                let glyph = BASIC_LEGACY[usize::from(byte)];
                let (fg, bg) = if cursor == Some((row, col)) {
                    (BACKGROUND, FOREGROUND)
                } else {
                    (FOREGROUND, BACKGROUND)
                };
                let left = MARGIN_PIXELS + col * CELL_PIXELS;
                let top = MARGIN_PIXELS + row * CELL_PIXELS;
                for (y, pixel_row) in pixels
                    .chunks_exact_mut(width)
                    .skip(top)
                    .take(CELL_PIXELS)
                    .enumerate()
                {
                    let bits = glyph[y / SCALE];
                    for (x, pixel) in pixel_row[left..left + CELL_PIXELS].iter_mut().enumerate() {
                        // Bit 0 of a glyph row is the leftmost pixel.
                        *pixel = if bits >> (x / SCALE) & 1 == 1 { fg } else { bg };
                    }
                }
            }
        }
    }

    /// Returns the index in `lines` of the top view row.
    fn top(&self) -> usize {
        self.lines.len() - self.rows() - self.scroll
    }

    fn view_row(&self, line: usize) -> Option<usize> {
        line.checked_sub(self.top())
            .filter(|&row| row < self.rows())
    }

    fn follow_cursor(&mut self) {
        if self.scroll != 0 {
            self.scroll = 0;
            self.mark_view();
        }
    }

    fn new_row(&mut self) {
        self.mark_line(self.line);
        self.col = 0;
        if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.mark_line(self.line);
            return;
        }
        self.lines.push_back(blank_line(self.cols));
        if self.lines.len() > MAX_HISTORY_ROWS.max(self.rows()) {
            self.lines.pop_front();
        }
        self.line = self.lines.len() - 1;
        self.mark_view();
    }

    fn mark_line(&mut self, line: usize) {
        if let Some(row) = self.view_row(line) {
            self.mark(row..row + 1);
        }
    }

    fn mark_view(&mut self) {
        self.mark(0..self.rows());
    }

    fn mark(&mut self, rows: Range<usize>) {
        self.dirty = Some(match self.dirty.take() {
            Some(dirty) => dirty.start.min(rows.start)..dirty.end.max(rows.end),
            None => rows,
        });
    }
}

fn blank_line(cols: NonZeroUsize) -> Box<[u8]> {
    vec![b' '; cols.get()].into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(cols: usize, rows: usize) -> Terminal {
        let mut terminal = Terminal::new(
            NonZeroUsize::new(cols).unwrap(),
            NonZeroUsize::new(rows).unwrap(),
        );
        terminal.take_dirty();
        terminal
    }

    fn view(terminal: &Terminal) -> Vec<&str> {
        (0..terminal.rows())
            .map(|row| terminal.row_text(row))
            .collect()
    }

    #[test]
    fn given_text_when_written_then_only_its_row_is_dirty() {
        let mut terminal = terminal(8, 3);
        terminal.put('\n');
        terminal.take_dirty();

        terminal.write_str("ab");

        assert_eq!(terminal.row_text(1), "ab");
        assert_eq!(terminal.take_dirty(), Some(1..2));
        assert_eq!(terminal.take_dirty(), None);
    }

    #[test]
    fn given_full_row_when_put_then_wraps_to_next_row() {
        let mut terminal = terminal(3, 2);

        terminal.write_str("abcd");

        assert_eq!(view(&terminal), ["abc", "d"]);
    }

    #[test]
    fn given_last_row_when_new_row_then_rows_move_up() {
        let mut terminal = terminal(4, 2);
        terminal.write_str("one\ntwo");
        terminal.take_dirty();

        terminal.write_str("\nsix");

        assert_eq!(view(&terminal), ["two", "six"]);
        assert_eq!(terminal.cursor(), Some((1, 3)));
        assert_eq!(terminal.take_dirty(), Some(0..2));
    }

    #[test]
    fn given_row_start_when_backspace_then_erases_end_of_row_above() {
        let mut terminal = terminal(3, 2);
        terminal.write_str("abcd");

        terminal.backspace();
        terminal.backspace();

        assert_eq!(view(&terminal), ["ab", ""]);
        assert_eq!(terminal.cursor(), Some((0, 2)));
    }

    #[test]
    fn given_top_left_when_backspace_then_nothing_changes() {
        let mut terminal = terminal(3, 2);

        terminal.backspace();

        assert_eq!(terminal.cursor(), Some((0, 0)));
        assert_eq!(terminal.take_dirty(), None);
    }

    #[test]
    fn given_history_when_scrolled_up_then_shows_older_rows_and_stops_at_oldest() {
        let mut terminal = terminal(4, 2);
        terminal.write_str("a\nb\nc\nd");
        terminal.take_dirty();

        terminal.scroll(1);

        assert_eq!(view(&terminal), ["b", "c"]);
        assert_eq!(terminal.cursor(), None);
        assert_eq!(terminal.take_dirty(), Some(0..2));

        terminal.scroll(10);

        assert_eq!(view(&terminal), ["a", "b"]);
    }

    #[test]
    fn given_bottom_when_scrolled_down_then_nothing_changes() {
        let mut terminal = terminal(4, 2);
        terminal.write_str("a\nb\nc");
        terminal.take_dirty();

        terminal.scroll(-3);

        assert_eq!(view(&terminal), ["b", "c"]);
        assert_eq!(terminal.take_dirty(), None);
    }

    #[test]
    fn given_scrolled_view_when_typed_then_view_returns_to_cursor() {
        let mut terminal = terminal(4, 2);
        terminal.write_str("a\nb\nc");
        terminal.scroll(1);

        terminal.put('x');

        assert_eq!(view(&terminal), ["b", "cx"]);
        assert_eq!(terminal.cursor(), Some((1, 2)));
    }

    #[test]
    fn given_full_history_when_new_row_then_oldest_row_goes_away() {
        let mut terminal = terminal(4, 2);
        for row in 0..MAX_HISTORY_ROWS + 5 {
            terminal.write_str(&format!("{}\n", row % 10));
        }

        terminal.scroll(isize::MAX);

        assert_eq!(view(&terminal), ["6", "7"]);
    }

    #[test]
    fn given_window_when_fitted_then_margin_and_cells_fit() {
        let terminal = Terminal::for_window(480, 360).unwrap();

        assert_eq!((terminal.cols(), terminal.rows()), (29, 21));
        assert!(Terminal::for_window(2 * MARGIN_PIXELS + CELL_PIXELS - 1, 100).is_none());
    }

    #[test]
    fn given_glyph_when_drawn_then_pixels_use_both_colours() {
        let mut terminal = terminal(1, 1);
        terminal.put('A');
        let width = 2 * MARGIN_PIXELS + CELL_PIXELS;
        let mut pixels = vec![0; width * width];

        terminal.draw(0..1, &mut pixels, width);

        let cell: Vec<u32> = pixels
            .chunks_exact(width)
            .skip(MARGIN_PIXELS)
            .take(CELL_PIXELS)
            .flat_map(|line| &line[MARGIN_PIXELS..MARGIN_PIXELS + CELL_PIXELS])
            .copied()
            .collect();
        assert!(cell.contains(&FOREGROUND));
        assert!(cell.contains(&BACKGROUND));
        assert!(cell.iter().all(|&p| p == FOREGROUND || p == BACKGROUND));
        assert_eq!(pixels[0], 0, "the margin stays untouched");
    }
}
