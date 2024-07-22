use std::collections::HashMap;

pub(crate) struct UserMap {
    map: HashMap<Vec<u8>, Vec<UserMapEntry>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UserMapEntry {
    rev_range: std::ops::RangeInclusive<u32>,
    name: String,
    email: String,
}

pub(crate) enum AuthorMapParseError {
    Io(std::io::Error),
    BadLine(usize, Vec<u8>),
}

impl From<std::io::Error> for AuthorMapParseError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for AuthorMapParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Io(ref e) => e.fmt(f),
            Self::BadLine(line, ref line_data) => {
                write!(f, "bad line {}: \"{}\"", line + 1, line_data.escape_ascii())
            }
        }
    }
}

impl UserMap {
    pub(crate) fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub(crate) fn parse(src: &mut dyn std::io::BufRead) -> Result<Self, AuthorMapParseError> {
        let mut map = HashMap::<Vec<_>, Vec<_>>::new();

        let mut line_i = 0;
        let mut line = Vec::new();
        loop {
            line.clear();
            src.read_until(b'\n', &mut line)?;

            match parse_line(&line) {
                Some(Some((user, entry))) => {
                    map.entry(user.to_vec()).or_default().push(entry);
                }
                Some(None) => {}
                None => return Err(AuthorMapParseError::BadLine(line_i, line)),
            }

            if !line.ends_with(b"\n") {
                break;
            }

            line_i += 1;
        }

        Ok(Self { map })
    }

    pub(crate) fn get(&self, user: &[u8], rev: u32) -> Option<(&str, &str)> {
        self.map
            .get(user)
            .and_then(|entries| entries.iter().find(|entry| entry.rev_range.contains(&rev)))
            .map(|entry| (entry.name.as_str(), entry.email.as_str()))
    }
}

fn parse_line(line: &[u8]) -> Option<Option<(Vec<u8>, UserMapEntry)>> {
    let mut rem = line;
    rem = rem.strip_suffix(b"\n").unwrap_or(rem);
    rem = rem.strip_suffix(b"\r").unwrap_or(rem);
    skip_spaces(&mut rem);

    if rem.is_empty() {
        return Some(None);
    }

    let user_len = rem
        .iter()
        .position(|&b| matches!(b, b' ' | b'\t' | b'@' | b'='))
        .filter(|&l| l != 0)?;

    let user = rem[..user_len].to_vec();
    rem = &rem[user_len..];

    skip_spaces(&mut rem);

    let mut rev_range = 0..=u32::MAX;

    if let Some(new_rem) = rem.strip_prefix(b"@") {
        rem = new_rem;

        let num_len = rem
            .iter()
            .position(|&b| matches!(b, b' ' | b'\t' | b':' | b'='))?;
        let start_rev = std::str::from_utf8(&rem[..num_len]).ok()?.parse().ok()?;
        rem = &rem[num_len..];

        let mut end_rev = start_rev;
        if let Some(new_rem) = rem.strip_prefix(b":") {
            rem = new_rem;

            let num_len = rem.iter().position(|&b| matches!(b, b' ' | b'\t' | b'='))?;
            end_rev = std::str::from_utf8(&rem[..num_len]).ok()?.parse().ok()?;
            rem = &rem[num_len..];
        }

        rev_range = start_rev..=end_rev;

        skip_spaces(&mut rem);
    }

    rem = rem.strip_prefix(b"=")?;

    let name_len = rem.iter().position(|&b| b == b'<')?;
    let name = String::from(std::str::from_utf8(&rem[..name_len]).ok()?.trim());
    rem = &rem[name_len..];

    rem = rem.strip_prefix(b"<").unwrap();
    let email_len = rem.iter().position(|&b| b == b'>')?;
    let email = String::from(std::str::from_utf8(&rem[..email_len]).ok()?);
    rem = &rem[email_len..];

    rem = rem.strip_prefix(b">").unwrap();
    if !rem.iter().all(|&b| matches!(b, b' ' | b'\t')) {
        return None;
    }

    Some(Some((
        user,
        UserMapEntry {
            rev_range,
            name,
            email,
        },
    )))
}

fn skip_spaces(slice: &mut &[u8]) {
    loop {
        if let Some(rem) = slice.strip_prefix(b" ") {
            *slice = rem;
        } else if let Some(rem) = slice.strip_prefix(b"\t") {
            *slice = rem;
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod test {
    use super::{parse_line, UserMapEntry};

    #[test]
    fn test_parse_line() {
        assert_eq!(
            parse_line(b" user = User Name <user@email> "),
            Some(Some((
                b"user".to_vec(),
                UserMapEntry {
                    rev_range: 0..=u32::MAX,
                    name: "User Name".into(),
                    email: "user@email".into(),
                }
            ))),
        );
        assert_eq!(
            parse_line(b"user=User Name<user@email>"),
            Some(Some((
                b"user".to_vec(),
                UserMapEntry {
                    rev_range: 0..=u32::MAX,
                    name: "User Name".into(),
                    email: "user@email".into(),
                }
            ))),
        );
        assert_eq!(
            parse_line(b"user @1 = User Name <user@email>"),
            Some(Some((
                b"user".to_vec(),
                UserMapEntry {
                    rev_range: 1..=1,
                    name: "User Name".into(),
                    email: "user@email".into(),
                }
            ))),
        );
        assert_eq!(
            parse_line(b"user@1= User Name <user@email>"),
            Some(Some((
                b"user".to_vec(),
                UserMapEntry {
                    rev_range: 1..=1,
                    name: "User Name".into(),
                    email: "user@email".into(),
                }
            ))),
        );
        assert_eq!(
            parse_line(b"user @1:2 = User Name <user@email>"),
            Some(Some((
                b"user".to_vec(),
                UserMapEntry {
                    rev_range: 1..=2,
                    name: "User Name".into(),
                    email: "user@email".into(),
                }
            ))),
        );
        assert_eq!(
            parse_line(b"user@1:2= User Name <user@email>"),
            Some(Some((
                b"user".to_vec(),
                UserMapEntry {
                    rev_range: 1..=2,
                    name: "User Name".into(),
                    email: "user@email".into(),
                }
            ))),
        );
    }
}
