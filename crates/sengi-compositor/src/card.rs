//! The cards the compositor draws over the desktop: the window switcher and
//! the menus. A card is laid out once, from the theme and the font, so that
//! drawing it and finding what is under the pointer agree. Lengths are in
//! the screen's pixels: the caller passes the theme scaled to the screen.

use crate::{
    decoration::{Canvas, TextBitmap, Theme},
    layout::Rect,
    text::Font,
};

/// One line of a menu, or a separator between lines.
pub enum Row<'a> {
    Item(&'a str),
    Separator,
}

/// One window in the switcher: its title and the letter of its badge.
pub struct Tile<'a> {
    pub title: &'a str,
    pub badge: char,
}

/// A card's parts, in the order of the rows or tiles it was made from.
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub parts: Vec<Part>,
    tiles: bool,
}

pub struct Part {
    /// Where the part is, relative to the card's top left corner.
    pub rect: Rect,
    /// Whether the part is a separator line, which cannot be selected.
    pub separator: bool,
    label: Option<TextBitmap>,
    badge: Option<TextBitmap>,
}

/// A menu: one row for each item, a line for each separator.
pub fn menu(theme: &Theme, font: Option<&Font>, rows: &[Row]) -> Layout {
    list(theme, font, rows, theme.card.menu_width)
}

/// A list of rows like a menu's, `width` pixels wide: the clipboard history.
pub fn list(theme: &Theme, font: Option<&Font>, rows: &[Row], width: i32) -> Layout {
    let card = theme.card;
    let mut parts = Vec::new();
    let pad = card.menu_pad;
    let mut y = pad;
    for row in rows {
        match row {
            Row::Item(label) => {
                let text = font
                    .map(|font| font.fit(label, (width - 2 * (pad + card.pad_x)).max(0) as usize));
                parts.push(Part {
                    rect: Rect::new(pad, y, width - 2 * pad, card.menu_row),
                    separator: false,
                    label: text,
                    badge: None,
                });
                y += card.menu_row;
            }
            Row::Separator => {
                // The line is pulled in from the ends of the rows above.
                let x = pad + card.menu_inset;
                parts.push(Part {
                    rect: Rect::new(x, y, width - 2 * x, card.gap),
                    separator: true,
                    label: None,
                    badge: None,
                });
                y += card.gap;
            }
        }
    }
    Layout {
        width,
        height: y + pad,
        parts,
        tiles: false,
    }
}

/// How many tiles fit in a row on a screen this wide, each no narrower than
/// its badge and a little of a title. Windows past that are left out.
pub fn switcher_capacity(theme: &Theme, screen_width: i32) -> usize {
    let card = theme.card;
    let min_tile = 2 * card.badge + 4 * card.gap;
    let avail = screen_width - 2 * card.margin - 2 * card.pad_x;
    ((avail + card.gap) / (min_tile + card.gap)).max(1) as usize
}

/// The switcher: one tile per window, in a row, centred by the caller.
/// Tiles share the screen between them, up to a menu's width each.
pub fn switcher(theme: &Theme, font: Option<&Font>, tiles: &[Tile], screen_width: i32) -> Layout {
    let card = theme.card;
    let count = tiles.len().max(1) as i32;
    let room = screen_width - 2 * card.margin - 2 * card.pad_x - (count - 1) * card.gap;
    let tile_width = (room / count).clamp(1, card.menu_width);
    let mut parts = Vec::new();
    let mut x = card.pad_x;
    for tile in tiles {
        let title_room = tile_width - card.gap - card.badge - 2 * card.gap;
        let label = font.map(|font| font.fit(tile.title, title_room.max(0) as usize));
        let badge = font.map(|font| font.render(&tile.badge.to_string()));
        parts.push(Part {
            rect: Rect::new(x, card.pad_y, tile_width, card.row),
            separator: false,
            label,
            badge,
        });
        x += tile_width + card.gap;
    }
    Layout {
        width: card.pad_x * 2 + count * tile_width + (count - 1) * card.gap,
        height: card.row + 2 * card.pad_y,
        parts,
        tiles: true,
    }
}

impl Layout {
    /// The part under the point `(x, y)`, relative to the card, if it is a
    /// selectable one.
    pub fn part_at(&self, x: i32, y: i32) -> Option<usize> {
        self.parts
            .iter()
            .position(|part| !part.separator && contains(part.rect, x, y))
    }

    /// The next selectable part after `from` in `step` direction (+1 or -1),
    /// wrapping round; `None` when there is none to select.
    pub fn step(&self, from: Option<usize>, step: i32) -> Option<usize> {
        let count = self.parts.len() as i32;
        if count == 0 {
            return None;
        }
        let mut index = from.map_or(if step > 0 { -1 } else { count }, |i| i as i32);
        for _ in 0..count {
            index = (index + step).rem_euclid(count);
            if !self.parts[index as usize].separator {
                return Some(index as usize);
            }
        }
        None
    }

    /// Draws the card with `selected` part highlighted, in pixels.
    pub fn draw(&self, theme: &Theme, selected: Option<usize>) -> Canvas {
        let card = theme.card;
        let mut canvas = Canvas::new(self.width.max(1) as usize, self.height.max(1) as usize);
        let radius = theme.radius as f32;
        let edge = theme.metrics.border;
        canvas.round_rect(0, 0, self.width, self.height, radius, theme.border);
        canvas.round_rect(
            edge,
            edge,
            self.width - 2 * edge,
            self.height - 2 * edge,
            (radius - edge as f32).max(0.0),
            theme.active,
        );
        for (index, part) in self.parts.iter().enumerate() {
            let rect = part.rect;
            if part.separator {
                let y = rect.y + rect.h / 2;
                canvas.round_rect(rect.x, y, rect.w, edge.max(1), 0.0, theme.border);
                continue;
            }
            let lit = selected == Some(index);
            if self.tiles {
                canvas.round_rect(
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h,
                    radius,
                    if lit { theme.accent } else { theme.raised },
                );
            } else if lit {
                canvas.round_rect(rect.x, rect.y, rect.w, rect.h, radius, theme.accent);
            }
            let ink = if lit { theme.accent_text } else { theme.text };
            // A menu's text starts one padding in from its row.
            let inset = if self.tiles { card.gap } else { 0 };
            let mut left = rect.x + inset + card.pad_x;
            if let Some(bitmap) = &part.badge {
                let side = card.badge;
                let top = rect.y + (rect.h - side) / 2;
                canvas.round_rect(rect.x + card.gap, top, side, side, radius, theme.hover);
                let x = rect.x + card.gap + (side - bitmap.width as i32) / 2;
                let y = top + (side - bitmap.height as i32) / 2;
                canvas.label(bitmap, x, y, theme.text);
                left = rect.x + card.gap + side + card.gap;
            }
            if let Some(label) = &part.label {
                let top = rect.y + (rect.h - label.height as i32) / 2;
                canvas.label(label, left, top, ink);
            }
        }
        canvas
    }
}

/// The keyboard shortcuts cheat sheet: a title, the sections of
/// shortcuts in columns, and how to close it. Laid out once, like the
/// other cards.
pub struct Sheet {
    pub width: i32,
    pub height: i32,
    title: Option<TextBitmap>,
    footer: Option<TextBitmap>,
    columns: Vec<SheetColumn>,
    /// What each line says, in words, in the order they are drawn: the
    /// keys and what they do. For the tests, which compare it with the
    /// table the keys are bound by.
    #[cfg_attr(not(test), allow(dead_code))]
    pub lines: Vec<(String, String)>,
}

struct SheetColumn {
    x: i32,
    key_width: i32,
    items: Vec<SheetItem>,
}

enum SheetItem {
    Heading {
        y: i32,
        text: Option<TextBitmap>,
    },
    Line {
        y: i32,
        keys: Option<TextBitmap>,
        keys_width: i32,
        what: Option<TextBitmap>,
    },
}

const SHEET_TITLE: &str = "Keyboard shortcuts";
const SHEET_FOOTER: &str = "Press Esc or click anywhere to close";

/// The sheet for `sections` on a screen `screen_width` pixels wide. The
/// sections go into two columns, the largest first, each into the column
/// that is shorter so far.
pub fn shortcuts(
    theme: &Theme,
    font: Option<&Font>,
    sections: &[crate::keys::SheetSection],
    screen_width: i32,
) -> Sheet {
    let card = theme.card;
    let row = card.menu_row;
    let render = |text: &str| font.map(|font| font.render(text));
    let width_of = |bitmap: &Option<TextBitmap>| bitmap.as_ref().map_or(0, |b| b.width as i32);

    // Which section goes into which column.
    let mut order: Vec<usize> = (0..sections.len()).collect();
    order.sort_by_key(|index| std::cmp::Reverse(sections[*index].1.len()));
    let mut taken: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    let mut rows = [0usize; 2];
    for index in order {
        let side = usize::from(rows[1] < rows[0]);
        taken[side].push(index);
        rows[side] += sections[index].1.len() + 1;
    }
    for side in &mut taken {
        side.sort_unstable();
    }

    let head = row + card.gap;
    let mut lines = Vec::new();
    let mut columns: Vec<SheetColumn> = Vec::new();
    let mut tallest = 0;
    let mut x = card.pad_x;
    let between = 2 * card.pad_x;
    let columns_count = taken.iter().filter(|side| !side.is_empty()).count().max(1) as i32;
    let room = screen_width - 2 * card.margin - 2 * card.pad_x - (columns_count - 1) * between;
    let each = (room / columns_count).max(1);
    for side in taken.iter().filter(|side| !side.is_empty()) {
        let mut items = Vec::new();
        let mut y = head;
        let mut key_width = 0;
        let mut whats: Vec<(usize, String)> = Vec::new();
        for (position, index) in side.iter().enumerate() {
            let (section, entries) = &sections[*index];
            if position > 0 {
                y += card.gap;
            }
            items.push(SheetItem::Heading {
                y,
                text: render(section.title()),
            });
            y += row;
            for (keys, what) in entries {
                let bitmap = render(keys);
                let chip = width_of(&bitmap) + card.gap;
                key_width = key_width.max(chip);
                whats.push((items.len(), (*what).to_string()));
                items.push(SheetItem::Line {
                    y,
                    keys: bitmap,
                    keys_width: chip,
                    what: None,
                });
                lines.push((keys.clone(), (*what).to_string()));
                y += row;
            }
        }
        // The words, cut to the room the column has.
        let room_for_words = (each - key_width - card.gap).max(0) as usize;
        let mut widest = 0;
        for (at, what) in whats {
            let bitmap = font.map(|font| font.fit(&what, room_for_words));
            widest = widest.max(width_of(&bitmap));
            if let SheetItem::Line { what, .. } = &mut items[at] {
                *what = bitmap;
            }
        }
        let column_width = key_width + card.gap + widest;
        columns.push(SheetColumn {
            x,
            key_width,
            items,
        });
        x += column_width + between;
        tallest = tallest.max(y);
    }
    let footer = render(SHEET_FOOTER);
    let title = render(SHEET_TITLE);
    let width = (x - between + card.pad_x).max(width_of(&footer) + 2 * card.pad_x);
    Sheet {
        width,
        height: tallest + card.gap + row + card.pad_y,
        title,
        footer,
        columns,
        lines,
    }
}

impl Sheet {
    #[cfg(test)]
    fn columns_count(&self) -> usize {
        self.columns.len()
    }

    /// How many lines, headings included, each column has.
    #[cfg(test)]
    fn column_rows(&self) -> Vec<usize> {
        self.columns.iter().map(|c| c.items.len()).collect()
    }

    pub fn draw(&self, theme: &Theme) -> Canvas {
        let card = theme.card;
        let row = card.menu_row;
        let mut canvas = Canvas::new(self.width.max(1) as usize, self.height.max(1) as usize);
        let radius = theme.radius as f32;
        let edge = theme.metrics.border;
        canvas.round_rect(0, 0, self.width, self.height, radius, theme.border);
        canvas.round_rect(
            edge,
            edge,
            self.width - 2 * edge,
            self.height - 2 * edge,
            (radius - edge as f32).max(0.0),
            theme.active,
        );
        let centre = |bitmap: &TextBitmap, y: i32| y + (row - bitmap.height as i32) / 2;
        if let Some(title) = &self.title {
            canvas.label(title, card.pad_x, centre(title, card.pad_y), theme.text);
        }
        for column in &self.columns {
            for item in &column.items {
                match item {
                    SheetItem::Heading { y, text } => {
                        if let Some(text) = text {
                            canvas.label(text, column.x, centre(text, *y), theme.text_muted);
                        }
                    }
                    SheetItem::Line {
                        y,
                        keys,
                        keys_width,
                        what,
                    } => {
                        canvas.round_rect(column.x, *y, *keys_width, row, radius, theme.raised);
                        if let Some(keys) = keys {
                            canvas.label(
                                keys,
                                column.x + card.gap / 2,
                                centre(keys, *y),
                                theme.text,
                            );
                        }
                        if let Some(what) = what {
                            let left = column.x + column.key_width + card.gap;
                            canvas.label(what, left, centre(what, *y), theme.text);
                        }
                    }
                }
            }
        }
        if let Some(footer) = &self.footer {
            let y = self.height - card.pad_y - row;
            canvas.label(footer, card.pad_x, centre(footer, y), theme.text_muted);
        }
        canvas
    }
}

fn contains(rect: Rect, x: i32, y: i32) -> bool {
    x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_rows_are_22_pixels_in_6_of_padding_with_pulled_in_separators() {
        let theme = Theme::from_tokens(&sengi_tokens::Tokens::builtin());
        let layout = menu(
            &theme,
            None,
            &[Row::Item("a"), Row::Separator, Row::Item("b")],
        );
        let rows: Vec<_> = layout.parts.iter().map(|part| part.rect).collect();
        let width = theme.card.menu_width;
        assert_eq!(rows[0], Rect::new(6, 6, width - 12, 22));
        // The line is six pixels in from the row's ends.
        assert_eq!(rows[1].x, 12);
        assert_eq!(rows[1].w, width - 24);
        assert_eq!(rows[2], Rect::new(6, 6 + 22 + rows[1].h, width - 12, 22));
        assert_eq!(layout.height, 6 + 22 + rows[1].h + 22 + 6);
    }

    #[test]
    fn the_sheet_says_what_the_key_table_says_and_no_more() {
        let theme = Theme::from_tokens(&sengi_tokens::Tokens::builtin());
        let sections = crate::keys::cheat_sheet();
        let sheet = shortcuts(&theme, None, &sections, 1280);
        let mut wanted: Vec<(String, String)> = sections
            .iter()
            .flat_map(|(_, lines)| lines.iter().map(|(k, w)| (k.clone(), (*w).to_string())))
            .collect();
        let mut drawn = sheet.lines.clone();
        wanted.sort();
        drawn.sort();
        assert_eq!(drawn, wanted);
        // One line for each listed binding, and the console keys.
        let bound = crate::keys::BINDINGS.iter().filter(|b| b.listed).count();
        assert_eq!(sheet.lines.len(), bound + 1);
    }

    #[test]
    fn the_sheet_is_in_two_columns_that_are_about_as_tall_as_each_other() {
        let theme = Theme::from_tokens(&sengi_tokens::Tokens::builtin());
        let sheet = shortcuts(&theme, None, &crate::keys::cheat_sheet(), 1280);
        assert_eq!(sheet.columns_count(), 2);
        let sizes = sheet.column_rows();
        let (a, b) = (sizes[0] as i32, sizes[1] as i32);
        assert!((a - b).abs() <= 4, "{sizes:?}");
        // And it is no taller than a small screen.
        assert!(sheet.height < 600, "{} px tall", sheet.height);
        let canvas = sheet.draw(&theme);
        assert_eq!(canvas.width() as i32, sheet.width);
    }

    #[test]
    fn the_switcher_never_wider_than_the_screen_allows() {
        let theme = Theme::from_tokens(&sengi_tokens::Tokens::builtin());
        for screen in [800, 1280, 1920, 3840] {
            let count = switcher_capacity(&theme, screen);
            assert!(count >= 1);
            let tiles: Vec<Tile> = (0..count)
                .map(|_| Tile {
                    title: "a title",
                    badge: 'x',
                })
                .collect();
            let layout = switcher(&theme, None, &tiles, screen);
            assert!(layout.width <= screen, "{count} tiles on {screen} px");
        }
    }
}
