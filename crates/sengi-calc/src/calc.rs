//! What the keys do. The calculator is an expression that is typed in, key
//! by key, and worked out with `=` or Return; the answer then stands in
//! place of the expression, so the next operator carries on from it. This
//! is the whole of the state, with no window in it, so the tests can press
//! keys.

use crate::eval::{self, CalcError};

/// How many finished sums are remembered.
pub const HISTORY: usize = 5;

/// An expression longer than this is not typed any further.
const MAX_LEN: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub expression: String,
    pub result: String,
}

#[derive(Debug, Default)]
pub struct Calc {
    expression: String,
    /// The expression is an answer that has just been worked out: a digit
    /// starts a new sum, an operator carries on from it.
    fresh: bool,
    error: Option<CalcError>,
    history: Vec<Entry>,
}

fn is_operator(c: char) -> bool {
    matches!(c, '+' | '-' | '*' | '/')
}

impl Calc {
    pub fn new() -> Calc {
        Calc::default()
    }

    /// What is typed, as it is evaluated: `*` and `/` are the keyboard's.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// What is typed, as it is drawn: with the signs of arithmetic.
    pub fn shown(&self) -> String {
        self.expression
            .chars()
            .map(|c| match c {
                '*' => '\u{d7}',
                '/' => '\u{f7}',
                '-' => '\u{2212}',
                other => other,
            })
            .collect()
    }

    pub fn error(&self) -> Option<CalcError> {
        self.error
    }

    /// Newest last.
    pub fn history(&self) -> &[Entry] {
        &self.history
    }

    /// The answer so far, shown small under the expression while it is
    /// being typed: only when there is something to work out.
    pub fn preview(&self) -> Option<String> {
        if self.fresh || self.error.is_some() || !self.expression.chars().any(is_operator_or_paren)
        {
            return None;
        }
        let result = eval::evaluate(&self.closed()).ok()?;
        (result != self.expression).then_some(result)
    }

    /// The expression with the brackets left open closed.
    fn closed(&self) -> String {
        let mut text = self.expression.clone();
        text.extend(std::iter::repeat_n(')', self.open_brackets()));
        text
    }

    /// The text Ctrl+C takes: the last answer if the display shows one,
    /// else what is worked out so far, else what is typed.
    pub fn copy_text(&self) -> Option<String> {
        if self.fresh {
            return Some(self.expression.clone());
        }
        self.preview()
            .or_else(|| (!self.expression.is_empty()).then(|| self.expression.clone()))
    }

    fn open_brackets(&self) -> usize {
        let opened = self.expression.matches('(').count();
        let closed = self.expression.matches(')').count();
        opened.saturating_sub(closed)
    }

    /// The number being typed at the end, if there is one.
    fn last_number(&self) -> &str {
        let start = self
            .expression
            .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map_or(0, |at| at + 1);
        &self.expression[start..]
    }

    fn last(&self) -> Option<char> {
        self.expression.chars().last()
    }

    /// Before typing something new: an error is gone, and a finished
    /// answer is kept only if what comes carries on from it.
    fn settle(&mut self, carries_on: bool) {
        self.error = None;
        if self.fresh {
            if !carries_on {
                self.expression.clear();
            }
            self.fresh = false;
        }
    }

    pub fn digit(&mut self, digit: char) {
        debug_assert!(digit.is_ascii_digit());
        self.settle(false);
        if self.expression.len() >= MAX_LEN || self.last() == Some(')') || self.last() == Some('%')
        {
            return;
        }
        self.expression.push(digit);
    }

    pub fn point(&mut self) {
        self.settle(false);
        if self.expression.len() >= MAX_LEN
            || self.last_number().contains('.')
            || matches!(self.last(), Some(')' | '%'))
        {
            return;
        }
        if self.last_number().is_empty() {
            self.expression.push('0');
        }
        self.expression.push('.');
    }

    pub fn operator(&mut self, op: char) {
        debug_assert!(is_operator(op));
        self.settle(true);
        if self.expression.len() >= MAX_LEN {
            return;
        }
        let Some(last) = self.last() else {
            if op == '-' {
                self.expression.push('-');
            }
            return;
        };
        if last == '(' {
            if op == '-' {
                self.expression.push('-');
            }
            return;
        }
        if !is_operator(last) {
            self.expression.push(op);
            return;
        }
        let run = self
            .expression
            .chars()
            .rev()
            .take_while(|c| is_operator(*c))
            .count();
        let before = &self.expression[..self.expression.len() - run];
        if before.is_empty() || before.ends_with('(') {
            // Only a sign is there so far, and it can only become another.
            if op == '-' || op == '+' {
                self.expression.truncate(before.len());
                if op == '-' {
                    self.expression.push('-');
                }
            }
        } else if op == '-' && run == 1 && matches!(last, '*' | '/') {
            // 5*-3
            self.expression.push('-');
        } else {
            self.expression.truncate(before.len());
            self.expression.push(op);
        }
    }

    pub fn open_bracket(&mut self) {
        self.settle(false);
        if self.expression.len() >= MAX_LEN || matches!(self.last(), Some(')' | '%')) {
            return;
        }
        if self.last().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            return;
        }
        self.expression.push('(');
    }

    pub fn close_bracket(&mut self) {
        self.settle(true);
        let after_value =
            matches!(self.last(), Some(c) if c.is_ascii_digit() || c == ')' || c == '%');
        if self.open_brackets() > 0 && after_value && self.expression.len() < MAX_LEN {
            self.expression.push(')');
        }
    }

    pub fn percent(&mut self) {
        self.settle(true);
        let after_value =
            matches!(self.last(), Some(c) if c.is_ascii_digit() || c == ')' || c == '%');
        if after_value && self.expression.len() < MAX_LEN {
            self.expression.push('%');
        }
    }

    pub fn backspace(&mut self) {
        self.error = None;
        self.fresh = false;
        self.expression.pop();
    }

    pub fn clear(&mut self) {
        *self = Calc {
            history: std::mem::take(&mut self.history),
            ..Calc::default()
        };
    }

    /// Takes a finished answer from the history as the expression.
    pub fn recall(&mut self, index: usize) {
        if let Some(entry) = self.history.get(index) {
            self.expression = entry.result.clone();
            self.fresh = true;
            self.error = None;
        }
    }

    /// Works the expression out. On a mistake the expression stays, so it
    /// can be mended.
    pub fn equals(&mut self) {
        if self.fresh || self.expression.is_empty() {
            return;
        }
        // Brackets left open are closed: "(2+3" means "(2+3)".
        let text = self.closed();
        match eval::evaluate(&text) {
            Ok(result) => {
                let entry = Entry {
                    expression: text,
                    result: result.clone(),
                };
                if self.history.last() != Some(&entry) {
                    self.history.push(entry);
                }
                if self.history.len() > HISTORY {
                    self.history.remove(0);
                }
                self.expression = result;
                self.fresh = true;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// A key as the keyboard sends it, as text. True if it did something.
    pub fn key(&mut self, text: &str) -> bool {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return false;
        };
        match c {
            '0'..='9' => self.digit(c),
            '.' | ',' => self.point(),
            '+' | '-' | '*' | '/' => self.operator(c),
            '\u{2212}' => self.operator('-'),
            '\u{d7}' | 'x' | 'X' => self.operator('*'),
            '\u{f7}' => self.operator('/'),
            '(' => self.open_bracket(),
            ')' => self.close_bracket(),
            '%' => self.percent(),
            '=' | '\n' | '\r' => self.equals(),
            '\u{8}' => self.backspace(),
            '\u{1b}' | '\u{7f}' | 'c' | 'C' => self.clear(),
            _ => return false,
        }
        true
    }
}

fn is_operator_or_paren(c: char) -> bool {
    is_operator(c) || matches!(c, '(' | ')' | '%')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keys: &str) -> Calc {
        let mut calc = Calc::new();
        for key in keys.chars() {
            calc.key(&key.to_string());
        }
        calc
    }

    #[test]
    fn typing_and_working_out() {
        assert_eq!(typed("2+3*4=").expression(), "14");
        assert_eq!(typed("0.1+0.2=").expression(), "0.3");
        assert_eq!(typed("1/3=").expression(), "0.333333333333");
        assert_eq!(typed("(1+2)*3\n").expression(), "9");
        assert_eq!(typed("200+10%=").expression(), "220");
    }

    #[test]
    fn display_uses_the_signs_of_arithmetic() {
        assert_eq!(typed("6*7-1/2").shown(), "6\u{d7}7\u{2212}1\u{f7}2");
    }

    #[test]
    fn an_answer_carries_on_with_an_operator_and_starts_over_with_a_digit() {
        let mut calc = typed("2+3=");
        calc.key("*");
        calc.key("4");
        assert_eq!(calc.expression(), "5*4");
        calc.key("=");
        assert_eq!(calc.expression(), "20");
        calc.key("7");
        assert_eq!(calc.expression(), "7");
    }

    #[test]
    fn a_second_operator_replaces_the_first() {
        assert_eq!(typed("5+*3").expression(), "5*3");
        assert_eq!(typed("5*-3").expression(), "5*-3");
        assert_eq!(typed("5*-3=").expression(), "-15");
        assert_eq!(typed("5*-+3").expression(), "5+3");
        assert_eq!(typed("-5").expression(), "-5");
        assert_eq!(typed("+5").expression(), "5");
        assert_eq!(typed("*5").expression(), "5");
        assert_eq!(typed("(-3)").expression(), "(-3)");
    }

    #[test]
    fn one_point_to_a_number() {
        assert_eq!(typed("1.2.3").expression(), "1.23");
        assert_eq!(typed(".5").expression(), "0.5");
        assert_eq!(typed("1.5+.5").expression(), "1.5+0.5");
        assert_eq!(typed("1,5").expression(), "1.5");
    }

    #[test]
    fn brackets_are_kept_balanced() {
        assert_eq!(typed(")").expression(), "");
        assert_eq!(typed("(2+3))").expression(), "(2+3)");
        assert_eq!(typed("(+").expression(), "(");
        assert_eq!(typed("2(").expression(), "2");
        // Left open, they are closed for the sum.
        assert_eq!(typed("(2+3=").expression(), "5");
        assert_eq!(typed("2*(3+4").preview().as_deref(), Some("14"));
    }

    #[test]
    fn percent_only_after_a_value() {
        assert_eq!(typed("%").expression(), "");
        assert_eq!(typed("+%").expression(), "");
        assert_eq!(typed("50%").expression(), "50%");
        assert_eq!(typed("50%5").expression(), "50%");
    }

    #[test]
    fn division_by_zero_shows_an_error_and_keeps_the_expression() {
        let mut calc = typed("5/0=");
        assert_eq!(calc.error(), Some(CalcError::DivideByZero));
        assert_eq!(calc.expression(), "5/0");
        calc.key("\u{8}");
        assert_eq!(calc.error(), None);
        calc.key("2");
        calc.key("=");
        assert_eq!(calc.expression(), "2.5");
    }

    #[test]
    fn a_broken_expression_is_a_syntax_error() {
        let calc = typed("5+=");
        assert_eq!(calc.error(), Some(CalcError::Syntax));
        assert_eq!(calc.expression(), "5+");
    }

    #[test]
    fn the_history_keeps_the_last_five() {
        let mut calc = Calc::new();
        for n in 1..=7 {
            for c in format!("{n}+{n}=").chars() {
                calc.key(&c.to_string());
            }
            calc.key("c");
        }
        let results: Vec<_> = calc.history().iter().map(|e| e.result.as_str()).collect();
        assert_eq!(results, ["6", "8", "10", "12", "14"]);
        assert_eq!(calc.history()[0].expression, "3+3");
        // A recalled answer stands as the expression.
        calc.recall(4);
        assert_eq!(calc.expression(), "14");
        calc.key("+");
        calc.key("1");
        calc.key("=");
        assert_eq!(calc.expression(), "15");
    }

    #[test]
    fn working_the_same_sum_twice_is_remembered_once() {
        let mut calc = typed("1+1=");
        calc.clear();
        for c in "1+1=".chars() {
            calc.key(&c.to_string());
        }
        assert_eq!(calc.history().len(), 1);
    }

    #[test]
    fn what_ctrl_c_copies() {
        assert_eq!(typed("").copy_text(), None);
        assert_eq!(typed("12").copy_text().as_deref(), Some("12"));
        assert_eq!(typed("2+3").copy_text().as_deref(), Some("5"));
        assert_eq!(typed("2+3=").copy_text().as_deref(), Some("5"));
        assert_eq!(typed("0.1+0.2=").copy_text().as_deref(), Some("0.3"));
    }

    #[test]
    fn clear_and_backspace() {
        let mut calc = typed("12+3");
        calc.key("\u{8}");
        assert_eq!(calc.expression(), "12+");
        calc.key("\u{1b}");
        assert_eq!(calc.expression(), "");
        // Clearing keeps the history.
        let mut calc = typed("1+1=");
        calc.key("c");
        assert_eq!(calc.history().len(), 1);
        // Backspace on an answer edits it like typed text.
        let mut calc = typed("12*12=");
        calc.key("\u{8}");
        assert_eq!(calc.expression(), "14");
        calc.key("5");
        assert_eq!(calc.expression(), "145");
    }

    #[test]
    fn keys_that_mean_nothing_are_not_taken() {
        let mut calc = Calc::new();
        assert!(!calc.key("q"));
        assert!(!calc.key(""));
        assert!(!calc.key("12"));
        assert!(calc.key("1"));
    }

    #[test]
    fn it_stops_growing() {
        let mut calc = Calc::new();
        for _ in 0..500 {
            calc.key("1");
        }
        assert_eq!(calc.expression().len(), MAX_LEN);
        calc.key("=");
        assert!(calc.error().is_none());
    }
}
