mod delta;
mod import;

pub(crate) use import::{ImportFinishProgress, Importer, TreeBuilder};

pub(crate) fn legalize_branch_name(raw_name: &[u8]) -> String {
    fn legalize_component(name: &mut String) {
        if name.ends_with(".lock") {
            name.truncate(name.len() - 5);
            name.push_str("_lock");
        } else if name.ends_with('.') {
            name.truncate(name.len() - 1);
            name.push('_');
        } else if name == "refs" {
            name.push('_');
        }
    }

    let mut legal_name = String::with_capacity(raw_name.len());
    for chr in String::from_utf8_lossy(raw_name).chars() {
        if chr == '/' {
            if !legal_name.ends_with('/') && !legal_name.is_empty() {
                legalize_component(&mut legal_name);
                legal_name.push('/');
            }
        } else {
            let disallowed_chr = matches!(
                chr,
                '\0'..=' '
                    | '*'
                    | ':'
                    | '?'
                    | '['
                    | '\\'
                    | ']'
                    | '^'
                    | '{'
                    | '}'
                    | '~'..
            );
            if disallowed_chr
                || ((legal_name.ends_with('/')
                    || legal_name.is_empty()
                    || legal_name.ends_with('.'))
                    && chr == '.')
                || (legal_name.is_empty() && chr == '-')
            {
                legal_name.push('_');
            } else {
                legal_name.push(chr);
            }
        }
    }

    if legal_name.ends_with('/') {
        legal_name.truncate(legal_name.len() - 1);
    }
    legalize_component(&mut legal_name);
    if legal_name.is_empty() {
        legal_name.push('_');
    }

    legal_name
}
