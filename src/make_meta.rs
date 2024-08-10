use std::collections::HashMap;

use crate::convert::{GitCommitMeta, GitTagMeta};
use crate::user_map::UserMap;

pub(crate) struct GitMetaMaker<'a> {
    user_map: &'a UserMap,
    jinja_env: minijinja::Environment<'a>,
}

impl<'a> GitMetaMaker<'a> {
    pub(crate) fn new(
        user_map: &'a UserMap,
        user_fallback_template: &'a str,
        commit_msg_template: &'a str,
        tag_msg_template: &'a str,
    ) -> Result<Self, String> {
        let mut jinja_env = minijinja::Environment::empty();
        jinja_env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);

        jinja_env
            .add_template("user_fallback", user_fallback_template)
            .map_err(|e| format!("failed to parse user fallback template: {e}"))?;
        jinja_env
            .add_template("commit_msg", commit_msg_template)
            .map_err(|e| format!("failed to parse commit message template: {e}"))?;
        jinja_env
            .add_template("tag_msg", tag_msg_template)
            .map_err(|e| format!("failed to parse tag message template: {e}"))?;

        Ok(Self {
            user_map,
            jinja_env,
        })
    }
}

impl crate::convert::GitMetaMaker for GitMetaMaker<'_> {
    fn make_git_commit_meta(
        &self,
        svn_uuid: Option<&uuid::Uuid>,
        svn_rev_no: u32,
        svn_path: Option<&[u8]>,
        svn_rev_props: &HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<GitCommitMeta, String> {
        let jinja_ctx = JinjaCtx::new(svn_uuid, svn_rev_no, svn_path, svn_rev_props, self.user_map);

        let (author_name, author_email) = self.convert_author(
            &jinja_ctx,
            svn_rev_no,
            svn_rev_props
                .get(b"svn:author".as_slice())
                .map(Vec::as_slice),
        )?;

        let date = self.extract_rev_date(svn_rev_props)?;
        let git_time = gix_date::Time {
            seconds: convert_date(date.as_ref()),
            offset: 0,
            sign: gix_date::time::Sign::Plus,
        };

        let msg_template = self.jinja_env.get_template("commit_msg").unwrap();
        let message = msg_template
            .render(&jinja_ctx)
            .map_err(|e| format!("failed to render git commit message: {e}"))?
            .replace("\r\n", "\n");

        Ok(GitCommitMeta {
            author: gix_actor::Signature {
                name: author_name.clone().into(),
                email: author_email.clone().into(),
                time: git_time,
            },
            committer: gix_actor::Signature {
                name: author_name.into(),
                email: author_email.into(),
                time: git_time,
            },
            message,
        })
    }

    fn make_git_tag_meta(
        &self,
        svn_uuid: Option<&uuid::Uuid>,
        svn_rev_no: u32,
        svn_path: &[u8],
        svn_rev_props: &HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<GitTagMeta, String> {
        let jinja_ctx = JinjaCtx::new(
            svn_uuid,
            svn_rev_no,
            Some(svn_path),
            svn_rev_props,
            self.user_map,
        );

        let (author_name, author_email) = self.convert_author(
            &jinja_ctx,
            svn_rev_no,
            svn_rev_props
                .get(b"svn:author".as_slice())
                .map(Vec::as_slice),
        )?;

        let date = self.extract_rev_date(svn_rev_props)?;
        let git_time = gix_date::Time {
            seconds: convert_date(date.as_ref()),
            offset: 0,
            sign: gix_date::time::Sign::Plus,
        };

        let msg_template = self.jinja_env.get_template("tag_msg").unwrap();
        let message = msg_template
            .render(&jinja_ctx)
            .map_err(|e| format!("failed to render git commit message: {e}"))?
            .replace("\r\n", "\n");

        Ok(GitTagMeta {
            tagger: Some(gix_actor::Signature {
                name: author_name.into(),
                email: author_email.into(),
                time: git_time,
            }),
            message,
        })
    }
}

impl GitMetaMaker<'_> {
    fn convert_author(
        &self,
        jinja_ctx: &JinjaCtx,
        svn_rev_no: u32,
        svn_author: Option<&[u8]>,
    ) -> Result<(String, String), String> {
        if let Some((name, email)) =
            svn_author.and_then(|svn_author| self.user_map.get(svn_author, svn_rev_no))
        {
            Ok((name.into(), email.into()))
        } else {
            let template = self.jinja_env.get_template("user_fallback").unwrap();
            let author = template
                .render(jinja_ctx)
                .map_err(|e| format!("failed to render fallback author: {e}"))?;
            let Some((name, email)) = split_author_name_email(&author) else {
                return Err(format!(
                    "author {author:?} is not in \"name <email>\" format"
                ));
            };

            Ok((name.into(), email.into()))
        }
    }

    fn extract_rev_date(
        &self,
        svn_rev_props: &HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<Option<chrono::NaiveDateTime>, String> {
        svn_rev_props
            .get(b"svn:date".as_slice())
            .map(|raw_date| {
                std::str::from_utf8(raw_date)
                    .ok()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|date| date.naive_utc())
                    .ok_or_else(|| {
                        format!("invalid SVN revision date \"{}\"", raw_date.escape_ascii(),)
                    })
            })
            .transpose()
    }
}

#[derive(serde::Serialize)]
struct JinjaCtx {
    svn_uuid: String,
    svn_rev: u32,
    svn_author: String,
    svn_log: String,
    svn_path: String,
    mapped_author_name: String,
    mapped_author_email: String,
}

impl JinjaCtx {
    fn new(
        uuid: Option<&uuid::Uuid>,
        rev_no: u32,
        branch_path: Option<&[u8]>,
        svn_rev_props: &HashMap<Vec<u8>, Vec<u8>>,
        user_map: &UserMap,
    ) -> Self {
        let svn_author = svn_rev_props
            .get(b"svn:author".as_slice())
            .map(Vec::as_slice);
        let svn_log = svn_rev_props.get(b"svn:log".as_slice()).map(Vec::as_slice);

        let (mapped_author_name, mapped_author_email) = svn_author
            .and_then(|svn_author| {
                user_map
                    .get(svn_author, rev_no)
                    .map(|(name, email)| (String::from(name), String::from(email)))
            })
            .unwrap_or_default();

        Self {
            svn_uuid: uuid.map(ToString::to_string).unwrap_or_default(),
            svn_rev: rev_no,
            svn_log: String::from_utf8_lossy(svn_log.unwrap_or_default()).into_owned(),
            svn_author: String::from_utf8_lossy(svn_author.unwrap_or_default()).into_owned(),
            svn_path: String::from_utf8_lossy(branch_path.unwrap_or_default()).into_owned(),
            mapped_author_name,
            mapped_author_email,
        }
    }
}

fn split_author_name_email(raw: &str) -> Option<(&str, &str)> {
    if raw.contains('\n') {
        return None;
    }

    let i_lt = raw.find('<')?;

    let name = raw[..i_lt].trim_matches(' ');
    let email = raw[(i_lt + 1)..]
        .trim_end_matches(' ')
        .strip_suffix('>')?
        .trim_matches(' ');

    Some((name, email))
}

fn convert_date(date: Option<&chrono::NaiveDateTime>) -> i64 {
    date.map_or(0, |date| date.and_utc().timestamp())
}
