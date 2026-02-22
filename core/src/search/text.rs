use crate::Error;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    All(Vec<Expr>),
    Any(Vec<Expr>),
    Term(String),
}

impl Expr {
    pub fn matches(&self, document: &str) -> bool {
        match self {
            Expr::All(parts) => parts.iter().all(|part| part.matches(document)),
            Expr::Any(parts) => parts.iter().any(|part| part.matches(document)),
            Expr::Term(term) => has_word(document, term),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextQuery {
    pub expr: Expr,
}

impl TextQuery {
    pub fn matches(&self, document: &str) -> bool {
        self.expr.matches(document)
    }
}

pub fn text_query(raw: &str) -> Result<TextQuery, Error> {
    let tokens = tokens(raw);
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.expr()?;
    if parser.pos != parser.tokens.len() {
        return Err(malformed(raw));
    }
    Ok(TextQuery { expr })
}

fn malformed(raw: &str) -> Error {
    Error::InvalidParameter(format!("text query {raw:?} is malformed"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    Word(String),
}

fn tokens(raw: &str) -> Vec<Token> {
    let mut found = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, found: &mut Vec<Token>| {
        let held = std::mem::take(word);
        let folded = normalize(&held);
        if !folded.is_empty() {
            found.push(Token::Word(folded));
        }
    };
    for ch in raw.chars() {
        match ch {
            '(' => {
                flush(&mut word, &mut found);
                found.push(Token::Open);
            }
            ')' => {
                flush(&mut word, &mut found);
                found.push(Token::Close);
            }
            other if other.is_whitespace() => flush(&mut word, &mut found),
            other => word.push(other),
        }
    }
    flush(&mut word, &mut found);
    found
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn expr(&mut self) -> Result<Expr, Error> {
        let mut operands = vec![self.and()?];
        while self.take_word("or").is_some() {
            operands.push(self.and()?);
        }
        Ok(match operands.len() {
            1 => operands.pop().expect("at least one operand"),
            _ => Expr::Any(operands),
        })
    }

    fn and(&mut self) -> Result<Expr, Error> {
        let mut operands = vec![self.atom()?];
        loop {
            match self.peek() {
                Some(Token::Word(word)) if word == "and" => {
                    self.pos += 1;
                    operands.push(self.atom()?);
                }
                Some(Token::Word(word)) if word != "or" => operands.push(self.atom()?),
                _ => break,
            }
        }
        Ok(match operands.len() {
            1 => operands.pop().expect("at least one operand"),
            _ => Expr::All(operands),
        })
    }

    fn atom(&mut self) -> Result<Expr, Error> {
        match self.peek() {
            None => Err(malformed("the query")),
            Some(Token::Close) => Err(malformed("the query")),
            Some(Token::Open) => {
                self.pos += 1;
                let inner = self.expr()?;
                if matches!(self.peek(), Some(Token::Close)) {
                    self.pos += 1;
                    Ok(inner)
                } else {
                    Err(malformed("the query"))
                }
            }
            Some(Token::Word(word)) if word == "and" || word == "or" => Err(malformed("the query")),
            Some(Token::Word(word)) => {
                let term = word.clone();
                self.pos += 1;
                Ok(Expr::Term(term))
            }
        }
    }

    fn take_word(&mut self, wanted: &str) -> Option<String> {
        match self.peek() {
            Some(Token::Word(word)) if word == wanted => {
                let held = word.clone();
                self.pos += 1;
                Some(held)
            }
            _ => None,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }
}

fn has_word(document: &str, term: &str) -> bool {
    let mut word = String::new();
    for ch in document.chars() {
        if ch.is_alphanumeric() {
            word.extend(ch.to_lowercase());
        } else {
            if word == term {
                return true;
            }
            word.clear();
        }
    }
    word == term
}

pub fn visible(value: &Value) -> String {
    let mut gathered = String::new();
    gather(value, &mut gathered);
    normalize(&stripped(&gathered))
}

fn gather(value: &Value, out: &mut String) {
    match value {
        Value::String(text) => out.push_str(text),
        Value::Array(items) => items.iter().for_each(|item| gather(item, out)),
        Value::Object(map) => {
            if let Some(Value::String(div)) = map.get("div") {
                out.push_str(div);
                return;
            }
            for (name, nested) in map {
                if name != "status" {
                    gather(nested, out);
                }
            }
        }
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn stripped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(current) = chars.next() {
        if current == '<' {
            for ch in chars.by_ref() {
                if ch == '>' {
                    break;
                }
            }
        } else {
            out.push(current);
        }
    }
    for (from, to) in [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&amp;", "&"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ] {
        out = out.replace(from, to);
    }
    out
}

pub fn normalize(text: &str) -> String {
    let mut words = Vec::new();
    let mut word = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            word.extend(ch.to_lowercase());
        } else {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn query(raw: &str) -> TextQuery {
        text_query(raw).expect("a query parses")
    }

    #[test]
    fn a_single_word_matches_the_word_of_a_document() {
        assert!(query("fever").matches("the patient had a fever"));
        assert!(query("fever").matches("FEVER and chills"));
        assert!(!query("fever").matches("the patient was feverish"));
        assert!(!query("metastases").matches("metastatic disease"));
    }

    #[test]
    fn a_term_does_not_match_part_of_a_word() {
        assert!(!query("bone").matches("season"));
        assert!(query("bone").matches("the femur bone"));
        assert!(query("bone").matches("bone, rib and skull"));
    }

    #[test]
    fn words_are_joined_by_an_implicit_and() {
        let matches = query("bone liver");
        assert!(matches.matches("metastases of the bone and liver"));
        assert!(!matches.matches("a bone fracture"));
    }

    #[test]
    fn an_operator_is_reserved_and_case_insensitive() {
        let any = query("bone OR liver");
        assert!(any.matches("liver metastases"));
        assert!(any.matches("a bone fracture"));
        assert!(query("bone or liver").matches("liver metastases"));
        let both = query("bone AND liver");
        assert!(both.matches("disease of the bone and liver"));
        assert!(!both.matches("liver metastases"));
    }

    #[test]
    fn parentheses_override_the_precedence() {
        let grouped = query("(bone OR liver) AND metastases");
        assert!(grouped.matches("bone metastases"));
        assert!(grouped.matches("liver metastases"));
        assert!(!grouped.matches("bone and liver"));
        assert!(!grouped.matches("metastases"));
        let expected = query("a OR (b AND c)");
        assert!(expected.matches("a"));
        assert!(expected.matches("b and c"));
        assert!(!expected.matches("b"));
        assert!(!expected.matches("c"));
    }

    #[test]
    fn or_binds_looser_than_and() {
        let mixed = query("a OR b AND c");
        assert!(mixed.matches("b and c"));
        assert!(!mixed.matches("b only"));
        assert!(mixed.matches("a only"));
    }

    #[test]
    fn punctuation_in_a_document_is_boundary_only() {
        assert!(query("liver").matches("the liver, the bone"));
        assert!(query("chills").matches("chills."));
        assert!(!query("liver").matches("the liverish cough"));
    }

    #[test]
    fn a_malformed_expression_is_refused() {
        for raw in ["", "bone OR", "(", "(bone", "bone)", "()", "AND", "OR"] {
            assert!(text_query(raw).is_err(), "{raw:?}");
        }
    }

    #[test]
    fn narrative_markup_is_stripped_to_words() {
        let narrative = json!({
            "status": "generated",
            "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\"><p><b>Bone</b> &amp; liver <i>metastases</i></p></div>"
        });
        assert_eq!(visible(&narrative), "bone liver metastases");
        assert!(query("metastases").matches(&visible(&narrative)));
        assert!(!query("div").matches(&visible(&narrative)));
    }

    #[test]
    fn a_narrative_without_markup_is_read_as_is() {
        let narrative = json!({"status": "generated", "div": "Fever and chills"});
        assert_eq!(visible(&narrative), "fever and chills");
    }

    #[test]
    fn a_string_element_is_its_own_visible_text() {
        let value = json!("Body Temperature, rectal");
        assert_eq!(visible(&value), "body temperature rectal");
    }
}
