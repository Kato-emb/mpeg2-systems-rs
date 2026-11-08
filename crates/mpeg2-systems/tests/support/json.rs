//! Just enough JSON to read what ffprobe, tstables and tsanalyze print

use std::iter::Peekable;
use std::str::Chars;

/// A JSON value, its numbers kept as the text the tool printed
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Reads `text`, which a tool printed and holds one JSON value
    pub(crate) fn parse(text: &str) -> Self {
        let mut chars = text.chars().peekable();
        let value = value(&mut chars);
        skip_whitespace(&mut chars);
        assert_eq!(chars.next(), None, "text after the JSON value");
        value
    }

    /// The member `key` of an object, or `None`
    pub(crate) fn get(&self, key: &str) -> Option<&Self> {
        let Self::Object(members) = self else {
            return None;
        };
        members
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    /// The elements of an array, or none
    pub(crate) fn elements(&self) -> &[Self] {
        if let Self::Array(elements) = self {
            elements
        } else {
            &[]
        }
    }

    /// The text of a string
    pub(crate) fn as_str(&self) -> Option<&str> {
        if let Self::String(text) = self {
            Some(text)
        } else {
            None
        }
    }

    /// An integer the tool printed as a number, or as a string in decimal or
    /// in hexadecimal after `0x`
    pub(crate) fn as_u64(&self) -> Option<u64> {
        let (Self::Number(text) | Self::String(text)) = self else {
            return None;
        };
        match text.strip_prefix("0x") {
            Some(hexadecimal) => u64::from_str_radix(hexadecimal, 16).ok(),
            None => text.parse().ok(),
        }
    }

    /// Every object in this value, at any depth, whose `#name` is `name`, in
    /// the order the tool printed them
    ///
    /// TSDuck prints an XML element as an object holding its name as `#name`.
    pub(crate) fn named(&self, name: &str) -> Vec<&Self> {
        let children: Vec<&Self> = match self {
            Self::Array(elements) => elements.iter().collect(),
            Self::Object(members) => members.iter().map(|(_, value)| value).collect(),
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) => Vec::new(),
        };
        let itself = self.get("#name").and_then(Self::as_str) == Some(name);
        itself
            .then_some(self)
            .into_iter()
            .chain(children.into_iter().flat_map(|child| child.named(name)))
            .collect()
    }
}

fn skip_whitespace(chars: &mut Peekable<Chars<'_>>) {
    while chars.next_if(|char| char.is_ascii_whitespace()).is_some() {}
}

fn value(chars: &mut Peekable<Chars<'_>>) -> Json {
    skip_whitespace(chars);
    match chars.next().expect("the text ends before a value") {
        '{' => Json::Object(sequence(chars, '}', |chars| {
            skip_whitespace(chars);
            assert_eq!(chars.next(), Some('"'), "an object member without a name");
            let name = string(chars);
            skip_whitespace(chars);
            assert_eq!(chars.next(), Some(':'), "a member name without a colon");
            (name, value(chars))
        })),
        '[' => Json::Array(sequence(chars, ']', value)),
        '"' => Json::String(string(chars)),
        't' => literal(chars, "true", Json::Bool(true)),
        'f' => literal(chars, "false", Json::Bool(false)),
        'n' => literal(chars, "null", Json::Null),
        first => {
            let mut number = String::from(first);
            while let Some(char) =
                chars.next_if(|char| "+-.eE".contains(*char) || char.is_ascii_digit())
            {
                number.push(char);
            }
            Json::Number(number)
        }
    }
}

/// The elements up to `close`, separated by commas, the opening bracket read
fn sequence<T>(
    chars: &mut Peekable<Chars<'_>>,
    close: char,
    mut element: impl FnMut(&mut Peekable<Chars<'_>>) -> T,
) -> Vec<T> {
    let mut elements = Vec::new();
    skip_whitespace(chars);
    if chars.next_if_eq(&close).is_some() {
        return elements;
    }
    loop {
        elements.push(element(chars));
        skip_whitespace(chars);
        let separator = chars.next();
        if separator != Some(',') {
            assert_eq!(
                separator,
                Some(close),
                "neither a comma nor the closing bracket"
            );
            return elements;
        }
    }
}

/// The rest of a string, the opening quote read
fn string(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut text = String::new();
    loop {
        match chars.next().expect("the text ends inside a string") {
            '"' => return text,
            '\\' => match chars.next().expect("the text ends inside an escape") {
                'n' => text.push('\n'),
                't' => text.push('\t'),
                'r' => text.push('\r'),
                'b' => text.push('\u{8}'),
                'f' => text.push('\u{c}'),
                'u' => {
                    let code: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&code, 16).unwrap();
                    text.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                }
                escaped => text.push(escaped),
            },
            char => text.push(char),
        }
    }
}

/// `value`, which `word` spells, the first letter of `word` read
fn literal(chars: &mut Peekable<Chars<'_>>, word: &str, value: Json) -> Json {
    let spelled = word
        .chars()
        .skip(1)
        .all(|letter| chars.next() == Some(letter));
    assert!(spelled, "a literal other than {word}");
    value
}
