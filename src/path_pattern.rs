use regex_syntax::hir as regex_hir;

#[derive(Debug)]
pub(crate) enum ParseError {
    InvalidDoubleAsterisk,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDoubleAsterisk => write!(f, "invalid '**'"),
        }
    }
}

fn pattern_to_hir(pattern: &str) -> Result<regex_hir::Hir, ParseError> {
    let mut hir = Vec::new();
    hir.push(regex_hir::Hir::look(regex_hir::Look::Start));

    fn non_slash() -> regex_hir::Hir {
        regex_hir::Hir::class(regex_hir::Class::Bytes(regex_hir::ClassBytes::new([
            regex_hir::ClassBytesRange::new(u8::MIN, b'/' - 1),
            regex_hir::ClassBytesRange::new(b'/' + 1, u8::MAX),
        ])))
    }

    fn repeated_non_slash() -> regex_hir::Hir {
        regex_hir::Hir::repetition(regex_hir::Repetition {
            min: 0,
            max: None,
            greedy: true,
            sub: Box::new(non_slash()),
        })
    }

    let mut rem = pattern;
    while !rem.is_empty() {
        if rem == "**" {
            // ".*"
            hir.push(regex_hir::Hir::repetition(regex_hir::Repetition {
                min: 0,
                max: None,
                greedy: true,
                sub: Box::new(regex_hir::Hir::dot(regex_hir::Dot::AnyByte)),
            }));
            rem = "";
        } else if let Some(new_rem) = rem.strip_prefix("**/") {
            // "([^/]*/)*"
            hir.push(regex_hir::Hir::repetition(regex_hir::Repetition {
                min: 0,
                max: None,
                greedy: true,
                sub: Box::new(regex_hir::Hir::concat(vec![
                    repeated_non_slash(),
                    regex_hir::Hir::literal(b"/".as_slice()),
                ])),
            }));
            rem = new_rem;
        } else {
            let mut i = 0;
            loop {
                if let Some(&byte) = rem.as_bytes().get(i) {
                    if byte == b'/' {
                        let (chunk, new_rem) = rem.split_at(i + 1);
                        hir.push(regex_hir::Hir::literal(chunk.as_bytes()));
                        rem = new_rem;
                        break;
                    } else if byte == b'*' {
                        if rem.as_bytes().get(i + 1) == Some(&b'*') {
                            return Err(ParseError::InvalidDoubleAsterisk);
                        }

                        let chunk = &rem.as_bytes()[..i];
                        if !chunk.is_empty() {
                            hir.push(regex_hir::Hir::literal(chunk));
                        }

                        hir.push(repeated_non_slash());

                        rem = &rem[(i + 1)..];
                        i = 0;
                    } else if byte == b'?' {
                        let chunk = &rem.as_bytes()[..i];
                        if !chunk.is_empty() {
                            hir.push(regex_hir::Hir::literal(chunk));
                        }

                        hir.push(non_slash());

                        rem = &rem[(i + 1)..];
                        i = 0;
                    } else {
                        i += 1;
                    }
                } else {
                    if i != 0 {
                        hir.push(regex_hir::Hir::literal(rem.as_bytes()));
                        rem = "";
                    }
                    break;
                }
            }
        }
    }

    hir.push(regex_hir::Hir::look(regex_hir::Look::End));

    Ok(regex_hir::Hir::concat(hir))
}

pub(crate) struct PathPattern {
    regex: regex_automata::meta::Regex,
}

impl Default for PathPattern {
    fn default() -> Self {
        let empty: [&str; 0] = [];
        Self::new(empty).unwrap()
    }
}

impl PathPattern {
    pub(crate) fn new<'a>(
        patterns: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, (&'a str, ParseError)> {
        let mut hirs = Vec::new();
        for pattern in patterns {
            hirs.push(pattern_to_hir(pattern).map_err(|e| (pattern, e))?);
        }

        let regex = regex_automata::meta::Builder::new()
            .build_many_from_hir(&hirs)
            .expect("failed to build regex");

        Ok(Self { regex })
    }

    pub(crate) fn is_match(&self, input: &[u8]) -> bool {
        self.regex.is_match(input)
    }
}

#[cfg(test)]
mod tests {
    use super::PathPattern;

    #[test]
    fn test_pattern_1() {
        let pattern = PathPattern::new(["**/foo/**/bar"]).unwrap();

        assert!(pattern.is_match(b"foo/bar"));
        assert!(pattern.is_match(b"a/foo/bar"));
        assert!(pattern.is_match(b"foo/b/bar"));
        assert!(pattern.is_match(b"a/foo/b/bar"));
        assert!(pattern.is_match(b"foo/1/2/3/bar"));
        assert!(pattern.is_match(b"1/2/3/foo/bar"));

        assert!(!pattern.is_match(b"foo/bar/1"));
        assert!(!pattern.is_match(b"fooo/bar"));
        assert!(!pattern.is_match(b"fooo/baar"));
        assert!(!pattern.is_match(b"foo/barr"));
        assert!(!pattern.is_match(b"ffoo/bar"));
    }

    #[test]
    fn test_pattern_2() {
        let pattern = PathPattern::new(["foo"]).unwrap();

        assert!(pattern.is_match(b"foo"));

        assert!(!pattern.is_match(b"fooo"));
        assert!(!pattern.is_match(b"ffoo"));
        assert!(!pattern.is_match(b"foo/1"));
        assert!(!pattern.is_match(b"1/foo"));
    }

    #[test]
    fn test_pattern_3() {
        let pattern = PathPattern::new(["fo?"]).unwrap();

        assert!(pattern.is_match(b"foo"));
        assert!(pattern.is_match(b"fo1"));
        assert!(pattern.is_match(b"fo2"));

        assert!(!pattern.is_match(b"foo/1"));
        assert!(!pattern.is_match(b"fo/"));
    }

    #[test]
    fn test_pattern_4() {
        let pattern = PathPattern::new(["*.foo"]).unwrap();

        assert!(pattern.is_match(b".foo"));
        assert!(pattern.is_match(b"1.foo"));
        assert!(pattern.is_match(b"2.foo"));
        assert!(pattern.is_match(b"100.foo"));

        assert!(!pattern.is_match(b"/.foo"));
        assert!(!pattern.is_match(b"/1.foo"));
        assert!(!pattern.is_match(b"1/2.foo"));
    }

    #[test]
    fn test_pattern_5() {
        let pattern = PathPattern::new(["foo", "bar"]).unwrap();

        assert!(pattern.is_match(b"foo"));
        assert!(pattern.is_match(b"bar"));

        assert!(!pattern.is_match(b"foobar"));
    }
}
