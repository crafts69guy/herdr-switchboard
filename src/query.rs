//! Shared field-filter and fuzzy query language for Commands and Ports.

use std::collections::HashMap;
use std::ops::Range;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    Exact,
    Contains,
}

#[derive(Clone, Debug, Default)]
pub struct FieldSchema {
    fields: HashMap<String, MatchKind>,
    aliases: HashMap<String, String>,
}

impl FieldSchema {
    pub fn new(fields: &[(&str, MatchKind)], aliases: &[(&str, &str)]) -> Self {
        Self {
            fields: fields
                .iter()
                .map(|(name, kind)| ((*name).to_string(), *kind))
                .collect(),
            aliases: aliases
                .iter()
                .map(|(alias, target)| ((*alias).to_string(), (*target).to_string()))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub fuzzy: String,
    pub fields: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryDiagnostic {
    pub message: String,
    pub span: Range<usize>,
}

#[derive(Clone, Debug)]
struct Filter {
    field: String,
    value: String,
    kind: MatchKind,
    negated: bool,
}

#[derive(Clone, Debug)]
pub struct CompiledQuery {
    filters: Vec<Filter>,
    fuzzy: Option<Pattern>,
}

impl CompiledQuery {
    pub fn compile(raw: &str, schema: &FieldSchema) -> Result<Self, QueryDiagnostic> {
        let mut filters = Vec::new();
        let mut fuzzy = Vec::new();
        for token in lex(raw)? {
            let (negated, text) = token
                .text
                .strip_prefix('-')
                .map(|text| (true, text))
                .unwrap_or((false, token.text.as_str()));
            let Some((raw_field, value)) = text.split_once(':') else {
                fuzzy.push(token.text);
                continue;
            };
            if raw_field.is_empty() || value.is_empty() {
                return Err(QueryDiagnostic {
                    message: "filter requires field and value".into(),
                    span: token.span,
                });
            }
            let field = schema
                .aliases
                .get(raw_field)
                .map(String::as_str)
                .unwrap_or(raw_field);
            let Some(kind) = schema.fields.get(field).copied() else {
                return Err(QueryDiagnostic {
                    message: format!("unknown field `{raw_field}`"),
                    span: token.span,
                });
            };
            filters.push(Filter {
                field: field.to_string(),
                value: value.to_lowercase(),
                kind,
                negated,
            });
        }
        Ok(Self {
            filters,
            fuzzy: (!fuzzy.is_empty()).then(|| {
                Pattern::parse(&fuzzy.join(" "), CaseMatching::Smart, Normalization::Smart)
            }),
        })
    }

    pub fn score(&self, document: &Document, matcher: &mut Matcher) -> Option<u32> {
        for filter in &self.filters {
            let candidate = document
                .fields
                .get(&filter.field)
                .map(|value| value.to_lowercase());
            let matched = candidate.is_some_and(|candidate| match filter.kind {
                MatchKind::Exact => candidate.split(',').any(|part| part.trim() == filter.value),
                MatchKind::Contains => candidate.contains(&filter.value),
            });
            if matched == filter.negated {
                return None;
            }
        }
        let Some(pattern) = &self.fuzzy else {
            return Some(0);
        };
        let mut buf = Vec::new();
        pattern.score(Utf32Str::new(&document.fuzzy, &mut buf), matcher)
    }
}

#[derive(Debug)]
struct Token {
    text: String,
    span: Range<usize>,
}

fn lex(raw: &str) -> Result<Vec<Token>, QueryDiagnostic> {
    let mut tokens = Vec::new();
    let mut chars = raw.char_indices().peekable();
    while let Some((start, first)) = chars.next() {
        if first.is_whitespace() {
            continue;
        }
        let mut text = String::new();
        let mut quoted = false;
        let mut end = start + first.len_utf8();
        if first == '"' {
            quoted = true;
        } else {
            text.push(first);
        }
        while let Some(&(index, ch)) = chars.peek() {
            if ch.is_whitespace() && !quoted {
                break;
            }
            chars.next();
            end = index + ch.len_utf8();
            if ch == '"' {
                quoted = !quoted;
            } else {
                text.push(ch);
            }
        }
        if quoted {
            return Err(QueryDiagnostic {
                message: "unterminated quote".into(),
                span: start..raw.len(),
            });
        }
        tokens.push(Token {
            text,
            span: start..end,
        });
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nucleo_matcher::Config;

    fn commands() -> FieldSchema {
        FieldSchema::new(
            &[
                ("command", MatchKind::Contains),
                ("cwd", MatchKind::Contains),
                ("source", MatchKind::Exact),
            ],
            &[("cmd", "command")],
        )
    }

    #[test]
    fn filters_and_fuzzy_text_match_together() {
        let query = CompiledQuery::compile(
            r#"cmd:"cargo test" -cwd:archive source:preset deploy"#,
            &commands(),
        )
        .expect("valid query");
        let document = Document {
            fuzzy: "deploy workspace cargo test".into(),
            fields: HashMap::from([
                ("command".into(), "cargo test --workspace".into()),
                ("cwd".into(), "/work/api".into()),
                ("source".into(), "preset".into()),
            ]),
        };

        assert!(query
            .score(&document, &mut Matcher::new(Config::DEFAULT))
            .is_some());
    }

    #[test]
    fn unknown_field_reports_the_token_span_and_fails_closed() {
        let error = CompiledQuery::compile("prot:3000", &commands()).unwrap_err();
        assert_eq!(error.span, 0..9);
        assert!(error.message.contains("unknown field `prot`"));
    }

    #[test]
    fn exact_fields_do_not_partially_match() {
        let query = CompiledQuery::compile("source:pre", &commands()).unwrap();
        let document = Document {
            fuzzy: String::new(),
            fields: HashMap::from([("source".into(), "preset".into())]),
        };
        assert_eq!(
            query.score(&document, &mut Matcher::new(Config::DEFAULT)),
            None
        );
    }

    #[test]
    fn repeated_negated_and_quoted_filters_use_and_semantics() {
        let query = CompiledQuery::compile(
            r#"source:shell source:preset -cwd:\"archive old\""#,
            &commands(),
        )
        .unwrap();
        let document = Document {
            fuzzy: String::new(),
            fields: HashMap::from([
                ("source".into(), "shell,preset".into()),
                ("cwd".into(), "/work/current".into()),
            ]),
        };
        assert!(query
            .score(&document, &mut Matcher::new(Config::DEFAULT))
            .is_some());
    }

    #[test]
    fn malformed_quote_reports_utf8_byte_span() {
        let error = CompiledQuery::compile("đẹp cmd:\"cargo", &commands()).unwrap_err();
        assert_eq!(error.span, 7..17);
        assert!(error.message.contains("unterminated quote"));
    }
}
