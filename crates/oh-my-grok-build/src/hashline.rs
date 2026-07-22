//! Hashline-safe, token-efficient text edits.
//!
//! Patch blocks are anchored by a short blake3 hash of the old text so a model
//! can propose edits without needing exact line numbers.  If the hash does not
//! match the current file contents the whole patch is rejected.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::args::HashlineCommand;

const HASH_LEN: usize = 16;

#[derive(Debug)]
struct Block {
    expected_hash: String,
    old_text: String,
    new_text: String,
}

fn short_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex()[..HASH_LEN].to_string()
}

fn parse_patch(patch: &str) -> Result<Vec<Block>> {
    let mut blocks = Vec::new();
    let mut lines = patch.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with(">>>>HASH") {
            continue;
        }
        let expected_hash = line[">>>>HASH".len()..].trim().to_string();
        if expected_hash.len() != HASH_LEN {
            bail!("hashline patch hash must be {HASH_LEN} hex chars");
        }
        let mut old_lines: Vec<&str> = Vec::new();
        while let Some(l) = lines.peek() {
            if *l == "====" {
                break;
            }
            old_lines.push(lines.next().unwrap());
        }
        if lines.next().is_none() {
            bail!("hashline patch missing ==== separator");
        }
        let mut new_lines: Vec<&str> = Vec::new();
        while let Some(l) = lines.peek() {
            if *l == ">>>>END" {
                break;
            }
            new_lines.push(lines.next().unwrap());
        }
        if lines.next().is_none() {
            bail!("hashline patch missing >>>>END terminator");
        }
        blocks.push(Block {
            expected_hash,
            old_text: old_lines.join("\n"),
            new_text: new_lines.join("\n"),
        });
    }
    Ok(blocks)
}

fn find_unique_position(content: &str, needle: &str) -> Option<usize> {
    let mut count = 0;
    let mut pos = None;
    for (idx, _) in content.match_indices(needle) {
        count += 1;
        if count > 1 {
            return None;
        }
        pos = Some(idx);
    }
    pos
}

fn apply_blocks(content: &str, blocks: &[Block]) -> Result<String> {
    let mut positions: Vec<(usize, &Block)> = Vec::new();
    for b in blocks {
        let actual = short_hash(&b.old_text);
        if actual != b.expected_hash {
            bail!(
                "hashline mismatch: expected {} but old text hashes to {} (block may be stale)",
                b.expected_hash,
                actual
            );
        }
        let pos = find_unique_position(content, &b.old_text)
            .with_context(|| "hashline anchor matches zero or multiple places in file")?;
        positions.push((pos, b));
    }
    positions.sort_by_key(|(pos, _)| *pos);
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    let mut last_end = 0;
    for (pos, b) in positions {
        if pos < last_end {
            bail!("hashline patch blocks overlap");
        }
        out.push_str(&content[cursor..pos]);
        out.push_str(&b.new_text);
        cursor = pos + b.old_text.len();
        last_end = cursor;
    }
    out.push_str(&content[cursor..]);
    Ok(out)
}

pub fn apply_patch(file: &Path, patch_text: &str) -> Result<()> {
    let blocks = parse_patch(patch_text)?;
    if blocks.is_empty() {
        bail!("no hashline blocks found in patch");
    }
    let content =
        std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let new_content = apply_blocks(&content, &blocks)?;
    std::fs::write(file, new_content).with_context(|| format!("write {}", file.display()))?;
    Ok(())
}

pub fn verify_patch(file: &Path, patch_text: &str) -> Result<()> {
    let blocks = parse_patch(patch_text)?;
    let content = std::fs::read_to_string(file)?;
    for b in blocks {
        let actual = short_hash(&b.old_text);
        if actual != b.expected_hash {
            bail!(
                "hashline mismatch for block starting with hash {}",
                b.expected_hash
            );
        }
        if find_unique_position(&content, &b.old_text).is_none() {
            bail!("hashline anchor not found or ambiguous in file");
        }
    }
    Ok(())
}

pub fn run_hashline(cmd: HashlineCommand) -> Result<()> {
    match cmd {
        HashlineCommand::Apply(args) => {
            let patch_text = read_patch(&args.patch)?;
            apply_patch(&args.file, &patch_text)?;
            println!("applied hashline patch to {}", args.file.display());
        }
        HashlineCommand::Verify(args) => {
            let patch_text = read_patch(&args.patch)?;
            verify_patch(&args.file, &patch_text)?;
            println!("hashline patch verifies for {}", args.file.display());
        }
    }
    Ok(())
}

fn read_patch(path_or_dash: &str) -> Result<String> {
    if path_or_dash == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read patch from stdin")?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path_or_dash).with_context(|| format!("read patch {path_or_dash}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_patch(old: &str, new: &str) -> String {
        format!(
            ">>>>HASH {hash}\n{old}\n====\n{new}\n>>>>END\n",
            hash = short_hash(old),
            old = old,
            new = new
        )
    }

    #[test]
    fn apply_basic_patch() {
        let tmp = std::env::temp_dir().join(format!("hashline-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("test.txt");
        std::fs::write(&file, "foo\nbar\nbaz\n").unwrap();
        let patch = make_patch("bar", "qux");
        apply_patch(&file, &patch).unwrap();
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "foo\nqux\nbaz\n");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn verify_patch_before_apply() {
        let tmp = std::env::temp_dir().join(format!("hashline-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("test.txt");
        std::fs::write(&file, "foo\nbar\nbaz\n").unwrap();
        let patch = make_patch("bar", "qux");
        verify_patch(&file, &patch).unwrap();
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn rejects_mismatched_hash() {
        let tmp = std::env::temp_dir().join(format!("hashline-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("test.txt");
        std::fs::write(&file, "foo\nbar\nbaz\n").unwrap();
        let mut patch = make_patch("bar", "qux");
        patch = patch.replace(&short_hash("bar"), "0000000000000000");
        assert!(apply_patch(&file, &patch).is_err());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn rejects_ambiguous_anchor() {
        let tmp = std::env::temp_dir().join(format!("hashline-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("test.txt");
        std::fs::write(&file, "foo\nbar\nbar\nbaz\n").unwrap();
        let patch = make_patch("bar", "qux");
        assert!(apply_patch(&file, &patch).is_err());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
