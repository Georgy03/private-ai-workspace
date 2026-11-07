//! A small exact calculator for agents: numbers with $ and thousands commas, + - * /, brackets,
//! a minus sign and a trailing % (divide by 100). It reads no data and calls nothing, so giving
//! it to an agent costs nothing in access. It exists so totals, differences and percentages in a
//! report come from arithmetic, not from a language model's guess.

const MAX_CHARS: usize = 500;

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    source: &'a str,
}

impl Parser<'_> {
    fn peek(&mut self) -> Option<char> {
        while self.chars.get(self.at).is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
        self.chars.get(self.at).copied()
    }

    fn expression(&mut self) -> Result<f64, String> {
        let mut value = self.term()?;
        while let Some(op) = self.peek() {
            if op != '+' && op != '-' {
                break;
            }
            self.at += 1;
            let right = self.term()?;
            value = if op == '+' { value + right } else { value - right };
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut value = self.unary()?;
        while let Some(op) = self.peek() {
            if op != '*' && op != '/' && op != '×' && op != '÷' {
                break;
            }
            self.at += 1;
            let right = self.unary()?;
            if (op == '/' || op == '÷') && right == 0.0 {
                return Err("division by zero".into());
            }
            value = if op == '*' || op == '×' { value * right } else { value / right };
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some('-') => {
                self.at += 1;
                Ok(-self.unary()?)
            }
            Some('+') => {
                self.at += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<f64, String> {
        let mut value = self.primary()?;
        while self.peek() == Some('%') {
            self.at += 1;
            value /= 100.0;
        }
        Ok(value)
    }

    fn primary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some('(') => {
                self.at += 1;
                let value = self.expression()?;
                if self.peek() != Some(')') {
                    return Err("a bracket is not closed".into());
                }
                self.at += 1;
                Ok(value)
            }
            Some(c) if c.is_ascii_digit() || c == '.' || c == '$' => {
                let mut text = String::new();
                while let Some(&c) = self.chars.get(self.at) {
                    if c == '$' || c == ',' {
                        self.at += 1;
                    } else if c.is_ascii_digit() || c == '.' {
                        text.push(c);
                        self.at += 1;
                    } else {
                        break;
                    }
                }
                text.parse::<f64>().map_err(|_| format!("\"{text}\" is not a number"))
            }
            Some(other) => Err(format!("unexpected \"{other}\" in \"{}\"", self.source)),
            None => Err("the expression ends too soon".into()),
        }
    }
}

/// The value of an arithmetic expression, rounded to six decimals so sums of cents come out clean.
pub fn evaluate(expression: &str) -> Result<f64, String> {
    if expression.chars().count() > MAX_CHARS {
        return Err("the expression is too long".into());
    }
    let mut parser = Parser { chars: expression.chars().collect(), at: 0, source: expression };
    let value = parser.expression()?;
    if parser.peek().is_some() {
        return Err(format!("could not read the expression after \"{}\"", &expression[..expression.char_indices().nth(parser.at).map_or(expression.len(), |(i, _)| i)]));
    }
    if !value.is_finite() {
        return Err("the result is not a number".into());
    }
    Ok((value * 1e6).round() / 1e6)
}

/// How a result is written: no trailing zeros, but always two decimals when it has any.
pub fn show(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value.round() as i64)
    } else {
        let text = format!("{value:.6}");
        let trimmed = text.trim_end_matches('0');
        let decimals = trimmed.split('.').nth(1).map_or(0, str::len);
        if decimals < 2 { format!("{value:.2}") } else { trimmed.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(text: &str) -> String {
        show(evaluate(text).unwrap())
    }

    #[test]
    fn sums_of_money_are_exact() {
        assert_eq!(calc("1,540.90 + 0 + 0 + 0 + 32.15 + 108.22"), "1681.27");
        assert_eq!(calc("$6,245.00 - $5,670.77"), "574.23");
        assert_eq!(calc("20 - 32.15"), "-12.15");
        assert_eq!(calc("150 - 108.22"), "41.78");
        assert_eq!(calc("3790 - 5670.77"), "-1880.77");
    }

    #[test]
    fn percentages_brackets_and_order_of_operations() {
        assert_eq!(calc("(2500 - 2325.48) / 2500 * 100"), "6.9808");
        assert_eq!(calc("2 + 3 * 4"), "14");
        assert_eq!(calc("(2 + 3) * 4"), "20");
        assert_eq!(calc("50%"), "0.50");
        assert_eq!(calc("-(3 - 5)"), "2");
    }

    #[test]
    fn bad_input_is_refused_not_guessed() {
        assert!(evaluate("1 / 0").unwrap_err().contains("zero"));
        assert!(evaluate("2 +").is_err());
        assert!(evaluate("(1 + 2").is_err());
        assert!(evaluate("sum(1,2)").is_err());
        assert!(evaluate("1 2").is_err());
        assert!(evaluate(&"1+".repeat(400)).is_err());
    }

    #[test]
    fn results_are_written_plainly() {
        assert_eq!(show(5.0), "5");
        assert_eq!(show(0.5), "0.50");
        assert_eq!(show(12.345), "12.345");
    }
}
