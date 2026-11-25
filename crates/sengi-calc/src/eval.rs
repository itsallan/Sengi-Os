//! Arithmetic on decimal numbers, written by hand: a number is a whole
//! mantissa and a power of ten, so 0.1 + 0.2 is exactly 0.3 and nothing is
//! ever a binary fraction. [`evaluate`] reads an expression and gives the
//! answer as the text the display shows.
//!
//! Grammar, loosest first:
//!
//! ```text
//!     sum     = product { ("+" | "-") product }
//!     product = unary { ("*" | "/") unary }
//!     unary   = ("-" | "+") unary | postfix
//!     postfix = primary { "%" }
//!     primary = number | "(" sum ")"
//! ```
//!
//! `x%` is x / 100. Right after a plus or a minus it is a share of the
//! left side instead, as on a shop calculator: 200 + 10% is 220.

use std::fmt;

/// Digits a number is kept to. A quotient is cut here, never earlier.
const KEEP: u32 = 30;
/// Digits shown for a number that has more than [`EXACT`].
const SHOWN: u32 = 12;
/// A number with this many digits or fewer is shown exactly.
const EXACT: u32 = 15;
/// Beyond this power of ten a result is called too large, and below its
/// negative it is zero.
const LIMIT: i32 = 300;
/// A number is shown in full up to this many digits before the point, and
/// down to this many zeros after it.
const PLAIN_WIDTH: i32 = 15;
const PLAIN_ZEROS: i32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalcError {
    /// Not an expression: a missing number, a stray bracket, a bad character.
    Syntax,
    DivideByZero,
    TooLarge,
}

impl CalcError {
    /// What the display says.
    pub fn message(self) -> &'static str {
        match self {
            CalcError::Syntax => "Check the expression",
            CalcError::DivideByZero => "Cannot divide by zero",
            CalcError::TooLarge => "Number too large",
        }
    }
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// `mantissa * 10^exponent`, with no trailing zeros in the mantissa and at
/// most [`KEEP`] digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dec {
    mantissa: i128,
    exponent: i32,
}

fn pow10(n: u32) -> Option<i128> {
    10i128.checked_pow(n)
}

fn digits(mantissa: i128) -> u32 {
    mantissa
        .unsigned_abs()
        .checked_ilog10()
        .map_or(1, |n| n + 1)
}

/// The mantissa with its last `drop` digits taken off, rounded half away
/// from zero.
fn rounded(mantissa: i128, drop: u32) -> i128 {
    let Some(unit) = pow10(drop) else { return 0 };
    let (whole, rest) = (mantissa / unit, mantissa % unit);
    if rest.unsigned_abs() * 2 >= unit.unsigned_abs() {
        whole + mantissa.signum()
    } else {
        whole
    }
}

impl Dec {
    pub const ZERO: Dec = Dec {
        mantissa: 0,
        exponent: 0,
    };

    pub fn from_int(n: i64) -> Dec {
        Dec::make(n as i128, 0).expect("an integer is in range")
    }

    /// Cuts to [`KEEP`] digits, drops trailing zeros and checks the size.
    fn make(mut mantissa: i128, mut exponent: i32) -> Result<Dec, CalcError> {
        if mantissa == 0 {
            return Ok(Dec::ZERO);
        }
        while digits(mantissa) > KEEP {
            let drop = digits(mantissa) - KEEP;
            mantissa = rounded(mantissa, drop);
            exponent += drop as i32;
        }
        while mantissa % 10 == 0 {
            mantissa /= 10;
            exponent += 1;
        }
        let magnitude = exponent + digits(mantissa) as i32;
        if magnitude > LIMIT {
            return Err(CalcError::TooLarge);
        }
        if magnitude < -LIMIT {
            return Ok(Dec::ZERO);
        }
        Ok(Dec { mantissa, exponent })
    }

    pub fn is_zero(self) -> bool {
        self.mantissa == 0
    }

    /// Where the first digit is: the value is 0.DIGITS * 10^magnitude.
    fn magnitude(self) -> i32 {
        self.exponent + digits(self.mantissa) as i32
    }

    fn neg(self) -> Dec {
        Dec {
            mantissa: -self.mantissa,
            ..self
        }
    }

    pub fn plus(self, other: Dec) -> Result<Dec, CalcError> {
        if self.is_zero() {
            return Ok(other);
        }
        if other.is_zero() {
            return Ok(self);
        }
        let exponent = self.exponent.min(other.exponent);
        let scale = |d: Dec| {
            let up = (d.exponent - exponent) as u32;
            if digits(d.mantissa) + up > 37 {
                return None;
            }
            d.mantissa.checked_mul(pow10(up)?)
        };
        match (scale(self), scale(other)) {
            (Some(a), Some(b)) => Dec::make(a + b, exponent),
            // One is so much smaller than the other that none of its
            // digits would be kept.
            _ if self.magnitude() >= other.magnitude() => Ok(self),
            _ => Ok(other),
        }
    }

    pub fn minus(self, other: Dec) -> Result<Dec, CalcError> {
        self.plus(other.neg())
    }

    /// The same number with at most `n` digits.
    fn trimmed(self, n: u32) -> Result<Dec, CalcError> {
        let drop = digits(self.mantissa).saturating_sub(n);
        Dec::make(rounded(self.mantissa, drop), self.exponent + drop as i32)
    }

    pub fn times(self, other: Dec) -> Result<Dec, CalcError> {
        let (mut a, mut b) = (self, other);
        let product = loop {
            if let Some(product) = a.mantissa.checked_mul(b.mantissa) {
                break product;
            }
            // Long numbers on both sides: keep 18 digits of each, which
            // is more than the answer shows.
            a = a.trimmed(18)?;
            b = b.trimmed(18)?;
        };
        Dec::make(product, a.exponent + b.exponent)
    }

    pub fn divided_by(self, other: Dec) -> Result<Dec, CalcError> {
        if other.is_zero() {
            return Err(CalcError::DivideByZero);
        }
        if self.is_zero() {
            return Ok(Dec::ZERO);
        }
        // Both sides to 15 digits, then the numerator is scaled up so the
        // quotient has at least 20 digits and the product fits.
        let short = |d: Dec| {
            let drop = digits(d.mantissa).saturating_sub(15);
            (rounded(d.mantissa, drop), d.exponent + drop as i32)
        };
        let ((a, ae), (b, be)) = (short(self), short(other));
        let up = 37 - digits(a);
        let quotient = a * pow10(up).expect("37 digits fit") / b;
        Dec::make(quotient, ae - be - up as i32)
    }

    /// One hundredth.
    pub fn percent(self) -> Result<Dec, CalcError> {
        Dec::make(self.mantissa, self.exponent - 2)
    }

    /// The text for the display: plain digits with a dot, no grouping and
    /// no trailing zeros. A number of more than 15 digits is cut to 12.
    pub fn text(self) -> String {
        let mut mantissa = self.mantissa;
        let mut exponent = self.exponent;
        if digits(mantissa) > EXACT {
            let drop = digits(mantissa) - SHOWN;
            mantissa = rounded(mantissa, drop);
            exponent += drop as i32;
            while mantissa != 0 && mantissa % 10 == 0 {
                mantissa /= 10;
                exponent += 1;
            }
        }
        if mantissa == 0 {
            return "0".to_string();
        }
        let sign = if mantissa < 0 { "-" } else { "" };
        let all = mantissa.unsigned_abs().to_string();
        let point = all.len() as i32 + exponent;
        let body = if exponent >= 0 && point <= PLAIN_WIDTH {
            format!("{all}{}", "0".repeat(exponent as usize))
        } else if exponent < 0 && point > 0 {
            let (whole, fraction) = all.split_at(point as usize);
            format!("{whole}.{fraction}")
        } else if exponent < 0 && point > -PLAIN_ZEROS {
            format!("0.{}{all}", "0".repeat((-point) as usize))
        } else {
            let (first, rest) = all.split_at(1);
            let rest = if rest.is_empty() {
                String::new()
            } else {
                format!(".{rest}")
            };
            format!("{first}{rest}e{}", point - 1)
        };
        format!("{sign}{body}")
    }

    /// Reads what [`Dec::text`] writes, and plain numbers like it.
    pub fn parse(text: &str) -> Result<Dec, CalcError> {
        let mut parser = Parser::new(text);
        let value = parser.number()?;
        match parser.peek() {
            None => Ok(value),
            Some(_) => Err(CalcError::Syntax),
        }
    }
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    depth: u32,
}

/// What a product was made of, for the percent after a plus or a minus.
struct Term {
    value: Dec,
    /// Set when the term is just `x%`: this is x.
    bare_percent: Option<Dec>,
}

/// Nesting that is deeper than anyone types, and than the stack takes.
const MAX_DEPTH: u32 = 64;

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Parser<'a> {
        Parser {
            chars: text.chars().peekable(),
            depth: 0,
        }
    }

    /// The next character that is not a space, not taken.
    fn peek(&mut self) -> Option<char> {
        while self.chars.next_if(|c| c.is_whitespace()).is_some() {}
        self.chars.peek().copied()
    }

    fn eat(&mut self, wanted: &[char]) -> Option<char> {
        let next = self.peek()?;
        if wanted.contains(&next) {
            self.chars.next()
        } else {
            None
        }
    }

    fn sum(&mut self) -> Result<Dec, CalcError> {
        let mut total = self.product()?.value;
        while let Some(op) = self.eat(&['+', '-', '\u{2212}']) {
            let term = self.product()?;
            // 200 + 10% is 200 + 10% of 200.
            let right = match term.bare_percent {
                Some(raw) => total.times(raw)?.percent()?,
                None => term.value,
            };
            total = if op == '+' {
                total.plus(right)?
            } else {
                total.minus(right)?
            };
        }
        Ok(total)
    }

    fn product(&mut self) -> Result<Term, CalcError> {
        let (mut value, mut bare_percent) = self.unary()?;
        while let Some(op) = self.eat(&['*', '/', '\u{d7}', '\u{f7}', 'x', 'X']) {
            let (right, _) = self.unary()?;
            value = if matches!(op, '/' | '\u{f7}') {
                value.divided_by(right)?
            } else {
                value.times(right)?
            };
            bare_percent = None;
        }
        Ok(Term {
            value,
            bare_percent,
        })
    }

    fn unary(&mut self) -> Result<(Dec, Option<Dec>), CalcError> {
        if let Some(sign) = self.eat(&['-', '+', '\u{2212}']) {
            self.descend()?;
            let (value, _) = self.unary()?;
            self.depth -= 1;
            return Ok((if sign == '+' { value } else { value.neg() }, None));
        }
        let base = self.primary()?;
        let mut value = base;
        let mut percents = 0;
        while self.eat(&['%']).is_some() {
            value = value.percent()?;
            percents += 1;
        }
        // `5%` alone is a share of what is to its left; `5%%` is not.
        Ok((value, (percents == 1).then_some(base)))
    }

    fn descend(&mut self) -> Result<(), CalcError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(CalcError::Syntax)
        } else {
            Ok(())
        }
    }

    fn primary(&mut self) -> Result<Dec, CalcError> {
        if self.eat(&['(']).is_some() {
            self.descend()?;
            let inside = self.sum()?;
            self.depth -= 1;
            return match self.eat(&[')']) {
                Some(_) => Ok(inside),
                None => Err(CalcError::Syntax),
            };
        }
        self.number()
    }

    /// Digits with at most one dot, and at least one digit.
    fn number(&mut self) -> Result<Dec, CalcError> {
        self.peek();
        let mut mantissa: i128 = 0;
        let mut exponent: i32 = 0;
        let mut kept: u32 = 0;
        let (mut seen, mut dot) = (false, false);
        while let Some(&c) = self.chars.peek() {
            match c {
                '0'..='9' => {
                    seen = true;
                    let digit = i128::from(c as u8 - b'0');
                    if kept < KEEP {
                        mantissa = mantissa * 10 + digit;
                        if mantissa != 0 {
                            kept += 1;
                        }
                        if dot {
                            exponent -= 1;
                        }
                    } else if !dot {
                        // More digits than are kept: they only say how
                        // big the number is.
                        exponent += 1;
                    }
                }
                '.' if !dot => dot = true,
                _ => break,
            }
            self.chars.next();
        }
        if !seen {
            return Err(CalcError::Syntax);
        }
        Dec::make(mantissa, exponent)
    }
}

/// The value of an expression.
pub fn eval(text: &str) -> Result<Dec, CalcError> {
    let mut parser = Parser::new(text);
    let value = parser.sum()?;
    match parser.peek() {
        None => Ok(value),
        Some(_) => Err(CalcError::Syntax),
    }
}

/// The answer to an expression, as the display shows it.
pub fn evaluate(text: &str) -> Result<String, CalcError> {
    eval(text).map(Dec::text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(expression: &str) -> String {
        evaluate(expression).unwrap_or_else(|e| panic!("{expression}: {e}"))
    }

    #[test]
    fn plain_arithmetic() {
        for (expression, answer) in [
            ("1+2", "3"),
            ("7-10", "-3"),
            ("6*7", "42"),
            ("7/2", "3.5"),
            ("1/8", "0.125"),
            ("  12 + 30 ", "42"),
            ("2.5*4", "10"),
            (".5+.5", "1"),
            ("5.+1", "6"),
            ("0", "0"),
            ("-0", "0"),
            ("100000*100000", "10000000000"),
            ("12 x 3", "36"),
            ("9\u{d7}9", "81"),
            ("9\u{f7}3", "3"),
            ("9\u{2212}12", "-3"),
        ] {
            assert_eq!(ok(expression), answer, "{expression}");
        }
    }

    #[test]
    fn precedence() {
        for (expression, answer) in [
            ("2+3*4", "14"),
            ("2*3+4", "10"),
            ("10-4-3", "3"),
            ("100/10/5", "2"),
            ("2+3*4-6/2", "11"),
            ("8/2*4", "16"),
            ("1-2*3+4", "-1"),
        ] {
            assert_eq!(ok(expression), answer, "{expression}");
        }
    }

    #[test]
    fn brackets_and_signs() {
        for (expression, answer) in [
            ("(2+3)*4", "20"),
            ("2*(3+4)*5", "70"),
            ("((1+2)*(3+4))", "21"),
            ("-(2+3)", "-5"),
            ("--5", "5"),
            ("2*-3", "-6"),
            ("2--3", "5"),
            ("+4", "4"),
            ("-2*-2", "4"),
            ("(1)", "1"),
        ] {
            assert_eq!(ok(expression), answer, "{expression}");
        }
    }

    #[test]
    fn percent() {
        for (expression, answer) in [
            ("50%", "0.5"),
            ("5%", "0.05"),
            ("200*10%", "20"),
            ("200+10%", "220"),
            ("200-10%", "180"),
            ("200+10%*2", "200.2"),
            ("50%*50%", "0.25"),
            ("(10+10)%", "0.2"),
            ("100/50%", "200"),
            ("1+2+50%", "4.5"),
            ("10%%", "0.001"),
        ] {
            assert_eq!(ok(expression), answer, "{expression}");
        }
    }

    #[test]
    fn decimals_are_exact() {
        for (expression, answer) in [
            ("0.1+0.2", "0.3"),
            ("0.3-0.1", "0.2"),
            ("0.1*3", "0.3"),
            ("1.1*1.1", "1.21"),
            ("0.7+0.1", "0.8"),
            ("4.35*100", "435"),
            ("1.005*1000", "1005"),
            ("19.99*3", "59.97"),
            ("0.1+0.7+0.2", "1"),
        ] {
            assert_eq!(ok(expression), answer, "{expression}");
        }
    }

    #[test]
    fn quotients_are_cut_to_twelve_digits() {
        for (expression, answer) in [
            ("1/3", "0.333333333333"),
            ("2/3", "0.666666666667"),
            ("10/3", "3.33333333333"),
            ("1/7", "0.142857142857"),
            ("1/3*3", "1"),
            ("1/7*7", "1"),
            ("1/3+1/3+1/3", "1"),
            ("22/7", "3.14285714286"),
            ("100/3", "33.3333333333"),
        ] {
            assert_eq!(ok(expression), answer, "{expression}");
        }
    }

    #[test]
    fn big_and_small_numbers() {
        for (expression, answer) in [
            ("123456789012345*1", "123456789012345"),
            ("999999999999*999999999999", "9.99999999998e23"),
            ("0.000001", "0.000001"),
            ("0.0000001", "1e-7"),
            ("10000000000000000", "1e16"),
            ("1000000*1000000*1000000", "1e18"),
            ("2*0.0000001", "2e-7"),
            ("0.000001*0.000001", "1e-12"),
        ] {
            assert_eq!(ok(expression), answer, "{expression}");
        }
    }

    #[test]
    fn division_by_zero() {
        for expression in ["1/0", "0/0", "5/(3-3)", "1/0.0", "2+1/0", "1/(2-2)*5"] {
            assert_eq!(
                evaluate(expression),
                Err(CalcError::DivideByZero),
                "{expression}"
            );
        }
        assert_eq!(CalcError::DivideByZero.message(), "Cannot divide by zero");
        // Zero over something is fine.
        assert_eq!(ok("0/5"), "0");
    }

    #[test]
    fn what_is_not_an_expression() {
        for expression in [
            "", " ", "+", "1+", "*2", "2**3", "(", ")", "(1", "1)", "()", "1 2", "1..2", "1.2.3",
            ".", "abc", "1+a", "5%+", "%", "2(3)", "1,5",
        ] {
            assert_eq!(
                evaluate(expression),
                Err(CalcError::Syntax),
                "{expression:?}"
            );
        }
    }

    #[test]
    fn nesting_has_a_limit() {
        let deep = format!("{}1{}", "(".repeat(200), ")".repeat(200));
        assert_eq!(evaluate(&deep), Err(CalcError::Syntax));
        let fine = format!("{}1{}", "(".repeat(30), ")".repeat(30));
        assert_eq!(ok(&fine), "1");
    }

    #[test]
    fn huge_results_are_refused() {
        let big = "9".repeat(29);
        let mut expression = big.clone();
        for _ in 0..12 {
            expression = format!("{expression}*{big}");
        }
        assert_eq!(evaluate(&expression), Err(CalcError::TooLarge));
        assert_eq!(ok("0.0000000001*0.0000000001"), "1e-20");
    }

    #[test]
    fn long_typed_numbers_do_not_break_it() {
        assert_eq!(ok(&format!("0.{}1+1", "0".repeat(40))), "1");
        assert_eq!(
            ok("123456789012345678901234567890123456789"),
            "1.23456789012e38"
        );
        assert_eq!(evaluate("1.5e"), Err(CalcError::Syntax));
    }

    #[test]
    fn the_display_text() {
        for (value, text) in [
            ("1000", "1000"),
            ("0.05", "0.05"),
            ("12.5", "12.5"),
            ("0.00001", "0.00001"),
            ("0.000001", "0.000001"),
        ] {
            assert_eq!(Dec::parse(value).unwrap().text(), text);
        }
        assert_eq!(Dec::parse("1+1"), Err(CalcError::Syntax));
        assert_eq!(Dec::from_int(-7).text(), "-7");
    }
}
