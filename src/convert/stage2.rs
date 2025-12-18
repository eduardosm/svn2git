use std::collections::{BTreeSet, VecDeque};

use super::options::Options;
use super::{ConvertError, GitMetaMaker, git_wrap, stage1};
use crate::term_out::ProgressPrint;
use crate::{FHashMap, git};

pub(super) fn run(
    progress_print: &ProgressPrint,
    options: &Options,
    metadata_maker: &dyn GitMetaMaker,
    git_import: &mut git_wrap::Importer,
    stage1_out: &stage1::Output,
) -> Result<(), ConvertError> {
    tracing::info!("Stage 2: emit commits");

    let reachable_revs = Stage::gather_reached_revs(progress_print, options, stage1_out);
    let (unbranched_name, refs_names) =
        Stage::calculate_git_names(progress_print, options, stage1_out);

    Stage {
        progress_print,
        options,
        metadata_maker,
        git_import,
        stage1_out,
        unbranched_name,
        refs_names,
        last_unbranched_commit: None,
        branch_rev_git_data: FHashMap::default(),
    }
    .run(&reachable_revs)
}

struct BranchRevGitData {
    git_commit_oid: gix_hash::ObjectId,
    merges: BTreeSet<usize>,
    cherrypicks: BTreeSet<usize>,
}

struct Stage<'a> {
    progress_print: &'a ProgressPrint,
    options: &'a Options,
    metadata_maker: &'a dyn GitMetaMaker,
    git_import: &'a mut git_wrap::Importer,
    stage1_out: &'a stage1::Output,
    unbranched_name: Option<String>,
    refs_names: FHashMap<usize, String>,
    last_unbranched_commit: Option<gix_hash::ObjectId>,
    branch_rev_git_data: FHashMap<usize, BranchRevGitData>,
}

impl Stage<'_> {
    fn run(mut self, reachable_revs: &BTreeSet<usize>) -> Result<(), ConvertError> {
        self.run_inner(reachable_revs)
    }

    fn gather_reached_revs(
        progress_print: &ProgressPrint,
        options: &Options,
        stage1_out: &stage1::Output,
    ) -> BTreeSet<usize> {
        progress_print.set_progress("gathering reachable commits".into());

        let mut reached_revs = BTreeSet::new();

        let mut queue = VecDeque::new();
        queue.extend(stage1_out.branch_data.iter().filter_map(|b| {
            b.tip_commit.filter(|_| {
                !b.deleted
                    || if b.is_tag {
                        options.keep_deleted_tags
                    } else {
                        options.keep_deleted_branches
                    }
            })
        }));

        while let Some(rev) = queue.pop_front() {
            if reached_revs.insert(rev) {
                queue.extend(stage1_out.branch_rev_data[rev].parent);
                queue.extend(&stage1_out.branch_rev_data[rev].added_svn_merges);
            }
        }

        reached_revs
    }

    fn calculate_git_names(
        progress_print: &ProgressPrint,
        options: &Options,
        stage1_out: &stage1::Output,
    ) -> (Option<String>, FHashMap<usize, String>) {
        tracing::info!("naming branches");
        progress_print.set_progress("naming branches".into());

        let mut ref_name_map = Vec::new();
        if !stage1_out.unbranched_rev_data.is_empty() {
            if let Some(ref unbranched_name) = options.unbranched_name {
                ref_name_map.push((unbranched_name.clone(), None));
            }
        }

        for (branch_i, branch_data) in stage1_out.branch_data.iter().enumerate() {
            if branch_data.deleted
                && ((branch_data.is_tag && !options.keep_deleted_tags)
                    || (!branch_data.is_tag && !options.keep_deleted_branches))
            {
                continue;
            }

            let renamer = if branch_data.is_tag {
                &options.rename_tags
            } else {
                &options.rename_branches
            };

            let pre_git_name = renamer.rename(&branch_data.svn_path);

            let mut git_name = git::legalize_branch_name(&pre_git_name);

            if git_name.as_bytes() != &*pre_git_name {
                tracing::warn!(
                    "branch \"{}\" named \"{}\" instead of \"{}\" due to invalid characters or sequences",
                    branch_data.svn_path.escape_ascii(),
                    git_name.escape_default(),
                    pre_git_name.escape_ascii(),
                );
            }

            if branch_data.deleted {
                git_name.insert_str(0, "deleted/");
            }

            let base_git_name = git_name.clone();
            let mut tries = 0;
            let i = loop {
                match ref_name_map.binary_search_by_key(&git_name.as_str(), |(n, _)| n) {
                    Ok(_) => {
                        tries += 1;
                        git_name = format!("{base_git_name}_{tries}");
                    }
                    Err(i) => break i,
                }
            };

            if git_name != base_git_name {
                tracing::warn!(
                    "using {} name \"{}\" instead of \"{}\" to avoid repetition",
                    if branch_data.is_tag { "tag" } else { "branch" },
                    git_name.escape_default(),
                    base_git_name.escape_default(),
                );
            }

            ref_name_map.insert(i, (git_name, Some(branch_i)));
        }

        let mut final_unbranched_name = None;
        let mut final_branch_name_map = FHashMap::default();

        // avoid prefix collisions
        // git does not allow the branch of a name the be the prefix of the name of another branch
        // (e.g., "a" and "a/b")
        for i in 0..ref_name_map.len() {
            let git_name = &ref_name_map[i].0;
            let mut new_git_name = git_name.clone();

            let mut tries = 0;
            loop {
                let mut ok = true;
                for (j, (check_name, _)) in ref_name_map.iter().enumerate() {
                    if i != j
                        && strip_path_prefix(check_name.as_bytes(), new_git_name.as_bytes())
                            .is_some()
                    {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    break;
                }

                tries += 1;
                new_git_name = format!("{git_name}_{tries}");
            }
            if new_git_name != *git_name {
                let is_tag = ref_name_map[i]
                    .1
                    .is_some_and(|branch_i| stage1_out.branch_data[branch_i].is_tag);
                tracing::warn!(
                    "using {} name \"{}\" instead of \"{}\" to avoid prefix collision",
                    if is_tag { "tag" } else { "branch" },
                    new_git_name.escape_default(),
                    git_name.escape_default(),
                );
                ref_name_map[i].0 = new_git_name;
            }

            if let Some(branch_i) = ref_name_map[i].1 {
                let final_name = if stage1_out.branch_data[branch_i].is_tag {
                    format!("refs/tags/{}", ref_name_map[i].0)
                } else {
                    format!("refs/heads/{}", ref_name_map[i].0)
                };
                final_branch_name_map.insert(branch_i, final_name);
            } else {
                let final_name = format!("refs/heads/{}", ref_name_map[i].0);
                final_unbranched_name = Some(final_name);
            }
        }

        (final_unbranched_name, final_branch_name_map)
    }

    fn run_inner(&mut self, reachable_revs: &BTreeSet<usize>) -> Result<(), ConvertError> {
        tracing::info!("Emitting unbranched commits");

        if self.unbranched_name.is_some() {
            for i in 0..self.stage1_out.unbranched_rev_data.len() {
                let svn_rev = self.stage1_out.root_rev_data
                    [self.stage1_out.unbranched_rev_data[i].root_rev]
                    .svn_rev;
                tracing::debug!("emitting unbranched commit for SVN revision {svn_rev}");
                self.progress_print.set_progress(format!(
                    "emitting branch commit - {} / {} (r{svn_rev})",
                    i + 1,
                    self.stage1_out.unbranched_rev_data.len(),
                ));

                self.make_unbranched_commit(i)?;
            }
        }

        tracing::info!("Emitting branch commits");

        let mut prev_svn_rev = 0;
        for &i in reachable_revs.iter() {
            let svn_rev =
                self.stage1_out.root_rev_data[self.stage1_out.branch_rev_data[i].root_rev].svn_rev;
            if svn_rev != prev_svn_rev {
                tracing::debug!("emitting branch commits and tags for SVN revision {svn_rev}");
            }

            self.progress_print.set_progress(format!(
                "emitting branch commit/tag - {} / {} (r{svn_rev})",
                i + 1,
                self.stage1_out.branch_rev_data.len(),
            ));

            if self.stage1_out.branch_data[self.stage1_out.branch_rev_data[i].branch].is_tag {
                self.make_branch_tag(i)?;
            } else {
                self.make_branch_commit(reachable_revs, i)?;
            }
            prev_svn_rev = svn_rev;
        }

        match self.stage1_out.head_branch {
            stage1::Head::Branch(branch) => {
                self.git_import.set_head(&self.refs_names[&branch]);
            }
            stage1::Head::Unbranched => {
                self.git_import
                    .set_head(self.unbranched_name.as_deref().unwrap());
            }
        }

        Ok(())
    }

    fn make_unbranched_commit(&mut self, unbranched_rev: usize) -> Result<(), ConvertError> {
        let unbranch_rev_data = &self.stage1_out.unbranched_rev_data[unbranched_rev];
        let root_commit = unbranch_rev_data.root_rev;

        let git_commit_meta = self
            .metadata_maker
            .make_git_commit_meta(
                self.stage1_out.svn_uuid.as_ref(),
                self.stage1_out.root_rev_data[root_commit].svn_rev,
                None,
                &self.stage1_out.root_rev_data[root_commit].svn_rev_props,
            )
            .map_err(|e| {
                tracing::error!("failed to make git commit metadata: {e}");
                ConvertError
            })?;

        let mut parents = smallvec::SmallVec::new();
        parents.extend(self.last_unbranched_commit);

        let git_commit_oid = self.git_import.put(
            gix_object::Commit {
                tree: unbranch_rev_data.tree_oid,
                parents,
                author: git_commit_meta.author,
                committer: git_commit_meta.committer,
                encoding: None,
                message: git_commit_meta.message.into(),
                extra_headers: vec![],
            },
            None,
        )?;

        self.git_import
            .set_ref(self.unbranched_name.as_deref().unwrap(), git_commit_oid);

        self.last_unbranched_commit = Some(git_commit_oid);

        Ok(())
    }

    fn make_branch_commit(
        &mut self,
        reachable_revs: &BTreeSet<usize>,
        branch_rev: usize,
    ) -> Result<(), ConvertError> {
        let branch_rev_data = &self.stage1_out.branch_rev_data[branch_rev];
        let branch = branch_rev_data.branch;
        let branch_data = &self.stage1_out.branch_data[branch];
        let branch_path = &branch_data.svn_path;
        let root_commit = branch_rev_data.root_rev;
        let parent_commit = branch_rev_data.parent;

        let (new_merges, new_cherrypicks) =
            if self.options.enable_merges && branch_rev_data.parent.is_some() {
                self.analyze_merges(reachable_revs, branch_rev)
            } else {
                (BTreeSet::new(), BTreeSet::new())
            };

        let mut git_merges = Vec::new();
        for &merge in new_merges.iter() {
            tracing::debug!(
                "candidate to be merged: \"{}\"@{}",
                self.stage1_out.branch_data[self.stage1_out.branch_rev_data[merge].branch]
                    .svn_path
                    .escape_ascii(),
                self.stage1_out.root_rev_data[self.stage1_out.branch_rev_data[merge].root_rev]
                    .svn_rev,
            );
            git_merges.push(self.branch_rev_git_data[&merge].git_commit_oid);
        }

        for &cherrypick in new_cherrypicks.iter() {
            tracing::debug!(
                "cherrypick: \"{}\"@{}",
                self.stage1_out.branch_data[self.stage1_out.branch_rev_data[cherrypick].branch]
                    .svn_path
                    .escape_ascii(),
                self.stage1_out.root_rev_data[self.stage1_out.branch_rev_data[cherrypick].root_rev]
                    .svn_rev,
            );
        }

        if !git_merges.is_empty() {
            if new_cherrypicks.is_empty() {
                tracing::debug!("merging into \"{}\"", branch_path.escape_ascii());
            } else {
                tracing::debug!(
                    "not merging into \"{}\" due to {} cherry-pick(s)",
                    branch_path.escape_ascii(),
                    new_cherrypicks.len(),
                );
                git_merges.clear();
            }
        }

        let git_commit_meta = self
            .metadata_maker
            .make_git_commit_meta(
                self.stage1_out.svn_uuid.as_ref(),
                self.stage1_out.root_rev_data[root_commit].svn_rev,
                Some(branch_path),
                &self.stage1_out.root_rev_data[root_commit].svn_rev_props,
            )
            .map_err(|e| {
                tracing::error!("failed to make git commit metadata: {e}");
                ConvertError
            })?;

        let mut parents = smallvec::SmallVec::new();
        parents.extend(parent_commit.map(|c| self.branch_rev_git_data[&c].git_commit_oid));
        parents.extend(git_merges);

        let git_commit_oid = self.git_import.put(
            gix_object::Commit {
                tree: branch_rev_data.tree_oid,
                parents,
                author: git_commit_meta.author,
                committer: git_commit_meta.committer,
                encoding: None,
                message: git_commit_meta.message.into(),
                extra_headers: vec![],
            },
            None,
        )?;

        if !branch_data.deleted || self.options.keep_deleted_branches {
            self.git_import
                .set_ref(&self.refs_names[&branch], git_commit_oid);
        }

        self.branch_rev_git_data.insert(
            branch_rev,
            BranchRevGitData {
                git_commit_oid,
                merges: new_merges,
                cherrypicks: new_cherrypicks,
            },
        );

        tracing::debug!("committed on branch \"{}\"", branch_path.escape_ascii());

        Ok(())
    }

    fn make_branch_tag(&mut self, branch_rev: usize) -> Result<(), ConvertError> {
        let branch_rev_data = &self.stage1_out.branch_rev_data[branch_rev];
        let branch = branch_rev_data.branch;
        let branch_data = &self.stage1_out.branch_data[branch];
        let branch_path = &branch_data.svn_path;
        let root_commit = branch_rev_data.root_rev;

        assert_eq!(branch_data.rev_map.len(), 1);

        let git_tag_meta = self
            .metadata_maker
            .make_git_tag_meta(
                self.stage1_out.svn_uuid.as_ref(),
                self.stage1_out.root_rev_data[root_commit].svn_rev,
                branch_path,
                &self.stage1_out.root_rev_data[root_commit].svn_rev_props,
            )
            .map_err(|e| {
                tracing::error!("failed to make git tag metadata: {e}");
                ConvertError
            })?;

        let target_rev = branch_rev_data.parent.unwrap();
        let target_commit_oid = self.branch_rev_git_data[&target_rev].git_commit_oid;

        let git_tag_oid = self.git_import.put(
            gix_object::Tag {
                target: target_commit_oid,
                target_kind: gix_object::Kind::Commit,
                name: self.refs_names[&branch]
                    .strip_prefix("refs/tags/")
                    .unwrap()
                    .into(),
                tagger: git_tag_meta.tagger,
                message: git_tag_meta.message.into(),
                pgp_signature: None,
            },
            None,
        )?;

        if !branch_data.deleted || self.options.keep_deleted_tags {
            self.git_import
                .set_ref(&self.refs_names[&branch], git_tag_oid);
        }

        self.branch_rev_git_data.insert(
            branch_rev,
            BranchRevGitData {
                git_commit_oid: target_commit_oid,
                merges: BTreeSet::new(),
                cherrypicks: BTreeSet::new(),
            },
        );

        tracing::debug!("created tag \"{}\"", branch_path.escape_ascii());

        Ok(())
    }

    fn analyze_merges(
        &self,
        reachable_revs: &BTreeSet<usize>,
        branch_commit: usize,
    ) -> (BTreeSet<usize>, BTreeSet<usize>) {
        if !Self::has_svn_merges(
            self.options.avoid_fully_reverted_merges,
            self.stage1_out,
            reachable_revs,
            branch_commit,
        ) {
            return (BTreeSet::new(), BTreeSet::new());
        }

        let parent_commit = self.stage1_out.branch_rev_data[branch_commit]
            .parent
            .unwrap();

        // Gather previous cherry-picks...
        let mut inh_cherrypicks = BTreeSet::new();
        // ... and all the merged commits
        let mut merged_history = BTreeSet::new();

        // Traverse parents to gather all cherry-picks and merged commits
        let mut visit_queue = VecDeque::new();
        visit_queue.push_back(parent_commit);
        while let Some(mut some_commit) = visit_queue.pop_front() {
            while merged_history.insert(some_commit) {
                inh_cherrypicks.extend(&self.branch_rev_git_data[&some_commit].cherrypicks);
                visit_queue.extend(&self.branch_rev_git_data[&some_commit].merges);

                some_commit = match self.stage1_out.branch_rev_data[some_commit].parent {
                    Some(c) => c,
                    None => break,
                };
            }
        }

        // Gather SVN merges
        let mut svn_merges = BTreeSet::<usize>::new();
        let mut history_commit = Some(branch_commit);
        while let Some(some_commit) = history_commit {
            if Self::has_svn_merges(
                self.options.avoid_fully_reverted_merges,
                self.stage1_out,
                reachable_revs,
                some_commit,
            ) {
                svn_merges.extend(&self.stage1_out.branch_rev_data[some_commit].added_svn_merges);
            }

            history_commit = self.stage1_out.branch_rev_data[some_commit].parent;
        }

        let mut new_merges = BTreeSet::new();
        let mut new_cherrypicks = BTreeSet::new();

        // Try to convert new SVN merges into git merges
        // This loop relies on `svn_merges` being an ordered set
        for &svn_merge in svn_merges.iter() {
            if merged_history.contains(&svn_merge) {
                // already merged
                continue;
            }

            let merged = if self.stage1_out.branch_rev_data[svn_merge].tail
                != self.stage1_out.branch_rev_data[branch_commit].tail
            {
                // Unrelated histories, do not merge
                false
            } else {
                let mut parent = svn_merge;
                loop {
                    parent = self.stage1_out.branch_rev_data[parent].parent.unwrap();

                    if merged_history.contains(&parent) {
                        // The parent of the cherry-pick is merged, so
                        // the cherry-pick can be converted to merge.
                        break true;
                    }

                    if !self.stage1_out.branch_rev_data[parent].required_in_mergeinfo {
                        // Commit marked as not required in mergeinfo, so it will
                        // not create a gap in the merged revision range although
                        // it is missing from SVN mergeinfo.
                        continue;
                    }

                    let is_merge_commit = !self.branch_rev_git_data[&parent].merges.is_empty()
                        || !self.branch_rev_git_data[&parent].cherrypicks.is_empty();

                    if is_merge_commit
                        && self.branch_rev_git_data[&parent]
                            .merges
                            .is_subset(&merged_history)
                        && self.branch_rev_git_data[&parent]
                            .cherrypicks
                            .is_subset(&merged_history)
                    {
                        // This commit is missing from mergeinfo, but the commit is a merge
                        // whose merged/cherry-picked commits are already part of the destination
                        // history, so we do not consider that it creates a gap.
                        continue;
                    }

                    // There is a gap, the cherry-pick stays cherry-pick
                    break false;
                }
            };

            if merged {
                new_merges.insert(svn_merge);
                merged_history.insert(svn_merge);
                inh_cherrypicks.extend(&self.branch_rev_git_data[&svn_merge].cherrypicks);

                visit_queue.push_back(self.stage1_out.branch_rev_data[svn_merge].parent.unwrap());
                visit_queue.extend(&self.branch_rev_git_data[&svn_merge].merges);

                while let Some(mut some_commit) = visit_queue.pop_front() {
                    while merged_history.insert(some_commit) {
                        inh_cherrypicks.extend(&self.branch_rev_git_data[&some_commit].cherrypicks);
                        // Remove parent in favor of merging the child
                        new_merges.remove(&some_commit);

                        some_commit = self.stage1_out.branch_rev_data[some_commit].parent.unwrap();
                    }
                    new_merges.remove(&some_commit);
                }
            } else {
                new_cherrypicks.insert(svn_merge);
            }
        }

        // Inherited cherry-picks are not new cherry-picks
        for &inh_cerrypick in inh_cherrypicks.iter() {
            new_cherrypicks.remove(&inh_cerrypick);
        }
        // Merged commits are not cherry-picks
        for &merged_commit in merged_history.iter() {
            new_cherrypicks.remove(&merged_commit);
        }

        (new_merges, new_cherrypicks)
    }

    fn has_svn_merges(
        avoid_fully_reverted_merges: bool,
        stage1_out: &stage1::Output,
        reachable_revs: &BTreeSet<usize>,
        branch_rev: usize,
    ) -> bool {
        !stage1_out.branch_rev_data[branch_rev].ignore_merges
            && !stage1_out.branch_rev_data[branch_rev]
                .added_svn_merges
                .is_empty()
            && (!avoid_fully_reverted_merges
                || stage1_out.branch_rev_data[branch_rev]
                    .fully_reverted_merges_in
                    .is_disjoint(reachable_revs))
    }
}

fn strip_path_prefix<'a>(path: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if let Some(suffix) = path.strip_prefix(prefix) {
        if suffix.is_empty() {
            Some(b"")
        } else {
            suffix.strip_prefix(b"/")
        }
    } else {
        None
    }
}
