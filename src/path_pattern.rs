use regex_syntax::hir as regex_hir;

#[derive(Debug)]
pub(crate) enum ParseError {
    InvalidDoubleAsterisk,
    UnallowedSlash,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDoubleAsterisk => write!(f, "invalid '**'"),
            Self::UnallowedSlash => write!(f, "unallowed '/'"),
        }
    }
}

fn pattern_to_hir(pattern: &str, full_path: bool) -> Result<regex_hir::Hir, ParseError> {
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
            if !full_path {
                return Err(ParseError::InvalidDoubleAsterisk);
            }
            // ".*"
            hir.push(regex_hir::Hir::repetition(regex_hir::Repetition {
                min: 0,
                max: None,
                greedy: true,
                sub: Box::new(regex_hir::Hir::dot(regex_hir::Dot::AnyByte)),
            }));
            rem = "";
        } else if let Some(new_rem) = rem.strip_prefix("**/") {
            if !full_path {
                return Err(ParseError::InvalidDoubleAsterisk);
            }
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
                        if !full_path {
                            return Err(ParseError::UnallowedSlash);
                        }
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
    regex: Option<regex_automata::meta::Regex>,
}

impl Default for PathPattern {
    fn default() -> Self {
        let empty: [&str; 0] = [];
        Self::new(empty, false).unwrap()
    }
}

impl PathPattern {
    pub(crate) fn new<'a>(
        patterns: impl IntoIterator<Item = &'a str>,
        full_path: bool,
    ) -> Result<Self, (&'a str, ParseError)> {
        let mut hirs = Vec::new();
        for pattern in patterns {
            hirs.push(pattern_to_hir(pattern, full_path).map_err(|e| (pattern, e))?);
        }

        if hirs.is_empty() {
            Ok(Self { regex: None })
        } else {
            let regex = regex_automata::meta::Builder::new()
                .build_many_from_hir(&hirs)
                .expect("failed to build regex");

            Ok(Self { regex: Some(regex) })
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.regex.is_none()
    }

    pub(crate) fn is_match(&self, input: &[u8]) -> bool {
        self.regex
            .as_ref()
            .is_some_and(|regex| regex.is_match(input))
    }
}

#[cfg(test)]
mod tests {
    use super::PathPattern;

    #[test]
    fn test_pattern_1() {
        let pattern = PathPattern::new(["*.foo"], false).unwrap();

        assert!(pattern.is_match(b".foo"));
        assert!(pattern.is_match(b"1.foo"));
        assert!(pattern.is_match(b"2.foo"));
        assert!(pattern.is_match(b"100.foo"));

        assert!(!pattern.is_match(b"1.bar"));
    }

    #[test]
    fn test_pattern_2() {
        let pattern = PathPattern::new(["fo?"], false).unwrap();

        assert!(pattern.is_match(b"foo"));
        assert!(pattern.is_match(b"fo1"));
        assert!(pattern.is_match(b"fo2"));

        assert!(!pattern.is_match(b"boo"));
        assert!(!pattern.is_match(b"fooo"));
        assert!(!pattern.is_match(b"ffoo"));
    }

    #[test]
    fn test_pattern_3() {
        let pattern = PathPattern::new(["foo", "bar"], false).unwrap();

        assert!(pattern.is_match(b"foo"));
        assert!(pattern.is_match(b"bar"));

        assert!(!pattern.is_match(b"foobar"));
    }

    #[test]
    fn test_pattern_full_1() {
        let pattern = PathPattern::new(["**/foo/**/bar"], true).unwrap();

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
    fn test_pattern_full_2() {
        let pattern = PathPattern::new(["foo"], true).unwrap();

        assert!(pattern.is_match(b"foo"));

        assert!(!pattern.is_match(b"fooo"));
        assert!(!pattern.is_match(b"ffoo"));
        assert!(!pattern.is_match(b"foo/1"));
        assert!(!pattern.is_match(b"1/foo"));
    }

    #[test]
    fn test_pattern_full_3() {
        let pattern = PathPattern::new(["fo?"], true).unwrap();

        assert!(pattern.is_match(b"foo"));
        assert!(pattern.is_match(b"fo1"));
        assert!(pattern.is_match(b"fo2"));

        assert!(!pattern.is_match(b"boo"));
        assert!(!pattern.is_match(b"fooo"));
        assert!(!pattern.is_match(b"ffoo"));
        assert!(!pattern.is_match(b"foo/1"));
        assert!(!pattern.is_match(b"fo/"));
    }

    #[test]
    fn test_pattern_full_4() {
        let pattern = PathPattern::new(["*.foo"], true).unwrap();

        assert!(pattern.is_match(b".foo"));
        assert!(pattern.is_match(b"1.foo"));
        assert!(pattern.is_match(b"2.foo"));
        assert!(pattern.is_match(b"100.foo"));

        assert!(!pattern.is_match(b"1.bar"));
        assert!(!pattern.is_match(b"/.foo"));
        assert!(!pattern.is_match(b"/1.foo"));
        assert!(!pattern.is_match(b"1/2.foo"));
    }

    #[test]
    fn test_pattern_full_5() {
        let pattern = PathPattern::new(["foo", "bar"], true).unwrap();

        assert!(pattern.is_match(b"foo"));
        assert!(pattern.is_match(b"bar"));

        assert!(!pattern.is_match(b"foobar"));
    }
}
