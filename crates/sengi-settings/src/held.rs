//! A list that does not move under the hand.
//!
//! Lists of networks and devices are refilled whenever the service has
//! news, and a fresh list comes sorted: a network that has just been heard
//! louder would jump up a row. While the pointer is over a list or the
//! keyboard is in it, that must not happen, or a click lands on a row that
//! was not there when the finger went down. So while a list is held, rows
//! keep their places: one that is still there is updated where it is, one
//! that has gone stays as a row that cannot be used, and new ones are added
//! at the end. When the list is let go it takes the fresh order.

/// One row of such a list: what the service last said of it, and whether
/// it has gone since. It reads as the thing itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<T> {
    pub item: T,
    /// No longer there, kept so that the rows below it do not move.
    pub gone: bool,
}

impl<T> Row<T> {
    pub fn new(item: T) -> Row<T> {
        Row { item, gone: false }
    }
}

impl<T> std::ops::Deref for Row<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.item
    }
}

/// The rows to show. `shown` is what is on screen, `fresh` what the
/// service now says, in its own order. `gone` turns a row into one that
/// is no longer there.
pub fn merge<T: Clone>(
    shown: &[T],
    fresh: Vec<T>,
    held: bool,
    id: impl Fn(&T) -> &str,
    gone: impl Fn(&mut T),
    limit: usize,
) -> Vec<T> {
    if !held {
        return fresh;
    }
    let mut rest: Vec<Option<T>> = fresh.into_iter().map(Some).collect();
    let mut rows: Vec<T> = shown
        .iter()
        .map(|old| {
            let found = rest
                .iter_mut()
                .find(|new| new.as_ref().is_some_and(|new| id(new) == id(old)));
            match found.and_then(Option::take) {
                Some(new) => new,
                None => {
                    let mut old = old.clone();
                    gone(&mut old);
                    old
                }
            }
        })
        .collect();
    rows.extend(rest.into_iter().flatten());
    rows.truncate(limit.max(shown.len()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Row(&'static str, u32, bool);

    fn merged(shown: &[Row], fresh: Vec<Row>, held: bool) -> Vec<Row> {
        merge(shown, fresh, held, |row| row.0, |row| row.2 = false, 4)
    }

    #[test]
    fn a_list_nobody_is_touching_takes_the_fresh_order() {
        let shown = [Row("a", 1, true), Row("b", 1, true)];
        let fresh = vec![Row("b", 2, true), Row("c", 2, true)];
        assert_eq!(merged(&shown, fresh.clone(), false), fresh);
    }

    #[test]
    fn a_held_list_keeps_its_rows_where_they_are() {
        let shown = [Row("a", 1, true), Row("b", 1, true), Row("c", 1, true)];
        // b got louder and sorts first; a has gone; d is new.
        let fresh = vec![Row("b", 9, true), Row("d", 5, true), Row("c", 2, true)];
        assert_eq!(
            merged(&shown, fresh, true),
            [
                Row("a", 1, false),
                Row("b", 9, true),
                Row("c", 2, true),
                Row("d", 5, true)
            ]
        );
    }

    #[test]
    fn a_held_list_does_not_grow_without_bound() {
        let shown = [Row("a", 1, true)];
        let fresh = vec![
            Row("b", 1, true),
            Row("c", 1, true),
            Row("d", 1, true),
            Row("e", 1, true),
            Row("f", 1, true),
        ];
        assert_eq!(merged(&shown, fresh, true).len(), 4);
        // But never shrinks below what is on screen.
        let shown = [
            Row("a", 1, true),
            Row("b", 1, true),
            Row("c", 1, true),
            Row("d", 1, true),
            Row("e", 1, true),
        ];
        assert_eq!(merged(&shown, vec![Row("z", 1, true)], true).len(), 5);
    }

    #[test]
    fn a_row_that_comes_back_is_itself_again() {
        let shown = [Row("a", 1, false), Row("b", 1, true)];
        let fresh = vec![Row("b", 1, true), Row("a", 3, true)];
        assert_eq!(
            merged(&shown, fresh, true),
            [Row("a", 3, true), Row("b", 1, true)]
        );
    }

    #[test]
    fn two_rows_with_one_id_do_not_swallow_each_other() {
        let shown = [Row("a", 1, true)];
        let fresh = vec![Row("a", 2, true), Row("a", 3, true)];
        assert_eq!(
            merged(&shown, fresh, true),
            [Row("a", 2, true), Row("a", 3, true)]
        );
    }
}
