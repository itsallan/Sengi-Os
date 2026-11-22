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
    let card = theme.card;
    let width = card.menu_width;
    let mut parts = Vec::new();
    let mut y = card.pad_y;
    for row in rows {
        match row {
            Row::Item(label) => {
                let text = font.map(|font| {
                    font.fit(label, (width - 2 * (card.gap + card.pad_x)).max(0) as usize)
                });
                parts.push(Part {
                    rect: Rect::new(card.gap, y, width - 2 * card.gap, card.row),
                    separator: false,
                    label: text,
                    badge: None,
                });
                y += card.row;
            }
            Row::Separator => {
                parts.push(Part {
                    rect: Rect::new(card.gap, y, width - 2 * card.gap, card.pad_y),
                    separator: true,
                    label: None,
                    badge: None,
                });
                y += card.pad_y;
            }
        }
    }
    Layout {
        width,
        height: y + card.pad_y,
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
            let mut left = rect.x + card.gap + card.pad_x;
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

fn contains(rect: Rect, x: i32, y: i32) -> bool {
    x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

#[cfg(test)]
mod tests {
    use super::*;

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
