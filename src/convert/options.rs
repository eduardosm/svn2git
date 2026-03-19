use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};

use super::ConvertError;
use crate::params_file::GitSvnParams;
use crate::path_pattern::PathPattern;
use crate::{FHashMap, FHashSet};

pub(crate) struct InitOptions {
    pub(crate) git_svn: Option<GitSvnParams>,
    pub(crate) keep_deleted_branches: bool,
    pub(crate) keep_deleted_tags: bool,
    pub(crate) head_path: Vec<u8>,
    pub(crate) unbranched_name: Option<String>,
    pub(crate) enable_merges: bool,
    pub(crate) merge_optional: PathPattern,
    pub(crate) avoid_fully_reverted_merges: bool,
    pub(crate) generate_gitignore: bool,
    pub(crate) delete_files: PathPattern,
    pub(crate) git_obj_cache_size: usize,
    pub(crate) git_repack: bool,
}

pub(crate) struct Options {
    root_dir_spec: ContainerDirSpecNode,
    pub(super) rename_branches: BranchRenamer,
    pub(crate) git_svn: Option<GitSvnParams>,
    pub(super) keep_deleted_branches: bool,
    pub(super) partial_branches: PartialBranchSet,
    pub(super) rename_tags: BranchRenamer,
    pub(super) keep_deleted_tags: bool,
    pub(super) partial_tags: PartialBranchSet,
    pub(crate) head_path: Vec<u8>,
    pub(super) unbranched_name: Option<String>,
    pub(super) enable_merges: bool,
    pub(super) merge_optional: PathPattern,
    pub(super) avoid_fully_reverted_merges: bool,
    pub(super) ignore_merges_at: FHashMap<u32, FHashSet<Vec<u8>>>,
    pub(super) generate_gitignore: bool,
    pub(super) delete_files: PathPattern,
    pub(super) git_obj_cache_size: usize,
    pub(super) git_repack: bool,
}

enum DirSpecNode {
    Branch(bool),
    Container(ContainerDirSpecNode),
}

struct ContainerDirSpecNode {
    wildcard: Option<bool>,
    subdirs: FHashMap<Vec<u8>, DirSpecNode>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DirClass<'a> {
    Unbranched,
    Branch(&'a [u8], bool, &'a [u8]),
    BranchParent,
}

#[derive(Debug)]
pub(crate) struct BranchRenameAddError;

pub(crate) struct PartialBranchAddError;

impl Options {
    pub(crate) fn new(init: InitOptions) -> Self {
        Self {
            root_dir_spec: ContainerDirSpecNode {
                wildcard: None,
                subdirs: FHashMap::default(),
            },
            rename_branches: BranchRenamer::new(),
            git_svn: init.git_svn,
            keep_deleted_branches: init.keep_deleted_branches,
            partial_branches: PartialBranchSet::new(),
            rename_tags: BranchRenamer::new(),
            keep_deleted_tags: init.keep_deleted_tags,
            partial_tags: PartialBranchSet::new(),
            head_path: init.head_path,
            unbranched_name: init.unbranched_name,
            enable_merges: init.enable_merges,
            merge_optional: init.merge_optional,
            avoid_fully_reverted_merges: init.avoid_fully_reverted_merges,
            ignore_merges_at: FHashMap::default(),
            generate_gitignore: init.generate_gitignore,
            delete_files: init.delete_files,
            git_obj_cache_size: init.git_obj_cache_size,
            git_repack: init.git_repack,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ConvertError> {
        if self.head_path.is_empty() {
            if self.unbranched_name.is_none() {
                tracing::error!("head path is empty, not unbranched branch name is not set");
                Err(ConvertError)
            } else {
                Ok(())
            }
        } else {
            match self.classify_dir(&self.head_path) {
                DirClass::Branch(_, _, b"") => Ok(()),
                _ => {
                    tracing::error!(
                        "head path \"{}\" is not a possible branch path",
                        self.head_path.escape_ascii(),
                    );
                    Err(ConvertError)
                }
            }
        }
    }

    pub(crate) fn add_branch_dir(
        &mut self,
        path: &[u8],
        is_tag: bool,
    ) -> Result<(), Option<Vec<u8>>> {
        if path == b"" || path.starts_with(b"/") || path.ends_with(b"/") {
            return Err(None);
        }

        let mut current_path_len = 0;
        let mut current_dir_node = &mut self.root_dir_spec;
        let mut components = path.split(|&c| c == b'/');
        let last_component = components.next_back().unwrap();
        for component in components {
            if component == b"*" {
                return Err(None);
            }
            if current_path_len != 0 {
                // count '/'
                current_path_len += 1;
            }
            current_path_len += component.len();

            match current_dir_node.subdirs.entry(component.to_vec()) {
                std::collections::hash_map::Entry::Occupied(entry) => match entry.into_mut() {
                    DirSpecNode::Branch(_) => {
                        return Err(Some(path[..current_path_len].to_vec()));
                    }
                    DirSpecNode::Container(container) => {
                        current_dir_node = container;
                    }
                },
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let new_node = entry.insert(DirSpecNode::Container(ContainerDirSpecNode {
                        wildcard: None,
                        subdirs: FHashMap::default(),
                    }));
                    let DirSpecNode::Container(container) = new_node else {
                        unreachable!();
                    };
                    current_dir_node = container;
                }
            }
        }

        if last_component == b"*" {
            if current_dir_node.wildcard.is_some() {
                return Err(Some(path.to_vec()));
            }

            current_dir_node.wildcard = Some(is_tag);
        } else {
            match current_dir_node.subdirs.entry(last_component.to_vec()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    return Err(Some(path.to_vec()));
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(DirSpecNode::Branch(is_tag));
                }
            }
        }

        Ok(())
    }

    pub(crate) fn add_branch_rename(
        &mut self,
        from: &[u8],
        to: &[u8],
    ) -> Result<(), BranchRenameAddError> {
        self.rename_branches.add(from, to)
    }

    pub(crate) fn add_tag_rename(
        &mut self,
        from: &[u8],
        to: &[u8],
    ) -> Result<(), BranchRenameAddError> {
        self.rename_tags.add(from, to)
    }

    pub(crate) fn add_partial_branch(&mut self, name: &[u8]) -> Result<(), PartialBranchAddError> {
        self.partial_branches.add(name)
    }

    pub(crate) fn add_partial_tag(&mut self, name: &[u8]) -> Result<(), PartialBranchAddError> {
        self.partial_tags.add(name)
    }

    pub(super) fn classify_dir<'a>(&self, path: &'a [u8]) -> DirClass<'a> {
        let mut current_path_len = 0;
        let mut current_dir_node = &self.root_dir_spec;
        if path != b"" {
            for component in path.split(|&c| c == b'/') {
                if current_path_len != 0 {
                    // count '/'
                    current_path_len += 1;
                }
                current_path_len += component.len();

                if let Some(subdir_spec) = current_dir_node.subdirs.get(component) {
                    match subdir_spec {
                        DirSpecNode::Branch(is_tag) => {
                            return DirClass::Branch(
                                &path[..current_path_len],
                                *is_tag,
                                if current_path_len == path.len() {
                                    b""
                                } else {
                                    &path[(current_path_len + 1)..]
                                },
                            );
                        }
                        DirSpecNode::Container(container) => {
                            current_dir_node = container;
                        }
                    }
                } else if let Some(is_tag) = current_dir_node.wildcard {
                    return DirClass::Branch(
                        &path[..current_path_len],
                        is_tag,
                        if current_path_len == path.len() {
                            b""
                        } else {
                            &path[(current_path_len + 1)..]
                        },
                    );
                } else {
                    return DirClass::Unbranched;
                }
            }
        }
        if current_dir_node.wildcard.is_some() || !current_dir_node.subdirs.is_empty() {
            DirClass::BranchParent
        } else {
            DirClass::Unbranched
        }
    }

    pub(crate) fn check_partial_branch(&self, branch_path: &[u8], is_tag: bool) -> bool {
        if is_tag {
            self.partial_tags.check(branch_path)
        } else {
            self.partial_branches.check(branch_path)
        }
    }

    pub(crate) fn add_ignored_merge_at(&mut self, path: &[u8], rev: u32) {
        self.ignore_merges_at
            .entry(rev)
            .or_default()
            .insert(path.to_vec());
    }

    fn git_svn_mapping<'a>(&'a self, remote_name: &'a str) -> Vec<String> {
        struct State<'o, 'r> {
            options: &'o Options,
            remote_name: &'o str,
            result: &'r mut Vec<String>,
            current: Vec<String>,
        }
        impl<'o, 'r> State<'o, 'r> {
            fn new(
                options: &'o Options,
                remote_name: &'o str,
                result: &'r mut Vec<String>,
            ) -> Self {
                Self {
                    options,
                    remote_name,
                    result,
                    current: Vec::new(),
                }
            }
            fn fill(&mut self) {
                self.traverse_container(&self.options.root_dir_spec, false);
                for (from_bytes, to_bytes) in &self.options.rename_branches.exact {
                    if let Ok(from) = String::from_utf8(from_bytes.clone()) {
                        if let Ok(to) = String::from_utf8(to_bytes.clone()) {
                            let remote = self.remote_name;
                            self.result
                                .push(format!("branches = {from}:refs/remotes/{remote}/{to}"));
                        }
                    }
                }

                for (from_bytes, to_bytes) in &self.options.rename_tags.exact {
                    if let Ok(from) = String::from_utf8(from_bytes.clone()) {
                        if let Ok(to) = String::from_utf8(to_bytes.clone()) {
                            let remote = self.remote_name;
                            self.result
                                .push(format!("tags = {from}:refs/remotes/{remote}/tags/{to}"));
                        }
                    }
                }
            }
            fn traverse_node_tags(&mut self, node: &DirSpecNode, is_tag: bool) {
                match node {
                    DirSpecNode::Branch(tag) => self.push(*tag, ""),
                    DirSpecNode::Container(container) => self.traverse_container(container, is_tag),
                }
            }
            fn traverse_container(&mut self, container: &ContainerDirSpecNode, mut is_tag: bool) {
                if let Some(tag) = container.wildcard {
                    self.push(tag, "/*");
                    is_tag = tag;
                }
                for (name_bytes, subnode) in &container.subdirs {
                    if let Ok(name) = String::from_utf8(name_bytes.clone()) {
                        self.current.push(name);
                        self.traverse_node_tags(subnode, is_tag);
                        self.current.pop();
                    }
                }
            }
            fn push(&mut self, tag: bool, suffix: &str) {
                let remote = self.remote_name;
                let path = self.current.join("/");

                let renamer = if tag {
                    &self.options.rename_tags
                } else {
                    &self.options.rename_branches
                };
                let suffixed = format!("{path}{suffix}");
                let mut git_name = suffixed.clone();

                for (from_prefix, to_prefix) in &renamer.prefix {
                    if suffixed.as_bytes().starts_with(from_prefix) {
                        let mut result_bytes = to_prefix.clone();
                        result_bytes.extend_from_slice(&suffixed.as_bytes()[from_prefix.len()..]);
                        git_name = String::from_utf8(result_bytes).unwrap_or(path.clone());
                        break;
                    }
                }

                if let Some(to) = renamer.exact.get(path.as_bytes()) {
                    git_name = String::from_utf8(to.clone()).unwrap_or(path.clone());
                }

                if tag {
                    self.result.push(format!(
                        "tags = {suffixed}:refs/remotes/{remote}/tags/{git_name}"
                    ));
                } else {
                    self.result.push(format!(
                        "branches = {suffixed}:refs/remotes/{remote}/{git_name}"
                    ));
                }
            }
        }

        let mut result = Vec::new();
        let mut state = State::new(self, remote_name, &mut result);
        state.fill();

        let mut mapping: HashMap<String, (String, bool)> = HashMap::new();

        for line in result {
            if let Some(rest) = line.strip_prefix("branches = ") {
                let colon_pos = rest.find(':').unwrap();
                let path = rest[..colon_pos].to_string();
                let git_name = rest[colon_pos + 1..]
                    .strip_prefix("refs/remotes/origin/")
                    .unwrap_or(&rest[colon_pos + 1..])
                    .to_string();
                mapping.insert(path, (git_name, false));
            } else if let Some(rest) = line.strip_prefix("tags = ") {
                let colon_pos = rest.find(':').unwrap();
                let path = rest[..colon_pos].to_string();
                let git_name = rest[colon_pos + 1..]
                    .strip_prefix("refs/remotes/origin/tags/")
                    .unwrap_or(&rest[colon_pos + 1..])
                    .to_string();
                mapping.insert(path, (git_name, true));
            }
        }

        let mut final_result: Vec<String> = mapping
            .into_iter()
            .map(|(path, (git_name, is_tag))| {
                if is_tag {
                    format!("tags = {path}:refs/remotes/origin/tags/{git_name}")
                } else {
                    format!("branches = {path}:refs/remotes/origin/{git_name}",)
                }
            })
            .collect();

        final_result.sort();
        final_result
    }

    pub(crate) fn write_git_svn_config(&self, file: &mut File) -> io::Result<()> {
        if let Some(svn_params) = &self.git_svn {
            let mut fetch = Vec::new();
            fetch.write_all(&self.head_path)?;
            fetch.write_all(b":refs/remotes/")?;
            fetch.write_all(svn_params.remote_name.as_bytes())?;
            fetch.write_all(b"/")?;
            fetch.write_all(&self.rename_branches.rename(&self.head_path))?;

            file.write_all(b"[svn-remote \"svn\"]\n\turl = ")?;
            file.write_all(svn_params.url.as_bytes())?;
            file.write_all(b"\n\tfetch = ")?;
            file.write_all(&fetch)?;
            for rule in &self.git_svn_mapping(&svn_params.remote_name) {
                if rule.as_bytes() != fetch {
                    file.write_all(b"\n\t")?;
                    file.write_all(rule.as_bytes())?;
                }
            }
        }
        Ok(())
    }
}

pub(super) struct BranchRenamer {
    exact: FHashMap<Vec<u8>, Vec<u8>>,
    prefix: Vec<(Vec<u8>, Vec<u8>)>,
}

impl BranchRenamer {
    fn new() -> Self {
        Self {
            exact: FHashMap::default(),
            prefix: Vec::new(),
        }
    }

    fn add(&mut self, from: &[u8], to: &[u8]) -> Result<(), BranchRenameAddError> {
        if let Some(from_prefix) = from.strip_suffix(b"*") {
            let to_prefix = to.strip_suffix(b"*").ok_or(BranchRenameAddError)?;

            if from_prefix.contains(&b'*') || to_prefix.contains(&b'*') {
                return Err(BranchRenameAddError);
            }

            self.prefix.push((from_prefix.to_vec(), to_prefix.to_vec()));

            Ok(())
        } else {
            if from.contains(&b'*') || to.contains(&b'*') {
                return Err(BranchRenameAddError);
            }

            self.exact.insert(from.to_vec(), to.to_vec());
            Ok(())
        }
    }

    pub(super) fn rename<'a>(&'a self, name: &'a [u8]) -> Cow<'a, [u8]> {
        if let Some(to) = self.exact.get(name) {
            Cow::Borrowed(to)
        } else {
            for (from_prefix, to_prefix) in &self.prefix {
                if name.starts_with(from_prefix) {
                    let mut new_name = to_prefix.clone();
                    new_name.extend_from_slice(&name[from_prefix.len()..]);
                    return Cow::Owned(new_name);
                }
            }

            Cow::Borrowed(name)
        }
    }
}

pub(super) struct PartialBranchSet {
    exact: FHashSet<Vec<u8>>,
    prefix: FHashSet<Vec<u8>>,
}

impl PartialBranchSet {
    fn new() -> Self {
        Self {
            exact: FHashSet::default(),
            prefix: FHashSet::default(),
        }
    }

    fn add(&mut self, name: &[u8]) -> Result<(), PartialBranchAddError> {
        if let Some(prefix) = name.strip_suffix(b"*") {
            if prefix.contains(&b'*') {
                return Err(PartialBranchAddError);
            }
            self.prefix.insert(prefix.to_vec());
            Ok(())
        } else {
            if name.contains(&b'*') {
                return Err(PartialBranchAddError);
            }
            self.exact.insert(name.to_vec());
            Ok(())
        }
    }

    fn check(&self, branch_path: &[u8]) -> bool {
        if self.exact.contains(branch_path) {
            return true;
        }

        for prefix in self.prefix.iter() {
            if branch_path.starts_with(prefix) {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::{DirClass, InitOptions, Options};
    use crate::path_pattern::PathPattern;

    fn default_init() -> InitOptions {
        InitOptions {
            git_svn: None,
            keep_deleted_branches: true,
            keep_deleted_tags: true,
            head_path: b"trunk".to_vec(),
            unbranched_name: Some("unbranched".into()),
            enable_merges: false,
            merge_optional: PathPattern::default(),
            avoid_fully_reverted_merges: false,
            generate_gitignore: false,
            delete_files: PathPattern::default(),
            git_obj_cache_size: 250_000_000,
            git_repack: false,
        }
    }

    #[test]
    fn test_add_branch_dir() {
        let mut options = Options::new(default_init());
        options.add_branch_dir(b"*", false).unwrap();
        options.add_branch_dir(b"a", false).unwrap();
        options.add_branch_dir(b"b", false).unwrap();
        options.add_branch_dir(b"c/*", false).unwrap();
        options.add_branch_dir(b"c/a/*", false).unwrap();
        options.add_branch_dir(b"c/b", false).unwrap();
        options.add_branch_dir(b"c/c/a", false).unwrap();
        assert_eq!(
            options.add_branch_dir(b"a", false),
            Err(Some(b"a".to_vec())),
        );
        assert_eq!(
            options.add_branch_dir(b"a/*", false),
            Err(Some(b"a".to_vec())),
        );
        assert_eq!(
            options.add_branch_dir(b"a/b", false),
            Err(Some(b"a".to_vec())),
        );
        assert_eq!(
            options.add_branch_dir(b"c", false),
            Err(Some(b"c".to_vec())),
        );
        assert_eq!(
            options.add_branch_dir(b"c/a", false),
            Err(Some(b"c/a".to_vec())),
        );
        assert_eq!(
            options.add_branch_dir(b"c/a/*", false),
            Err(Some(b"c/a/*".to_vec())),
        );
    }

    #[test]
    fn test_classify_dir() {
        let mut options = Options::new(default_init());
        options.add_branch_dir(b"a", false).unwrap();
        options.add_branch_dir(b"b/*", false).unwrap();
        options.add_branch_dir(b"b/a/*", false).unwrap();
        options.add_branch_dir(b"b/b", false).unwrap();
        options.add_branch_dir(b"b/c/a", false).unwrap();

        assert_eq!(
            options.classify_dir(b"a"),
            DirClass::Branch(b"a", false, b""),
        );
        assert_eq!(
            options.classify_dir(b"a/1"),
            DirClass::Branch(b"a", false, b"1"),
        );
        assert_eq!(
            options.classify_dir(b"a/1/2"),
            DirClass::Branch(b"a", false, b"1/2"),
        );
        assert_eq!(options.classify_dir(b"b"), DirClass::BranchParent);
        assert_eq!(options.classify_dir(b"b/a"), DirClass::BranchParent);
        assert_eq!(
            options.classify_dir(b"b/a/a"),
            DirClass::Branch(b"b/a/a", false, b""),
        );
        assert_eq!(
            options.classify_dir(b"b/a/a/1"),
            DirClass::Branch(b"b/a/a", false, b"1"),
        );
        assert_eq!(
            options.classify_dir(b"b/a/a/1/2"),
            DirClass::Branch(b"b/a/a", false, b"1/2"),
        );
        assert_eq!(options.classify_dir(b"b/c"), DirClass::BranchParent);
        assert_eq!(
            options.classify_dir(b"b/c/a"),
            DirClass::Branch(b"b/c/a", false, b""),
        );
        assert_eq!(options.classify_dir(b"b/c/b"), DirClass::Unbranched);
        assert_eq!(options.classify_dir(b"c"), DirClass::Unbranched);
    }

    #[test]
    fn test_branches_mapping_simple() {
        // branches = a    :refs/remotes/origin/a
        // branches = b/*  :refs/remotes/origin/b/*
        // branches = b/a/*:refs/remotes/origin/b/a/*
        // branches = b/b  :refs/remotes/origin/b/b
        // branches = b/c/a:refs/remotes/origin/b/c/a
        // tags = ta    :refs/remotes/origin/ta
        // tags = tb/*  :refs/remotes/origin/tb/*
        // tags = tb/a/*:refs/remotes/origin/tb/a/*
        // tags = tb/b  :refs/remotes/origin/tb/b
        // tags = tb/c/a:refs/remotes/origin/tb/c/a
        let mut options = Options::new(default_init());
        options.add_branch_dir(b"a", false).unwrap();
        options.add_branch_dir(b"b/*", false).unwrap();
        options.add_branch_dir(b"b/a/*", false).unwrap();
        options.add_branch_dir(b"b/b", false).unwrap();
        options.add_branch_dir(b"b/c/a", false).unwrap();
        options.add_branch_dir(b"d-*", false).unwrap();

        options.add_branch_dir(b"ta", true).unwrap();
        options.add_branch_dir(b"tb/*", true).unwrap();
        options.add_branch_dir(b"tb/a/*", true).unwrap();
        options.add_branch_dir(b"tb/b", true).unwrap();
        options.add_branch_dir(b"tb/c/a", true).unwrap();
        options.add_branch_dir(b"td-*", true).unwrap();

        assert_eq!(
            options.git_svn_mapping("origin"),
            &[
                "branches = a:refs/remotes/origin/a",
                "branches = b/*:refs/remotes/origin/b/*",
                "branches = b/a/*:refs/remotes/origin/b/a/*",
                "branches = b/b:refs/remotes/origin/b/b",
                "branches = b/c/a:refs/remotes/origin/b/c/a",
                "branches = d-*:refs/remotes/origin/d-*",
                "tags = ta:refs/remotes/origin/tags/ta",
                "tags = tb/*:refs/remotes/origin/tags/tb/*",
                "tags = tb/a/*:refs/remotes/origin/tags/tb/a/*",
                "tags = tb/b:refs/remotes/origin/tags/tb/b",
                "tags = tb/c/a:refs/remotes/origin/tags/tb/c/a",
                "tags = td-*:refs/remotes/origin/tags/td-*",
            ]
        );
    }

    #[test]
    fn test_branches_mapping_with_rename() {
        // branches = a    :refs/remotes/origin/a
        // branches = b/*  :refs/remotes/origin/b/*
        // branches = b/a/*:refs/remotes/origin/b/a/*
        // branches = b/b  :refs/remotes/origin/b/b
        // branches = b/c/a:refs/remotes/origin/b/c/a
        // tags = ta    :refs/remotes/origin/ta
        // tags = tb/*  :refs/remotes/origin/tb/*
        // tags = tb/a/*:refs/remotes/origin/tb/a/*
        // tags = tb/b  :refs/remotes/origin/tb/b
        // tags = tb/c/a:refs/remotes/origin/tb/c/a
        let mut options = Options::new(default_init());
        options.add_branch_dir(b"a", false).unwrap();
        options.add_branch_dir(b"b/*", false).unwrap();
        options.add_branch_dir(b"b/a/*", false).unwrap();
        options.add_branch_dir(b"b/b", false).unwrap();
        options.add_branch_dir(b"b/c/a", false).unwrap();
        options.add_branch_dir(b"d-*", false).unwrap();

        options.add_branch_dir(b"ta", true).unwrap();
        options.add_branch_dir(b"tb/*", true).unwrap();
        options.add_branch_dir(b"tb/a/*", true).unwrap();
        options.add_branch_dir(b"tb/b", true).unwrap();
        options.add_branch_dir(b"tb/c/a", true).unwrap();
        options.add_branch_dir(b"td-*", true).unwrap();

        options.add_branch_rename(b"exact", b"exact2").unwrap();
        options.add_branch_rename(b"b/exact", b"exact3").unwrap();
        options.add_branch_rename(b"b/a/*", b"exact/*").unwrap();
        options.add_branch_rename(b"d-*", b"exact-*").unwrap();

        options.add_tag_rename(b"texact", b"texact2").unwrap();
        options.add_tag_rename(b"tb/exact", b"texact3").unwrap();
        options.add_tag_rename(b"tb/a/*", b"texact/*").unwrap();
        options.add_tag_rename(b"td-*", b"texact-*").unwrap();

        assert_eq!(
            options.git_svn_mapping("origin"),
            &[
                "branches = a:refs/remotes/origin/a",
                "branches = b/*:refs/remotes/origin/b/*",
                "branches = b/a/*:refs/remotes/origin/exact/*",
                "branches = b/b:refs/remotes/origin/b/b",
                "branches = b/c/a:refs/remotes/origin/b/c/a",
                "branches = b/exact:refs/remotes/origin/exact3",
                "branches = d-*:refs/remotes/origin/exact-*",
                "branches = exact:refs/remotes/origin/exact2",
                "tags = ta:refs/remotes/origin/tags/ta",
                "tags = tb/*:refs/remotes/origin/tags/tb/*",
                "tags = tb/a/*:refs/remotes/origin/tags/texact/*",
                "tags = tb/b:refs/remotes/origin/tags/tb/b",
                "tags = tb/c/a:refs/remotes/origin/tags/tb/c/a",
                "tags = tb/exact:refs/remotes/origin/tags/texact3",
                "tags = td-*:refs/remotes/origin/tags/texact-*",
                "tags = texact:refs/remotes/origin/tags/texact2",
            ]
        );
    }
}
