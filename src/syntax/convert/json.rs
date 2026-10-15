//! A minimal JSON reader, just enough to load TextMate grammars (`*.tmLanguage.json`).
//! Hand-written to avoid pulling a serialization framework into the editor.

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    /// Keys in file order.
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(source: &str) -> Result<Json, String> {
        let mut parser = Parser { chars: source.char_indices().peekable(), source };
        let value = parser.value()?;
        parser.skip_whitespace();
        match parser.chars.next() {
            None => Ok(value),
            Some((i, c)) => Err(format!("unexpected `{c}` at byte {i}")),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(fields) => Some(fields),
            _ => None,
        }
    }

    /// `get(key)` as a string, if both exist.
    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Json::as_str)
    }
}

struct Parser<'a> {
    source: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl<'a> Parser<'a> {
    fn skip_whitespace(&mut self) {
        while matches!(self.chars.peek(), Some((_, c)) if c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), String> {
        match self.chars.next() {
            Some((_, c)) if c == expected => Ok(()),
            Some((i, c)) => Err(format!("expected `{expected}` but found `{c}` at byte {i}")),
            None => Err(format!("expected `{expected}` but reached the end")),
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.skip_whitespace();
        match self.chars.peek().copied() {
            None => Err("unexpected end of input".to_string()),
            Some((_, '{')) => self.object(),
            Some((_, '[')) => self.array(),
            Some((_, '"')) => Ok(Json::String(self.string()?)),
            Some((_, 't')) => self.literal("true", Json::Bool(true)),
            Some((_, 'f')) => self.literal("false", Json::Bool(false)),
            Some((_, 'n')) => self.literal("null", Json::Null),
            Some((i, c)) if c == '-' || c.is_ascii_digit() => self.number(i),
            Some((i, c)) => Err(format!("unexpected `{c}` at byte {i}")),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        for expected in word.chars() {
            self.expect(expected)?;
        }
        Ok(value)
    }

    fn number(&mut self, start: usize) -> Result<Json, String> {
        let mut end = start;
        while let Some(&(i, c)) = self.chars.peek() {
            if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E') {
                end = i + c.len_utf8();
                self.chars.next();
            } else {
                break;
            }
        }
        self.source[start..end].parse().map(Json::Number).map_err(|_| format!("bad number at byte {start}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.chars.next() {
                None => return Err("unterminated string".to_string()),
                Some((_, '"')) => return Ok(out),
                Some((_, '\\')) => match self.chars.next() {
                    Some((_, '"')) => out.push('"'),
                    Some((_, '\\')) => out.push('\\'),
                    Some((_, '/')) => out.push('/'),
                    Some((_, 'b')) => out.push('\u{8}'),
                    Some((_, 'f')) => out.push('\u{c}'),
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 'r')) => out.push('\r'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, 'u')) => out.push(self.unicode_escape()?),
                    Some((i, c)) => return Err(format!("bad escape `\\{c}` at byte {i}")),
                    None => return Err("unterminated string".to_string()),
                },
                Some((_, c)) => out.push(c),
            }
        }
    }

    /// The four hex digits after `\u`, joined with a following `\uDC00`-style
    /// low surrogate when the first one is a high surrogate.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        if (0xD800..0xDC00).contains(&first) {
            // Surrogate pair: expect `\uXXXX` right after.
            self.expect('\\')?;
            self.expect('u')?;
            let second = self.hex4()?;
            let code = 0x10000 + ((first - 0xD800) << 10) + (second.wrapping_sub(0xDC00) & 0x3FF);
            return char::from_u32(code).ok_or_else(|| "bad surrogate pair".to_string());
        }
        char::from_u32(first).ok_or_else(|| format!("bad unicode escape {first:x}"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = self.chars.next().and_then(|(_, c)| c.to_digit(16)).ok_or("bad \\u escape")?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if matches!(self.chars.peek(), Some((_, ']'))) {
            self.chars.next();
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_whitespace();
            match self.chars.next() {
                Some((_, ',')) => continue,
                Some((_, ']')) => return Ok(Json::Array(items)),
                _ => return Err("expected `,` or `]` in array".to_string()),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect('{')?;
        let mut fields = Vec::new();
        self.skip_whitespace();
        if matches!(self.chars.peek(), Some((_, '}'))) {
            self.chars.next();
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(':')?;
            let value = self.value()?;
            fields.push((key, value));
            self.skip_whitespace();
            match self.chars.next() {
                Some((_, ',')) => continue,
                Some((_, '}')) => return Ok(Json::Object(fields)),
                _ => return Err("expected `,` or `}` in object".to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_values_and_escapes() {
        let json = Json::parse(r#"{"a": [1, -2.5e1, true, null], "s": "x\n\"q\" é 😀", "o": {}}"#).unwrap();
        assert_eq!(json.get("a").unwrap().as_array().unwrap().len(), 4);
        assert_eq!(json.get("a").unwrap().as_array().unwrap()[1], Json::Number(-25.0));
        assert_eq!(json.str_field("s"), Some("x\n\"q\" é 😀"));
        assert_eq!(json.get("o"), Some(&Json::Object(vec![])));
        assert!(Json::parse("[1,]").is_err());
        assert!(Json::parse("{\"a\" 1}").is_err());
    }
}
