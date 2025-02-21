use std::collections::HashMap;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConvParams {
    #[serde(default = "Vec::new")]
    pub(crate) branches: Vec<String>,
    #[serde(rename = "rename-branches", default = "HashMap::new")]
    pub(crate) rename_branches: HashMap<String, String>,
    #[serde(rename = "keep-deleted-branches", default = "true_")]
    pub(crate) keep_deleted_branches: bool,
    #[serde(default = "Vec::new")]
    pub(crate) tags: Vec<String>,
    #[serde(rename = "rename-tags", default = "HashMap::new")]
    pub(crate) rename_tags: HashMap<String, String>,
    #[serde(rename = "keep-deleted-tags", default = "true_")]
    pub(crate) keep_deleted_tags: bool,
    #[serde(default = "default_head")]
    pub(crate) head: String,
    #[serde(rename = "unbranched-name")]
    pub(crate) unbranched_name: Option<String>,
    #[serde(rename = "enable-merges", default = "true_")]
    pub(crate) enable_merges: bool,
    #[serde(rename = "merge-optional", default = "Vec::new")]
    pub(crate) merge_optional: Vec<String>,
    #[serde(rename = "avoid-fully-reverted-merges", default = "false_")]
    pub(crate) avoid_fully_reverted_merges: bool,
    #[serde(rename = "ignore-merges", default = "Vec::new")]
    pub(crate) ignore_merges: Vec<BranchRev>,
    #[serde(rename = "generate-gitignore", default = "true_")]
    pub(crate) generate_gitignore: bool,
    #[serde(rename = "delete-files", default = "Vec::new")]
    pub(crate) delete_files: Vec<String>,
    #[serde(rename = "user-map-file")]
    pub(crate) user_map_file: Option<PathBuf>,
    #[serde(rename = "user-fallback-template")]
    pub(crate) user_fallback_template: Option<String>,
    #[serde(rename = "commit-msg-template")]
    pub(crate) commit_msg_template: Option<String>,
    #[serde(rename = "tag-msg-template")]
    pub(crate) tag_msg_template: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BranchRev {
    pub(crate) path: String,
    pub(crate) rev: u32,
}

#[inline(always)]
fn false_() -> bool {
    false
}

#[inline(always)]
fn true_() -> bool {
    true
}

fn default_head() -> String {
    "trunk".into()
}
