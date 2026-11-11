//! The state behind the panel while it is open: the query, the ranked
//! results, which one is selected and which rows are scrolled into view.
//! It knows nothing about drawing, so it is the part the tests drive.

use crate::matcher::{rank, Candidate, Hit, Query, TieBreak};

pub struct Session {
    candidates: Vec<Candidate>,
    tie_break: TieBreak,
    lines: usize,
    query: String,
    hits: Vec<Hit>,
    selected: usize,
    top: usize,
}

impl Session {
    /// `lines` is how many rows fit on screen.
    pub fn new(candidates: Vec<Candidate>, tie_break: TieBreak, lines: usize) -> Self {
        let mut session = Session {
            candidates,
            tie_break,
            lines: lines.max(1),
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            top: 0,
        };
        session.set_query("");
        session
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn lines(&self) -> usize {
        self.lines
    }

    /// Change how many rows fit on screen, keeping the selection in view.
    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines.max(1);
        self.top = self.top.min(self.selected);
        if self.selected >= self.top + self.lines {
            self.top = self.selected + 1 - self.lines;
        }
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.hits = rank(&self.candidates, &Query::new(query), self.tie_break);
        self.selected = 0;
        self.top = 0;
    }

    /// Move the selection by `delta` rows, stopping at either end and
    /// scrolling just enough to keep it in view.
    pub fn move_selection(&mut self, delta: i32) {
        if self.hits.is_empty() {
            return;
        }
        let last = self.hits.len() - 1;
        let target = self.selected as i64 + delta as i64;
        self.selected = target.clamp(0, last as i64) as usize;
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + self.lines {
            self.top = self.selected + 1 - self.lines;
        }
    }

    /// All results, best first.
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// The results currently on screen.
    pub fn visible(&self) -> &[Hit] {
        let end = (self.top + self.lines).min(self.hits.len());
        &self.hits[self.top..end]
    }

    /// Index in `hits` of the first visible row.
    pub fn first_visible(&self) -> usize {
        self.top
    }

    /// Which visible row is selected, if there are any results.
    pub fn selected_row(&self) -> Option<usize> {
        (!self.hits.is_empty()).then(|| self.selected - self.top)
    }

    /// Index of the selected candidate, as passed to `new`.
    pub fn chosen(&self) -> Option<usize> {
        self.hits.get(self.selected).map(|hit| hit.index)
    }

    /// Select a visible row, as a click does. Returns its candidate index.
    pub fn choose_row(&mut self, row: usize) -> Option<usize> {
        let index = self.visible().get(row)?.index;
        self.selected = self.top + row;
        Some(index)
    }

    pub fn candidate(&self, index: usize) -> &Candidate {
        &self.candidates[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(count: usize, lines: usize) -> Session {
        let candidates = (0..count)
            .map(|i| Candidate::new(format!("App {i:02}")))
            .collect();
        Session::new(candidates, TieBreak::Alphabetical, lines)
    }

    fn view(s: &Session) -> Vec<&str> {
        s.visible()
            .iter()
            .map(|hit| s.candidate(hit.index).name.as_str())
            .collect()
    }

    #[test]
    fn starts_with_everything_and_the_first_row_selected() {
        let s = session(3, 8);
        assert_eq!(view(&s), vec!["App 00", "App 01", "App 02"]);
        assert_eq!(s.selected_row(), Some(0));
        assert_eq!(s.chosen(), Some(0));
        assert_eq!(s.query(), "");
    }

    #[test]
    fn selection_stops_at_both_ends() {
        let mut s = session(3, 8);
        s.move_selection(-1);
        assert_eq!(s.selected_row(), Some(0));
        s.move_selection(1);
        s.move_selection(1);
        s.move_selection(1);
        assert_eq!(s.selected_row(), Some(2));
        assert_eq!(s.chosen(), Some(2));
        s.move_selection(i32::MIN);
        assert_eq!(s.selected_row(), Some(0));
        s.move_selection(i32::MAX);
        assert_eq!(s.selected_row(), Some(2));
    }

    #[test]
    fn the_view_scrolls_to_follow_the_selection() {
        let mut s = session(10, 4);
        assert_eq!(view(&s), vec!["App 00", "App 01", "App 02", "App 03"]);
        for _ in 0..3 {
            s.move_selection(1);
        }
        assert_eq!(view(&s)[0], "App 00");
        assert_eq!(s.selected_row(), Some(3));

        s.move_selection(1);
        assert_eq!(view(&s), vec!["App 01", "App 02", "App 03", "App 04"]);
        assert_eq!(s.selected_row(), Some(3));
        assert_eq!(s.chosen(), Some(4));

        s.move_selection(100);
        assert_eq!(view(&s), vec!["App 06", "App 07", "App 08", "App 09"]);
        assert_eq!(s.first_visible(), 6);
        assert_eq!(s.selected_row(), Some(3));

        s.move_selection(-2);
        assert_eq!(view(&s)[0], "App 06");
        assert_eq!(s.selected_row(), Some(1));
        s.move_selection(-2);
        assert_eq!(view(&s)[0], "App 05");
        assert_eq!(s.selected_row(), Some(0));
    }

    #[test]
    fn a_new_query_resets_selection_and_scroll() {
        let mut s = session(20, 4);
        s.move_selection(10);
        s.set_query("app 1");
        assert_eq!(s.selected_row(), Some(0));
        assert_eq!(view(&s), vec!["App 10", "App 11", "App 12", "App 13"]);
        // The ten that start with "App 1", then "App 01" on its two words.
        assert_eq!(s.hits().len(), 11);
        assert_eq!(s.query(), "app 1");
    }

    #[test]
    fn nothing_is_chosen_when_nothing_matches() {
        let mut s = session(5, 4);
        s.set_query("zzz");
        assert!(view(&s).is_empty());
        assert_eq!(s.selected_row(), None);
        assert_eq!(s.chosen(), None);
        s.move_selection(1);
        s.move_selection(-1);
        assert_eq!(s.chosen(), None);
        assert_eq!(s.choose_row(0), None);

        let mut empty = Session::new(Vec::new(), TieBreak::InputOrder, 0);
        assert_eq!(empty.lines(), 1);
        empty.move_selection(1);
        assert_eq!(empty.chosen(), None);
    }

    #[test]
    fn clicking_a_row_chooses_what_is_shown_there() {
        let mut s = session(10, 4);
        s.move_selection(5);
        assert_eq!(view(&s)[0], "App 02");
        assert_eq!(s.choose_row(1), Some(3));
        assert_eq!(s.chosen(), Some(3));
        assert_eq!(s.selected_row(), Some(1));
        assert_eq!(s.choose_row(4), None);
    }

    #[test]
    fn changing_the_row_count_keeps_the_selection_in_view() {
        let mut s = session(10, 1);
        assert_eq!(s.candidate_count(), 10);
        s.set_lines(4);
        assert_eq!(view(&s), vec!["App 00", "App 01", "App 02", "App 03"]);
        s.move_selection(7);
        assert_eq!(view(&s)[0], "App 04");
        s.set_lines(2);
        assert_eq!(view(&s), vec!["App 06", "App 07"]);
        assert_eq!(s.selected_row(), Some(1));
        s.set_lines(0);
        assert_eq!(s.lines(), 1);
        assert_eq!(view(&s), vec!["App 07"]);
        s.set_lines(20);
        assert_eq!(view(&s).len(), 3);
        assert_eq!(s.chosen(), Some(7));
    }
}
