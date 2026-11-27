//! Where the arrow keys go on a page of grids: the category tiles first,
//! then the app cards, each grid with as many columns as the window has
//! room for. The search field is above them all.

/// The search field: nothing in the grids is chosen.
pub const FIELD: i32 = -1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
}

impl Key {
    /// The numbers the window sends.
    pub fn from_code(code: i32) -> Option<Key> {
        match code {
            0 => Some(Key::Left),
            1 => Some(Key::Right),
            2 => Some(Key::Up),
            3 => Some(Key::Down),
            _ => None,
        }
    }
}

/// One grid: how many things are in it and how many sit side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub count: usize,
    pub columns: usize,
}

impl Grid {
    fn rows(self) -> usize {
        self.count.div_ceil(self.columns.max(1))
    }
}

/// The column of a grid `to` wide that is under or over column `column` of a
/// grid `from` wide: the one nearest its middle.
fn across(column: usize, from: usize, to: usize) -> usize {
    ((2 * column + 1) * to / (2 * from.max(1))).min(to.saturating_sub(1))
}

/// The place a key leads to from `current`, which counts through every grid
/// in turn. Left and right run on across the rows; up from the first row
/// goes to the search field; nothing moves past the end.
pub fn step(grids: &[Grid], current: i32, key: Key) -> i32 {
    let grids: Vec<Grid> = grids
        .iter()
        .filter(|g| g.count > 0)
        .map(|g| Grid {
            count: g.count,
            columns: g.columns.max(1),
        })
        .collect();
    let total: usize = grids.iter().map(|g| g.count).sum();
    if total == 0 {
        return FIELD;
    }
    let Ok(at) = usize::try_from(current) else {
        return match key {
            Key::Right | Key::Down => 0,
            Key::Left | Key::Up => FIELD,
        };
    };
    let at = at.min(total - 1);
    let (mut index, mut before) = (0, 0);
    while at >= before + grids[index].count {
        before += grids[index].count;
        index += 1;
    }
    let grid = grids[index];
    let local = at - before;
    let (row, column) = (local / grid.columns, local % grid.columns);
    let to = match key {
        Key::Left => at.saturating_sub(1),
        Key::Right => (at + 1).min(total - 1),
        Key::Down if row + 1 < grid.rows() => before + (local + grid.columns).min(grid.count - 1),
        Key::Down => match grids.get(index + 1) {
            Some(next) => {
                let under = across(column, grid.columns, next.columns);
                before + grid.count + under.min(next.count - 1)
            }
            None => at,
        },
        Key::Up if row > 0 => at - grid.columns,
        Key::Up if index == 0 => return FIELD,
        Key::Up => {
            let prior = grids[index - 1];
            let over = across(column, grid.columns, prior.columns);
            let last = (prior.rows() - 1) * prior.columns;
            before - prior.count + (last + over).min(prior.count - 1)
        }
    };
    i32::try_from(to).unwrap_or(FIELD)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TILES: Grid = Grid {
        count: 9,
        columns: 4,
    };
    const CARDS: Grid = Grid {
        count: 5,
        columns: 3,
    };

    fn go(from: i32, key: Key) -> i32 {
        step(&[TILES, CARDS], from, key)
    }

    #[test]
    fn the_field_leads_into_the_first_thing() {
        assert_eq!(go(FIELD, Key::Down), 0);
        assert_eq!(go(FIELD, Key::Right), 0);
        assert_eq!(go(FIELD, Key::Up), FIELD);
        assert_eq!(go(FIELD, Key::Left), FIELD);
        assert_eq!(step(&[], FIELD, Key::Down), FIELD);
        assert_eq!(
            step(
                &[Grid {
                    count: 0,
                    columns: 3
                }],
                4,
                Key::Down
            ),
            FIELD
        );
    }

    #[test]
    fn left_and_right_run_across_rows_and_grids() {
        assert_eq!(go(3, Key::Right), 4);
        assert_eq!(go(4, Key::Left), 3);
        assert_eq!(go(8, Key::Right), 9);
        assert_eq!(go(9, Key::Left), 8);
        assert_eq!(go(0, Key::Left), 0);
        assert_eq!(go(13, Key::Right), 13);
    }

    #[test]
    fn down_and_up_keep_the_column() {
        assert_eq!(go(1, Key::Down), 5);
        assert_eq!(go(5, Key::Up), 1);
        assert_eq!(go(2, Key::Up), FIELD);
        // A short last row takes its last thing.
        assert_eq!(go(6, Key::Down), 8);
    }

    #[test]
    fn moving_between_grids_goes_to_what_is_under_or_over() {
        // Tile row 3 has one tile; below it, the first card.
        assert_eq!(go(8, Key::Down), 9);
        assert_eq!(go(9, Key::Up), 8);
        // The middle card's place over four columns is the last tile row,
        // which holds one tile only.
        assert_eq!(go(10, Key::Up), 8);
        let two = [
            Grid {
                count: 4,
                columns: 4,
            },
            CARDS,
        ];
        assert_eq!(step(&two, 3, Key::Down), 6);
        assert_eq!(step(&two, 0, Key::Down), 4);
        assert_eq!(step(&two, 5, Key::Up), 2);
    }

    #[test]
    fn nothing_moves_past_the_last_row() {
        assert_eq!(go(12, Key::Down), 12);
        assert_eq!(go(13, Key::Down), 13);
        // From the second card row, the first.
        assert_eq!(go(12, Key::Up), 9);
        // Down from a row above a shorter last row.
        assert_eq!(go(11, Key::Down), 13);
    }

    #[test]
    fn a_place_that_is_gone_is_taken_for_the_last() {
        assert_eq!(go(99, Key::Left), 12);
        assert_eq!(Key::from_code(0), Some(Key::Left));
        assert_eq!(Key::from_code(3), Some(Key::Down));
        assert_eq!(Key::from_code(7), None);
    }
}
