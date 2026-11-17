//! A small calculator for the launcher: `2+2*3`, `(1+2)^3`, `sqrt(2)`, `15% of`-less percent as modulo, `pi*2`.

/// The value of `text` if it is a calculation (it needs an operator, a bracket or a function, so a plain number or a
/// word is not one), formatted for showing and copying.
pub fn eval_text(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || !t.chars().any(|c| "+-*/^%()".contains(c) || c.is_alphabetic()) || t.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    // a lone "-5" or "pi" is not worth a result row; a real calculation has an operator past the first character,
    // a bracket, or a function call
    let operator = t.char_indices().any(|(i, c)| i > 0 && "+-*/^%".contains(c));
    let call = t.contains('(');
    if !operator && !call {
        return None;
    }
    let v = Parser { s: t.as_bytes(), i: 0 }.parse()?;
    if !v.is_finite() {
        return None;
    }
    Some(format(v))
}

fn format(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn parse(mut self) -> Option<f64> {
        let v = self.sum()?;
        self.skip();
        (self.i == self.s.len()).then_some(v)
    }

    fn skip(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.skip();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn sum(&mut self) -> Option<f64> {
        let mut v = self.product()?;
        loop {
            if self.eat(b'+') {
                v += self.product()?;
            } else if self.eat(b'-') {
                v -= self.product()?;
            } else {
                return Some(v);
            }
        }
    }

    fn product(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            self.skip();
            if self.s.get(self.i) == Some(&b'*') && self.s.get(self.i + 1) == Some(&b'*') {
                self.i += 2;
                let e = self.unary()?;
                v = v.powf(e);
            } else if self.eat(b'*') || self.eat(b'x') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                v /= self.unary()?;
            } else if self.eat(b'%') {
                v %= self.unary()?;
            } else {
                return Some(v);
            }
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        if self.eat(b'^') {
            let e = self.unary()?; // right associative
            return Some(base.powf(e));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        self.skip();
        if self.eat(b'(') {
            let v = self.sum()?;
            return self.eat(b')').then_some(v);
        }
        let start = self.i;
        if self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.' || *c == b'_') {
                self.i += 1;
            }
            return std::str::from_utf8(&self.s[start..self.i]).ok()?.replace('_', "").parse().ok();
        }
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i]).ok()?.to_lowercase();
        match name.as_str() {
            "pi" => return Some(std::f64::consts::PI),
            "e" => return Some(std::f64::consts::E),
            "" => return None,
            _ => {}
        }
        if !self.eat(b'(') {
            return None;
        }
        let a = self.sum()?;
        if !self.eat(b')') {
            return None;
        }
        Some(match name.as_str() {
            "sqrt" => a.sqrt(),
            "sin" => a.sin(),
            "cos" => a.cos(),
            "tan" => a.tan(),
            "ln" => a.ln(),
            "log" => a.log10(),
            "abs" => a.abs(),
            "round" => a.round(),
            "floor" => a.floor(),
            "ceil" => a.ceil(),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Option<String> {
        eval_text(s)
    }

    #[test]
    fn arithmetic() {
        assert_eq!(v("2+2*3").as_deref(), Some("8"));
        assert_eq!(v("(1+2)^3").as_deref(), Some("27"));
        assert_eq!(v("2^3^2").as_deref(), Some("512"));
        assert_eq!(v("10/4").as_deref(), Some("2.5"));
        assert_eq!(v("-3 + 5").as_deref(), Some("2"));
        assert_eq!(v("7 % 4").as_deref(), Some("3"));
        assert_eq!(v("2**10").as_deref(), Some("1024"));
        assert_eq!(v("1/3").as_deref(), Some("0.3333333333"));
    }

    #[test]
    fn functions_and_constants() {
        assert_eq!(v("sqrt(16)").as_deref(), Some("4"));
        assert_eq!(v("round(pi*100)/100").as_deref(), Some("3.14"));
        assert_eq!(v("abs(-4.5)").as_deref(), Some("4.5"));
    }

    #[test]
    fn not_calculations() {
        assert_eq!(v("firefox"), None);
        assert_eq!(v("42"), None);
        assert_eq!(v("pi"), None);
        assert_eq!(v("-5"), None);
        assert_eq!(v("1/0"), None);
        assert_eq!(v("2+"), None);
        assert_eq!(v("ls -la"), None);
    }
}
