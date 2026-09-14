//! The hosts the user can already `ssh` to, read out of their own `~/.ssh/config`.
//!
//! Nothing about a machine is configured twice: a host you reach by typing `ssh theresa` is a
//! host the pet can offer to watch, under that same name. Only the *names* are read here. Every
//! other setting stays ssh's business, because the source runs the system `ssh` binary rather
//! than speaking the protocol itself — so `ProxyJump`, `IdentityFile`, `Port` and the rest apply
//! without this file ever having heard of them.
//!
//! This is a list of what could be offered, not of what works. A name here has never been
//! connected to, and may well be a host that is switched off, or one with no `remi-hook` on it.
//! Finding that out is what pressing Connect does.

use std::path::{Path, PathBuf};

/// How deep `Include` is followed before we assume the files include each other in a circle.
/// OpenSSH's own limit is 16; nobody legitimately nests this far.
const MAX_INCLUDE_DEPTH: u8 = 8;

/// Every host `~/.ssh/config` names, in the order it names them — the user's own grouping, which
/// is more use than alphabetical.
///
/// Patterns (`Host web-*`, `Host !bastion`) are left out: they configure other hosts rather than
/// naming one, so there is nothing to connect to. A host the user wants that ssh only matches by
/// pattern can still be written into the pet's own config by hand.
pub fn hosts() -> Vec<String> {
    let Some(base) = directories::BaseDirs::new() else {
        tracing::warn!("no home directory: offering no ssh hosts");
        return Vec::new();
    };
    let dir = base.home_dir().join(".ssh");
    hosts_in(&dir.join("config"))
}

/// [`hosts`], reading a named file rather than the user's own.
pub fn hosts_in(path: &Path) -> Vec<String> {
    let mut found = Vec::new();
    read(path, &mut found, 0);
    found
}

fn read(path: &Path, found: &mut Vec<String>, depth: u8) {
    if depth > MAX_INCLUDE_DEPTH {
        tracing::warn!(
            "not following {} : Include is nested too deep",
            path.display()
        );
        return;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // Having no ssh config at all is an ordinary state, not a problem to report.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => return tracing::warn!("reading {}: {err}", path.display()),
    };

    for line in text.lines() {
        let Some((keyword, arguments)) = split(line) else {
            continue;
        };
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                for name in arguments.filter(|name| is_literal(name)) {
                    // The same host may be named by more than one line, and an included file may
                    // name one the including file already did.
                    if !found.iter().any(|seen| seen == name) {
                        found.push(name.to_owned());
                    }
                }
            }
            "include" => {
                let relative_to = path.parent().unwrap_or(Path::new("."));
                for argument in arguments {
                    for file in expand(argument, relative_to) {
                        read(&file, found, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// A config line as a keyword and its arguments, or `None` for a blank or a comment.
///
/// ssh accepts `Host theresa` and `Host=theresa` alike, and treats `#` as a comment only where it
/// starts the line — a `#` further along is part of an argument.
fn split(line: &str) -> Option<(&str, impl Iterator<Item = &str>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (keyword, rest) = match line.find(['=', ' ', '\t']) {
        Some(end) => (
            &line[..end],
            line[end + 1..].trim_start_matches(['=', ' ', '\t']),
        ),
        None => (line, ""),
    };
    Some((keyword, rest.split_whitespace()))
}

/// Whether this is a host's name rather than a pattern matching other hosts'.
fn is_literal(name: &str) -> bool {
    !name.is_empty() && !name.contains(['*', '?', '!'])
}

/// The files an `Include` argument names. `~` is the user's home, a relative path is relative to
/// the including file's directory, and `*`/`?` in the final component are matched against that
/// directory — which is the shape people actually write (`Include config.d/*`). A wildcard higher
/// up the path is not expanded; ssh would, and the cost of not doing so is a host going unoffered.
fn expand(argument: &str, relative_to: &Path) -> Vec<PathBuf> {
    let path = match argument.strip_prefix("~/") {
        Some(rest) => match directories::BaseDirs::new() {
            Some(base) => base.home_dir().join(rest),
            None => return Vec::new(),
        },
        None => relative_to.join(argument),
    };

    let Some(pattern) = path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    if is_literal(pattern) {
        return vec![path];
    }

    let dir = path.parent().unwrap_or(Path::new("."));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut matched: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| matches(pattern, name))
        })
        .map(|entry| entry.path())
        .collect();
    // A directory listing has no useful order of its own, and ssh reads an Include's matches in
    // lexical order.
    matched.sort();
    matched
}

/// Matches a shell-style pattern of `*` and `?` against one path component. Written out rather
/// than pulled in, because this is the only place in the project that needs it.
fn matches(pattern: &str, name: &str) -> bool {
    let (pattern, name): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), name.chars().collect());
    // The furthest we have got, and where to resume from if the last `*` turns out to have been
    // too short. Backtracking to one remembered `*` is enough: an earlier one can always give
    // away what a later one takes.
    let (mut p, mut n) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                p += 1;
                resume = n;
            }
            Some('?') => {
                p += 1;
                n += 1;
            }
            Some(&literal) if literal == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some(at) => {
                    p = at + 1;
                    resume += 1;
                    n = resume;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&char| char == '*')
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn config(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn reads_hosts_in_the_order_the_file_names_them() {
        let (_dir, path) = config(
            "# the home boxes\n\
             Host plume\n  HostName plume.example\n  User someone\n\n\
             Host theresa\n  HostName theresa.example\n",
        );

        assert_eq!(hosts_in(&path), ["plume", "theresa"]);
    }

    #[test]
    fn one_line_may_name_several_hosts_and_none_is_listed_twice() {
        let (_dir, path) = config("Host a b\nHost b c\n");

        assert_eq!(hosts_in(&path), ["a", "b", "c"]);
    }

    #[test]
    fn patterns_configure_other_hosts_rather_than_naming_one() {
        let (_dir, path) =
            config("Host *\n  ServerAliveInterval 60\nHost web-?\nHost !gate\nHost real\n");

        assert_eq!(hosts_in(&path), ["real"]);
    }

    #[test]
    fn a_keyword_may_be_joined_to_its_argument_by_an_equals_sign() {
        let (_dir, path) = config("Host=joined\n\tHost \tspaced\n");

        assert_eq!(hosts_in(&path), ["joined", "spaced"]);
    }

    #[test]
    fn an_included_file_is_read_where_it_is_included() {
        let (dir, path) = config("Host first\nInclude more\nHost last\n");
        fs::write(dir.path().join("more"), "Host included\n").unwrap();

        assert_eq!(hosts_in(&path), ["first", "included", "last"]);
    }

    #[test]
    fn an_include_matches_a_wildcard_against_its_directory() {
        let (dir, path) = config("Include conf.d/*.conf\n");
        fs::create_dir(dir.path().join("conf.d")).unwrap();
        fs::write(dir.path().join("conf.d").join("b.conf"), "Host second\n").unwrap();
        fs::write(dir.path().join("conf.d").join("a.conf"), "Host first\n").unwrap();
        fs::write(
            dir.path().join("conf.d").join("notes.txt"),
            "Host ignored\n",
        )
        .unwrap();

        assert_eq!(hosts_in(&path), ["first", "second"]);
    }

    #[test]
    fn files_that_include_each_other_stop_rather_than_recurse_forever() {
        let (dir, path) = config("Host one\nInclude other\n");
        fs::write(dir.path().join("other"), "Host two\nInclude config\n").unwrap();

        assert_eq!(hosts_in(&path), ["one", "two"]);
    }

    #[test]
    fn no_config_file_is_not_a_problem() {
        let dir = tempfile::tempdir().unwrap();

        assert!(hosts_in(&dir.path().join("nothing-here")).is_empty());
    }

    #[test]
    fn wildcards_match_the_way_a_shell_would() {
        assert!(matches("*.conf", "work.conf"));
        assert!(matches("*.conf", ".conf"));
        assert!(!matches("*.conf", "conf"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "ac"));
        assert!(matches("*", "anything"));
        assert!(matches("a*b*c", "a-bb-ccc"));
        assert!(!matches("a*b*c", "a-bb-ccd"));
    }
}
