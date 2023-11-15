#![allow(clippy::or_fun_call)]
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{anyhow, Context as _};
use chrono::prelude::*;
use json_color::{Color, Colorizer};

use radicle::identity::Id;
use radicle::identity::Identity;
use radicle::node::tracking::Policy;
use radicle::node::AliasStore as _;
use radicle::storage::refs::RefsAt;
use radicle::storage::{ReadRepository, ReadStorage};

use crate::terminal as term;
use crate::terminal::args::{Args, Error, Help};

pub const HELP: Help = Help {
    name: "inspect",
    description: "Inspect a radicle repository",
    version: env!("CARGO_PKG_VERSION"),
    usage: r#"
Usage

    rad inspect <path> [<option>...]
    rad inspect <rid>  [<option>...]
    rad inspect [<option>...]

    Inspects the given path or RID. If neither is specified,
    the current repository is inspected.

Options

    --rid        Return the repository identifier (RID)
    --payload    Inspect the repository's identity payload
    --refs       Inspect the repository's refs on the local device
    --sigrefs    Inspect the values of `rad/sigrefs` for all remotes of this repository
    --identity   Inspect the identity document
    --visibility Inspect the repository's visibility
    --delegates  Inspect the repository's delegates
    --policy     Inspect the repository's tracking policy
    --history    Show the history of the repository identity document
    --help       Print help
"#,
};

#[derive(Default, Debug, Eq, PartialEq)]
pub enum Target {
    Refs,
    Payload,
    Delegates,
    Identity,
    Visibility,
    Sigrefs,
    Policy,
    History,
    #[default]
    RepoId,
}

#[derive(Default, Debug, Eq, PartialEq)]
pub struct Options {
    pub rid: Option<Id>,
    pub target: Target,
}

impl Args for Options {
    fn from_args(args: Vec<OsString>) -> anyhow::Result<(Self, Vec<OsString>)> {
        use lexopt::prelude::*;

        let mut parser = lexopt::Parser::from_args(args);
        let mut rid: Option<Id> = None;
        let mut target = Target::default();

        while let Some(arg) = parser.next()? {
            match arg {
                Long("help") | Short('h') => {
                    return Err(Error::Help.into());
                }
                Long("refs") => {
                    target = Target::Refs;
                }
                Long("payload") => {
                    target = Target::Payload;
                }
                Long("policy") => {
                    target = Target::Policy;
                }
                Long("delegates") => {
                    target = Target::Delegates;
                }
                Long("history") => {
                    target = Target::History;
                }
                Long("identity") => {
                    target = Target::Identity;
                }
                Long("sigrefs") => {
                    target = Target::Sigrefs;
                }
                Long("rid") => {
                    target = Target::RepoId;
                }
                Long("visibility") => {
                    target = Target::Visibility;
                }
                Value(val) if rid.is_none() => {
                    let val = val.to_string_lossy();

                    if let Ok(val) = Id::from_str(&val) {
                        rid = Some(val);
                    } else if let Ok(val) = PathBuf::from_str(&val) {
                        rid = radicle::rad::at(val)
                            .map(|(_, id)| Some(id))
                            .context("Supplied argument is not a valid path")?;
                    } else {
                        return Err(anyhow!("invalid path or RID '{}'", val));
                    }
                }
                _ => return Err(anyhow::anyhow!(arg.unexpected())),
            }
        }

        Ok((Options { rid, target }, vec![]))
    }
}

pub fn run(options: Options, ctx: impl term::Context) -> anyhow::Result<()> {
    let rid = match options.rid {
        Some(rid) => rid,
        None => radicle::rad::cwd()
            .map(|(_, rid)| rid)
            .context("Current directory is not a radicle project")?,
    };

    if options.target == Target::RepoId {
        term::info!("{}", term::format::highlight(rid.urn()));
        return Ok(());
    }

    let profile = ctx.profile()?;
    let storage = &profile.storage;
    let signer = term::signer(&profile)?;
    let repo = storage
        .repository(rid)
        .context("No project with the given RID exists")?;
    let project = repo.identity_doc()?;

    match options.target {
        Target::Refs => {
            refs(&repo)?;
        }
        Target::Payload => {
            println!(
                "{}",
                colorizer().colorize_json_str(&serde_json::to_string_pretty(&project.payload)?)?
            );
        }
        Target::Identity => {
            println!(
                "{}",
                colorizer().colorize_json_str(&serde_json::to_string_pretty(&project.doc)?)?
            );
        }
        Target::Sigrefs => {
            for remote in repo.remote_ids()? {
                let remote = remote?;
                let refs = RefsAt::new(&repo, remote)?;

                println!(
                    "{:<48} {}",
                    term::format::tertiary(remote.to_human()),
                    term::format::secondary(refs.at)
                );
            }
        }
        Target::Policy => {
            let tracking = profile.tracking()?;
            if let Some(repo) = tracking.repo_policy(&rid)? {
                let tracking = match repo.policy {
                    Policy::Track => term::format::positive("tracked"),
                    Policy::Block => term::format::negative("blocked"),
                };
                println!(
                    "Repository {} is {} with scope {}",
                    term::format::tertiary(&rid),
                    tracking,
                    term::format::dim(format!("`{}`", repo.scope))
                );
            } else {
                term::print(term::format::italic(format!(
                    "No tracking policy found for {rid}"
                )));
            }
        }
        Target::Delegates => {
            let aliases = profile.aliases();
            for did in project.doc.delegates {
                if let Some(alias) = aliases.alias(&did) {
                    println!(
                        "{} {}",
                        term::format::tertiary(&did),
                        term::format::parens(term::format::dim(alias))
                    );
                } else {
                    println!("{}", term::format::tertiary(&did));
                }
            }
        }
        Target::Visibility => {
            println!("{}", term::format::visibility(&project.doc.visibility));
        }
        Target::History => {
            let identity = Identity::load(&repo)?;
            let head = repo.identity_head_of(signer.public_key())?;
            let history = repo.revwalk(head)?;

            for oid in history {
                let oid = oid?.into();
                let tip = repo.commit(oid)?;

                let Some(revision) = identity.revision(&tip.id().into()) else {
                    continue;
                };
                if !revision.is_accepted() {
                    continue;
                }
                let doc = &revision.doc;
                let timezone = if tip.time().sign() == '+' {
                    #[allow(deprecated)]
                    FixedOffset::east(tip.time().offset_minutes() * 60)
                } else {
                    #[allow(deprecated)]
                    FixedOffset::west(tip.time().offset_minutes() * 60)
                };
                let time = DateTime::<Utc>::from(
                    std::time::UNIX_EPOCH
                        + std::time::Duration::from_secs(tip.time().seconds() as u64),
                )
                .with_timezone(&timezone)
                .to_rfc2822();

                println!(
                    "{} {}",
                    term::format::yellow("commit"),
                    term::format::yellow(oid),
                );
                if let Ok(parent) = tip.parent_id(0) {
                    println!("parent {parent}");
                }
                println!("blob   {}", revision.blob);
                println!("date   {time}");
                println!();

                if let Some(msg) = tip.message() {
                    for line in msg.lines() {
                        if line.is_empty() {
                            println!();
                        } else {
                            term::indented(term::format::dim(line));
                        }
                    }
                    term::blank();
                }

                let json = colorizer().colorize_json_str(&serde_json::to_string_pretty(&doc)?)?;
                for line in json.lines() {
                    println!(" {line}");
                }
                println!();
            }
        }
        Target::RepoId => {
            // Handled above.
        }
    }

    Ok(())
}

// Used for JSON Colorizing
fn colorizer() -> Colorizer {
    Colorizer::new()
        .null(Color::Cyan)
        .boolean(Color::Cyan)
        .number(Color::Magenta)
        .string(Color::Green)
        .key(Color::Blue)
        .build()
}

fn refs(repo: &radicle::storage::git::Repository) -> anyhow::Result<()> {
    let mut refs = Vec::new();
    for r in repo.references()? {
        let r = r?;
        if let Some(namespace) = r.namespace {
            refs.push(format!("{}/{}", namespace, r.name));
        }
    }

    print!("{}", tree(refs));

    Ok(())
}

/// Show the list of given git references as a newline terminated tree `String` similar to the tree command.
fn tree(mut refs: Vec<String>) -> String {
    refs.sort();

    // List of references with additional unique entries for each 'directory'.
    //
    // i.e. "refs/heads/master" becomes ["refs"], ["refs", "heads"], and ["refs", "heads",
    // "master"].
    let mut refs_expanded: Vec<Vec<String>> = Vec::new();
    // Number of entries per Git 'directory'.
    let mut ref_entries: HashMap<Vec<String>, usize> = HashMap::new();
    let mut last: Vec<String> = Vec::new();

    for r in refs {
        let r: Vec<String> = r.split('/').map(|s| s.to_string()).collect();

        for (i, v) in r.iter().enumerate() {
            let last_v = last.get(i);
            if Some(v) != last_v {
                last = r.clone().iter().take(i + 1).map(String::from).collect();

                refs_expanded.push(last.clone());

                let mut dir = last.clone();
                dir.pop();
                if dir.is_empty() {
                    continue;
                }

                if let Some(num) = ref_entries.get_mut(&dir) {
                    *num += 1;
                } else {
                    ref_entries.insert(dir, 1);
                }
            }
        }
    }
    let mut tree = String::default();

    for mut ref_components in refs_expanded {
        // Better to explode when things do not go as expected.
        let name = ref_components.pop().expect("non-empty vector");
        if ref_components.is_empty() {
            tree.push_str(&format!("{name}\n"));
            continue;
        }

        for i in 1..ref_components.len() {
            let parent: Vec<String> = ref_components.iter().take(i).cloned().collect();

            let num = ref_entries.get(&parent).unwrap_or(&0);
            if *num == 0 {
                tree.push_str("    ");
            } else {
                tree.push_str("│   ");
            }
        }

        if let Some(num) = ref_entries.get_mut(&ref_components) {
            if *num == 1 {
                tree.push_str(&format!("└── {name}\n"));
            } else {
                tree.push_str(&format!("├── {name}\n"));
            }
            *num -= 1;
        }
    }

    tree
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_tree() {
        let arg = vec![
            String::from("z6MknSLrJoTcukLrE435hVNQT4JUhbvWLX4kUzqkEStBU8Vi/refs/heads/master"),
            String::from("z6MknSLrJoTcukLrE435hVNQT4JUhbvWLX4kUzqkEStBU8Vi/refs/rad/id"),
            String::from("z6MknSLrJoTcukLrE435hVNQT4JUhbvWLX4kUzqkEStBU8Vi/refs/rad/sigrefs"),
        ];
        let exp = r#"
z6MknSLrJoTcukLrE435hVNQT4JUhbvWLX4kUzqkEStBU8Vi
└── refs
    ├── heads
    │   └── master
    └── rad
        ├── id
        └── sigrefs
"#
        .trim_start();

        assert_eq!(tree(arg), exp);
        assert_eq!(tree(vec![String::new()]), "\n");
    }
}
