//! Parser for `${scheme:path[:key]}` secret references.
//!
//! Grammar:
//! ```text
//! value     := ( literal | reference )*
//! reference := "${" scheme ":" path ( ":" key )? "}"
//! ```
//!
//! `$$` is an escape for a literal `$`. An unmatched `${` is a
//! `BadReference` error.

use std::mem;

use crate::error::ConfigError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    Ref {
        scheme: String,
        path: String,
        key: Option<String>,
    },
}

/// Parse a config value into a sequence of literal/reference segments.
pub fn parse(input: &str) -> Result<Vec<Segment>, ConfigError> {
    let bytes = input.as_bytes();
    let mut out: Vec<Segment> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            buf.push('$');
            i += 2;
            continue;
        }
        if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // flush literal
            if !buf.is_empty() {
                out.push(Segment::Literal(mem::take(&mut buf)));
            }
            // find matching '}'
            let start = i + 2;
            let end = input[start..]
                .find('}')
                .ok_or_else(|| ConfigError::BadReference(input.to_owned()))?;
            let body = &input[start..start + end];
            out.push(parse_ref_body(body, input)?);
            i = start + end + 1;
            continue;
        }
        // bare '$' followed by something else: keep as literal
        buf.push(b as char);
        i += 1;
    }
    if !buf.is_empty() {
        out.push(Segment::Literal(buf));
    }
    Ok(out)
}

fn parse_ref_body(body: &str, full: &str) -> Result<Segment, ConfigError> {
    // scheme:path[:key]
    let (scheme, rest) = body
        .split_once(':')
        .ok_or_else(|| ConfigError::BadReference(full.to_owned()))?;
    if scheme.is_empty() {
        return Err(ConfigError::BadReference(full.to_owned()));
    }
    let (path, key) = match rest.split_once(':') {
        Some((p, k)) => (p.to_owned(), Some(k.to_owned())),
        None => (rest.to_owned(), None),
    };
    if path.is_empty() {
        return Err(ConfigError::BadReference(full.to_owned()));
    }
    Ok(Segment::Ref {
        scheme: scheme.to_owned(),
        path,
        key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_literal() {
        assert_eq!(
            parse("hello").unwrap(),
            vec![Segment::Literal("hello".into())]
        );
    }

    #[test]
    fn single_ref_no_key() {
        assert_eq!(
            parse("${env:FOO}").unwrap(),
            vec![Segment::Ref {
                scheme: "env".into(),
                path: "FOO".into(),
                key: None
            }]
        );
    }

    #[test]
    fn single_ref_with_key() {
        assert_eq!(
            parse("${file:/etc/x.toml:db.password}").unwrap(),
            vec![Segment::Ref {
                scheme: "file".into(),
                path: "/etc/x.toml".into(),
                key: Some("db.password".into())
            }]
        );
    }

    #[test]
    fn mixed_literal_and_refs() {
        let s = parse("https://${env:HOST}/v1/${env:VER}").unwrap();
        assert_eq!(s.len(), 4);
        assert!(matches!(&s[0], Segment::Literal(l) if l == "https://"));
        assert!(matches!(&s[2], Segment::Literal(l) if l == "/v1/"));
    }

    #[test]
    fn dollar_escape() {
        assert_eq!(
            parse("price: $$5").unwrap(),
            vec![Segment::Literal("price: $5".into())]
        );
    }

    #[test]
    fn unmatched_brace() {
        assert!(matches!(
            parse("${env:FOO"),
            Err(ConfigError::BadReference(_))
        ));
    }

    #[test]
    fn empty_scheme() {
        assert!(matches!(
            parse("${:FOO}"),
            Err(ConfigError::BadReference(_))
        ));
    }
}
