use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use regex_syntax::hir as regex_hir;

use crate::defs;

pub(crate) fn run_test(test_path: &Path) -> Result<(), String> {
    let temp_dir = get_tmp_dir()?;
    let svn2git_bin = Path::new(env!("CARGO_BIN_EXE_svn2git"));

    let test_def_raw =
        std::fs::read(test_path).map_err(|e| format!("failed to read {test_path:?}: {e}"))?;

    let test_def: defs::Test = serde_yaml::from_slice(&test_def_raw)
        .map_err(|e| format!("failed to parse {test_path:?}: {e}"))?;

    if let Some(ref user_map) = test_def.user_map {
        let user_map_path = temp_dir.join("user-map.txt");

        std::fs::write(&user_map_path, user_map)
            .map_err(|e| format!("failed to write {user_map_path:?}: {e}"))?;
    }

    let conv_params_path = temp_dir.join("conv-params.toml");
    std::fs::write(&conv_params_path, test_def.conv_params.as_bytes())
        .map_err(|e| format!("failed to write {conv_params_path:?}: {e}"))?;

    let svn_dump_path = temp_dir.join("svn-dump");
    let svn_dump = make_svn_dump(&test_def);
    let svn_dump = match test_def.svn_dump_source {
        defs::SvnDumpSource::Uncompressed => svn_dump,
        defs::SvnDumpSource::CompressedXz => {
            liblzma::encode_all(&mut svn_dump.as_slice(), 6).unwrap()
        }
        defs::SvnDumpSource::CompressedGzip => {
            let mut compressed = Vec::new();
            let mut encoder =
                flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::default());
            encoder.write_all(&svn_dump).unwrap();
            encoder.finish().unwrap();
            compressed
        }
        defs::SvnDumpSource::CompressedBzip2 => {
            let mut compressed = Vec::new();
            let mut encoder =
                bzip2::write::BzEncoder::new(&mut compressed, bzip2::Compression::default());
            encoder.write_all(&svn_dump).unwrap();
            encoder.finish().unwrap();
            compressed
        }
        defs::SvnDumpSource::CompressedZstd => {
            zstd::encode_all(&mut svn_dump.as_slice(), 3).unwrap()
        }
        defs::SvnDumpSource::CompressedLz4 => {
            let mut compressed = Vec::new();
            let mut encoder = lz4_flex::frame::FrameEncoder::new(&mut compressed);
            encoder.write_all(&svn_dump).unwrap();
            encoder.finish().unwrap();
            compressed
        }
    };
    std::fs::write(&svn_dump_path, svn_dump)
        .map_err(|e| format!("failed to write {svn_dump_path:?}: {e}"))?;

    let git_repo_path = temp_dir.join("converted.git");
    let conv_log_path = temp_dir.join("conv.log");

    run_convert(
        svn2git_bin,
        &conv_params_path,
        &svn_dump_path,
        &git_repo_path,
        &conv_log_path,
        test_def.git_repack,
        test_def.failed.into(),
    )?;

    if let Some(ref expected_logs) = test_def.logs {
        check_log(&conv_log_path, expected_logs)?;
    }

    if !test_def.failed {
        let fsck_result = std::process::Command::new("git")
            .current_dir(&git_repo_path)
            .arg("fsck")
            .arg("--strict")
            .arg("--no-progress")
            .output()
            .map_err(|e| format!("failed to run git fsck: {e}"))?;

        if !fsck_result.status.success() {
            return Err(format!(
                "git fsck finished with {}\nstdout:\n{}\nstderr:\n{}",
                fsck_result.status,
                String::from_utf8_lossy(&fsck_result.stdout),
                String::from_utf8_lossy(&fsck_result.stderr),
            ));
        }

        let git_repo = gix::open(&git_repo_path)
            .map_err(|e| format!("failed to open git repository {git_repo_path:?}: {e}"))?;

        for git_tag in test_def.git_tags.iter() {
            check_git_tag(&git_repo, git_tag)
                .map_err(|e| format!("tag {:?} check failed: {e}", git_tag.tag))?;
        }

        for git_rev in test_def.git_revs.iter() {
            check_git_rev(&git_repo, git_rev)
                .map_err(|e| format!("revision {:?} check failed: {e}", git_rev.rev))?;
        }
    }

    std::fs::remove_dir_all(&temp_dir)
        .map_err(|e| format!("failed to remove {temp_dir:?}: {e}"))?;

    Ok(())
}

fn get_tmp_dir() -> Result<PathBuf, String> {
    use rand::{Rng as _, SeedableRng as _};

    let mut rng = rand::rngs::StdRng::from_os_rng();

    loop {
        let mut path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        path.push(format!("convert-test-{:08x}", rng.random::<u32>()));

        match std::fs::create_dir(&path) {
            Ok(()) => {
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }
            Err(e) => {
                return Err(format!("failed to create directory {path:?}: {e}"));
            }
        }
    }
}

fn make_svn_dump(test_def: &defs::Test) -> Vec<u8> {
    use std::io::Write as _;

    let mut dump = Vec::<u8>::new();

    dump.extend(b"SVN-fs-dump-format-version: ");
    dump.extend(match test_def.svn_dump_version {
        defs::SvnDumpVersion::Two => b"2\n\n",
        defs::SvnDumpVersion::Three => b"3\n\n",
    });

    if let Some(ref svn_uuid) = test_def.svn_uuid {
        dump.extend(b"UUID: ");
        dump.extend(svn_uuid.as_bytes());
        dump.extend(b"\n\n");
    }

    let mut rev0_props = Vec::<u8>::new();
    end_svn_props(&mut rev0_props);

    dump.extend(b"Revision-number: 0\n");
    writeln!(dump, "Prop-content-length: {}", rev0_props.len()).unwrap();
    writeln!(dump, "Content-length: {}", rev0_props.len()).unwrap();
    dump.extend(b"\n");
    dump.extend(rev0_props);
    dump.extend(b"\n");

    let mut prev_svn_rev_no = 0;
    for svn_rev in test_def.svn_revs.iter() {
        let svn_rev_no = svn_rev.no.unwrap_or(prev_svn_rev_no + 1);

        let mut rev_props = Vec::<u8>::new();
        for (prop_name, prop_value) in svn_rev.props.iter() {
            push_svn_prop(
                prop_name.as_bytes(),
                Some(prop_value.as_bytes()),
                &mut rev_props,
            );
        }
        end_svn_props(&mut rev_props);

        writeln!(dump, "Revision-number: {svn_rev_no}").unwrap();
        writeln!(dump, "Prop-content-length: {}", rev_props.len()).unwrap();
        writeln!(dump, "Content-length: {}", rev_props.len()).unwrap();
        dump.extend(b"\n");
        dump.extend(&rev_props);
        dump.extend(b"\n");

        for svn_node in svn_rev.nodes.iter() {
            let node_props = if let Some(ref props) = svn_node.props {
                let mut node_props = Vec::<u8>::new();
                for (prop_name, prop_value) in props.iter() {
                    push_svn_prop(
                        prop_name.as_bytes(),
                        prop_value.as_ref().map(String::as_bytes),
                        &mut node_props,
                    );
                }
                end_svn_props(&mut node_props);

                Some(node_props)
            } else {
                None
            };

            dump.extend(b"Node-path: ");
            dump.extend(svn_node.path.as_bytes());
            dump.extend(b"\n");

            dump.extend(b"Node-kind: ");
            dump.extend(match svn_node.kind {
                defs::SvnNodeKind::File => b"file".as_slice(),
                defs::SvnNodeKind::Dir => b"dir".as_slice(),
            });
            dump.extend(b"\n");

            dump.extend(b"Node-action: ");
            dump.extend(match svn_node.action {
                defs::SvnNodeAction::Change => b"change".as_slice(),
                defs::SvnNodeAction::Add => b"add".as_slice(),
                defs::SvnNodeAction::Delete => b"delete".as_slice(),
                defs::SvnNodeAction::Replace => b"replace".as_slice(),
            });
            dump.extend(b"\n");

            if let Some(ref copy_from_path) = svn_node.copy_from_path {
                dump.extend(b"Node-copyfrom-path: ");
                dump.extend(copy_from_path.as_bytes());
                dump.extend(b"\n");
                writeln!(
                    dump,
                    "Node-copyfrom-rev: {}",
                    svn_node.copy_from_rev.unwrap_or(prev_svn_rev_no),
                )
                .unwrap();
            }

            if let Some(prop_delta) = svn_node.prop_delta {
                writeln!(
                    dump,
                    "Prop-delta: {}",
                    if prop_delta { "true" } else { "false" },
                )
                .unwrap();
            }

            if let Some(text_delta) = svn_node.text_delta {
                writeln!(
                    dump,
                    "Text-delta: {}",
                    if text_delta { "true" } else { "false" },
                )
                .unwrap();
            }

            let props_len = node_props.as_ref().map(Vec::len);
            let text_len = svn_node.text.as_ref().map(defs::Bytes::len);
            if let Some(props_len) = props_len {
                writeln!(dump, "Prop-content-length: {props_len}").unwrap();
            }
            if let Some(text_len) = text_len {
                writeln!(dump, "Text-content-length: {text_len}").unwrap();
            }
            writeln!(
                dump,
                "Content-length: {}",
                props_len.unwrap_or(0) + text_len.unwrap_or(0)
            )
            .unwrap();
            dump.extend(b"\n");

            if let Some(ref node_props) = node_props {
                dump.extend(node_props);
            }
            if let Some(ref text) = svn_node.text {
                dump.extend(text.as_slice());
            }
            dump.extend(b"\n");
        }

        prev_svn_rev_no = svn_rev_no;
    }

    dump
}

fn push_svn_prop(k: &[u8], v: Option<&[u8]>, out: &mut Vec<u8>) {
    use std::io::Write as _;

    if let Some(v) = v {
        writeln!(out, "K {}", k.len()).unwrap();
        out.extend(k);
        writeln!(out, "\nV {}", v.len()).unwrap();
        out.extend(v);
    } else {
        writeln!(out, "D {}", k.len()).unwrap();
        out.extend(k);
    }
    out.push(b'\n');
}

fn end_svn_props(out: &mut Vec<u8>) {
    out.extend(b"PROPS-END\n");
}

fn run_convert(
    conv_bin: &Path,
    conv_params_path: &Path,
    svn_dump_path: &Path,
    git_repo_path: &Path,
    conv_log_path: &Path,
    git_repack: bool,
    expect_exit_code: i32,
) -> Result<(), String> {
    let mut conv_cmd = std::process::Command::new(conv_bin);
    conv_cmd
        .arg("--no-progress")
        .arg("--src")
        .arg(svn_dump_path)
        .arg("--dest")
        .arg(git_repo_path)
        .arg("--conv-params")
        .arg(conv_params_path)
        .arg("--log-file")
        .arg(conv_log_path)
        .args(git_repack.then_some("--git-repack"));

    let cmd_out = conv_cmd
        .output()
        .map_err(|e| format!("failed to run {conv_bin:?}: {e}"))?;
    drop(conv_cmd);

    if cmd_out.status.code() != Some(expect_exit_code) {
        return Err(format!(
            "converter finished with exit code {}\nsvn2git stdout:\n{}svn2git stderr:\n{}",
            cmd_out.status,
            String::from_utf8_lossy(&cmd_out.stdout),
            String::from_utf8_lossy(&cmd_out.stderr),
        ));
    }

    Ok(())
}

fn check_log(log_path: &Path, expected_pattern: &str) -> Result<(), String> {
    let log_data =
        std::fs::read(log_path).map_err(|e| format!("failed to read {log_path:?}: {e}"))?;

    let mut re_hir = Vec::new();
    re_hir.push(regex_hir::Hir::look(regex_hir::Look::Start));

    fn digits(n: u32) -> regex_hir::Hir {
        let digit = regex_hir::Hir::class(regex_hir::Class::Bytes(regex_hir::ClassBytes::new([
            regex_hir::ClassBytesRange::new(b'0', b'9'),
        ])));

        if n == 1 {
            digit
        } else {
            regex_hir::Hir::repetition(regex_hir::Repetition {
                min: n,
                max: Some(n),
                greedy: true,
                sub: Box::new(digit),
            })
        }
    }
    fn date_regex() -> regex_hir::Hir {
        regex_hir::Hir::concat(vec![
            digits(4),
            regex_hir::Hir::literal(b"-".as_slice()),
            digits(2),
            regex_hir::Hir::literal(b"-".as_slice()),
            digits(2),
            regex_hir::Hir::literal(b"T".as_slice()),
            digits(2),
            regex_hir::Hir::literal(b":".as_slice()),
            digits(2),
            regex_hir::Hir::literal(b":".as_slice()),
            digits(2),
            regex_hir::Hir::literal(b".".as_slice()),
            digits(6),
            regex_hir::Hir::literal(b"Z ".as_slice()),
        ])
    }
    fn wildcard_lines() -> regex_hir::Hir {
        regex_hir::Hir::repetition(regex_hir::Repetition {
            min: 0,
            max: None,
            greedy: true,
            sub: Box::new(regex_hir::Hir::concat(vec![
                date_regex(),
                regex_hir::Hir::repetition(regex_hir::Repetition {
                    min: 0,
                    max: None,
                    greedy: true,
                    sub: Box::new(regex_hir::Hir::dot(regex_hir::Dot::AnyByteExceptLF)),
                }),
                regex_hir::Hir::literal(b"\n".as_slice()),
            ])),
        })
    }

    re_hir.push(wildcard_lines());

    for pattern_line in expected_pattern.lines() {
        if pattern_line.is_empty() {
            continue;
        }

        let (level, line) = if let Some(line) = pattern_line.strip_prefix("D ") {
            ("DEBUG ", line)
        } else if let Some(line) = pattern_line.strip_prefix("I ") {
            (" INFO ", line)
        } else if let Some(line) = pattern_line.strip_prefix("W ") {
            (" WARN ", line)
        } else if let Some(line) = pattern_line.strip_prefix("E ") {
            ("ERROR ", line)
        } else {
            return Err(format!("invalid log pattern line: {pattern_line:?}"));
        };

        re_hir.push(date_regex());
        re_hir.push(regex_hir::Hir::literal(level.as_bytes()));
        re_hir.push(regex_hir::Hir::literal(line.as_bytes()));
        re_hir.push(regex_hir::Hir::literal(b"\n".as_slice()));
        re_hir.push(wildcard_lines());
    }
    re_hir.push(regex_hir::Hir::look(regex_hir::Look::End));
    let re_hir = regex_hir::Hir::concat(re_hir);

    let regex = regex_automata::meta::Builder::new()
        .build_from_hir(&re_hir)
        .expect("failed to build regex");

    if !regex.is_match(&log_data) {
        return Err(format!("unexpected log at {log_path:?}"));
    }

    Ok(())
}

fn check_git_tag(git_repo: &gix::Repository, git_tag: &defs::GitTag) -> Result<(), String> {
    let parsed_tag = git_repo
        .rev_parse_single(git_tag.tag.as_str())
        .map_err(|e| format!("failed to revparse {:?}: {e}", git_tag.tag))?;
    let parsed_rev = git_repo
        .rev_parse_single(git_tag.rev.as_str())
        .map_err(|e| format!("failed to revparse {:?}: {e}", git_tag.rev))?;

    let tag: gix::objs::Tag = parsed_tag
        .object()
        .map_err(|e| format!("failed to get object: {e}"))?
        .try_into_tag()
        .map_err(|e| format!("failed to get tag: {e}"))?
        .decode()
        .map_err(|e| format!("failed to decode tag: {e}"))?
        .into();

    if tag.target != parsed_rev {
        return Err(format!(
            "tag {:?} does not point to {:?}",
            git_tag.tag, git_tag.rev,
        ));
    }

    if let Some(ref expected_tagger) = git_tag.tagger {
        let tagger = tag.tagger.as_ref().ok_or("tag does not have tagger")?;
        check_git_signature("tagger", &tagger.to_ref(), expected_tagger)?;
    }

    if let Some(ref expected_msg) = git_tag.message {
        if tag.message != *expected_msg {
            return Err(format!(
                "unexpected tag message: {:?} != {expected_msg:?}",
                tag.message,
            ));
        }
    }

    Ok(())
}

fn check_git_rev(git_repo: &gix::Repository, git_rev: &defs::GitRev) -> Result<(), String> {
    let parsed_rev = git_repo
        .rev_parse_single(git_rev.rev.as_str())
        .map_err(|e| format!("failed to revparse {:?}: {e}", git_rev.rev))?;
    let rev_obj = parsed_rev
        .object()
        .map_err(|e| format!("failed to get object {:?}: {e}", git_rev.rev))?;

    let commit = rev_obj
        .try_into_commit()
        .map_err(|e| format!("failed to get commit {:?}: {e}", git_rev.rev))?;

    if let Some(ref expected_author) = git_rev.author {
        let author = commit
            .author()
            .map_err(|e| format!("failed to get commit author: {e}"))?;
        check_git_signature("author", &author, expected_author)?;
    }

    if let Some(ref expected_committer) = git_rev.committer {
        let committer = commit
            .committer()
            .map_err(|e| format!("failed to get commit committer: {e}"))?;
        check_git_signature("committer", &committer, expected_committer)?;
    }

    if let Some(ref expected_msg) = git_rev.message {
        let msg = commit
            .message_raw()
            .map_err(|e| format!("failed to get commit message: {e}"))?;
        if msg != expected_msg {
            return Err(format!(
                "unexpected commit message: {msg:?} != {expected_msg:?}"
            ));
        }
    }

    if let Some(ref same) = git_rev.same {
        for same in same.iter() {
            let parsed_same_rev = git_repo
                .rev_parse_single(same.as_str())
                .map_err(|e| format!("failed to revparse {same:?}: {e}"))?;
            if parsed_same_rev != parsed_rev {
                return Err(format!(
                    "{same:?} and {:?} are not the same commit",
                    git_rev.rev
                ));
            }
        }
    }

    if let Some(ref expected_parents) = git_rev.parents {
        let parent_ids = commit.parent_ids().collect::<Vec<_>>();
        if parent_ids.len() != expected_parents.len() {
            return Err(format!(
                "mismatched number of parents: expected {}, got {}",
                expected_parents.len(),
                parent_ids.len()
            ));
        }

        for (i, (&parent_id, expected_parent)) in
            parent_ids.iter().zip(expected_parents.iter()).enumerate()
        {
            let parsed_parent_rev = git_repo
                .rev_parse_single(expected_parent.as_str())
                .map_err(|e| format!("failed to revparse {expected_parent:?}: {e}"))?;
            if parsed_parent_rev != parent_id {
                return Err(format!(
                    "parent {i} of {:?} is not {expected_parent:?}",
                    git_rev.rev
                ));
            }
        }
    }

    if let Some(ref expected_tree) = git_rev.tree {
        let tree_id = commit
            .tree_id()
            .map_err(|e| format!("failed to get tree ID: {e}"))?;
        let expected_tree = expected_tree
            .iter()
            .map(|(k, v)| (k.as_bytes(), v))
            .collect();
        check_git_tree(tree_id, &expected_tree)?;
    }

    Ok(())
}

fn check_git_signature(
    which: &str,
    git_signature: &gix::actor::SignatureRef<'_>,
    expected: &defs::GitSignature,
) -> Result<(), String> {
    if git_signature.name != expected.name {
        return Err(format!(
            "unexpected {which} name: {:?} != {:?}",
            git_signature.name, expected.name,
        ));
    }
    if git_signature.email != expected.email {
        return Err(format!(
            "unexpected {which} email: {:?} != {:?}",
            git_signature.email, expected.email,
        ));
    }

    if let Some(ref expected_time) = expected.time {
        if git_signature.time.seconds != expected_time.seconds {
            return Err(format!(
                "unexpected {which} time seconds: {} != {}",
                git_signature.time.seconds, expected_time.seconds,
            ));
        }

        if git_signature.time.offset.unsigned_abs() != expected_time.offset {
            return Err(format!(
                "unexpected {which} time offset: {} != {}",
                git_signature.time.offset, expected_time.offset,
            ));
        }

        let sign = match git_signature.time.sign {
            gix::date::time::Sign::Plus => defs::GitTimeSign::Plus,
            gix::date::time::Sign::Minus => defs::GitTimeSign::Minus,
        };

        if sign != expected_time.sign {
            return Err(format!(
                "unexpected {which} time sign: {sign} != {}",
                expected_time.sign,
            ));
        }
    }

    Ok(())
}

fn check_git_tree(
    git_root_tree_id: gix::Id<'_>,
    expected: &BTreeMap<&[u8], &defs::GitTreeEntry>,
) -> Result<(), String> {
    let mut git_entries = BTreeMap::new();
    let mut tree_queue = Vec::new();

    tree_queue.push((vec![], git_root_tree_id));
    while let Some((tree_path, tree_id)) = tree_queue.pop() {
        let git_tree = tree_id
            .object()
            .map_err(|e| format!("failed to get git object {tree_id}: {e}"))?
            .try_into_tree()
            .map_err(|e| format!("failed to convert git object {tree_id} to tree: {e}"))?;

        for entry in git_tree.iter() {
            let entry = entry.map_err(|e| format!("failed to iterate over tree entries: {e}"))?;
            let mode = entry.mode();

            let mut entry_path = tree_path.clone();
            entry_path.push(entry.filename().to_owned());

            if mode.is_tree() {
                tree_queue.push((entry_path.clone(), entry.id()));
            }

            let entry_path = entry_path.join(b"/".as_slice());
            let prev = git_entries.insert(entry_path, (mode, entry.id()));
            assert!(prev.is_none());
        }
    }

    for (entry_path, (entry_mode, entry_id)) in git_entries.iter() {
        let Some(expected_entry) = expected.get(entry_path.as_slice()) else {
            return Err(format!(
                "unexpected tree entry: \"{}\"",
                entry_path.escape_ascii(),
            ));
        };

        match expected_entry {
            defs::GitTreeEntry::Normal {
                data: expected_data,
            } => {
                if !entry_mode.is_blob() || entry_mode.is_executable() {
                    return Err(format!(
                        "entry \"{}\" with mode {:o} was expected to be a regular file",
                        entry_path.escape_ascii(),
                        entry_mode.kind() as u16,
                    ));
                }

                let entry_obj = entry_id
                    .object()
                    .map_err(|e| format!("failed to convert tree entry to object: {e}"))?;
                let blob = entry_obj.into_blob();
                if blob.data != expected_data.as_bytes() {
                    return Err(format!(
                        "incorrect data in entry \"{}\": expected: \"{}\"\nactual: \"{}\"",
                        entry_path.escape_ascii(),
                        expected_data.as_bytes().escape_ascii(),
                        blob.data.escape_ascii(),
                    ));
                }
            }
            defs::GitTreeEntry::Exec {
                data: expected_data,
            } => {
                if !entry_mode.is_blob() || !entry_mode.is_executable() {
                    return Err(format!(
                        "entry \"{}\" with mode {} was expected to be an executable file",
                        entry_path.escape_ascii(),
                        entry_mode.kind().as_octal_str(),
                    ));
                }

                let entry_obj = entry_id
                    .object()
                    .map_err(|e| format!("failed to convert tree entry to object: {e}"))?;
                let blob = entry_obj.into_blob();
                if blob.data != expected_data.as_bytes() {
                    return Err(format!(
                        "incorrect data in entry \"{}\": expected: \"{}\"\nactual: \"{}\"",
                        entry_path.escape_ascii(),
                        expected_data.as_bytes().escape_ascii(),
                        blob.data.escape_ascii(),
                    ));
                }
            }
            defs::GitTreeEntry::Symlink {
                target: expected_target,
            } => {
                if !entry_mode.is_link() {
                    return Err(format!(
                        "entry \"{}\" with mode {} was expected to be a symbolic link",
                        entry_path.escape_ascii(),
                        entry_mode.kind().as_octal_str(),
                    ));
                }

                let entry_obj = entry_id
                    .object()
                    .map_err(|e| format!("failed to convert tree entry to object: {e}"))?;
                let blob = entry_obj.into_blob();
                if blob.data != expected_target.as_bytes() {
                    return Err(format!(
                        "incorrect data in entry \"{}\": expected: \"{}\"\nactual: \"{}\"",
                        entry_path.escape_ascii(),
                        expected_target.as_bytes().escape_ascii(),
                        blob.data.escape_ascii(),
                    ));
                }
            }
            defs::GitTreeEntry::Dir { .. } => {
                if !entry_mode.is_tree() {
                    return Err(format!(
                        "entry \"{}\" with mode {} was expected to be a directory",
                        entry_path.escape_ascii(),
                        entry_mode.kind().as_octal_str(),
                    ));
                }
            }
        }
    }

    for &entry_path in expected.keys() {
        if !git_entries.contains_key(entry_path) {
            return Err(format!(
                "missing tree entry: \"{}\"",
                entry_path.escape_ascii(),
            ));
        }
    }

    Ok(())
}
