//! Building and recognising the `statusLine.command` string.
//!
//! On Windows Claude Code runs the command through Git Bash when it is
//! installed and through PowerShell otherwise. Git Bash eats unquoted
//! backslashes, so paths are written with forward slashes. All of that is
//! expressed as pure functions of (path string, `windows` flag, shell flavour)
//! so it is covered by unit tests on any OS.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// File stem of our binary. Used to recognise our own entry in a statusLine
/// command so we never chain to ourselves.
pub const BIN_STEM: &str = "cooldown-statusline";

/// Env var Claude Code honours to locate `bash.exe` on Windows.
pub const ENV_GIT_BASH: &str = "CLAUDE_CODE_GIT_BASH_PATH";

/// Which shell syntax the command string must be valid in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFlavor {
    /// POSIX `sh` / Git Bash.
    Posix,
    /// Windows PowerShell.
    PowerShell,
}

impl ShellFlavor {
    /// Name used in CLI output and the `--shell` option.
    pub fn name(self) -> &'static str {
        match self {
            Self::Posix => "bash",
            Self::PowerShell => "powershell",
        }
    }
}

/// Normalise an executable path for embedding in a settings command.
///
/// On Windows this turns `C:\Users\me\x.exe` into `C:/Users/me/x.exe`, strips
/// the `\\?\` verbatim prefix that `canonicalize` produces, and maps
/// `\\?\UNC\server\share` to `//server/share`. Elsewhere the path is returned
/// untouched (a backslash is a legal filename character there).
pub fn normalize_exe_path(path: &str, windows: bool) -> String {
    if !windows {
        return path.to_owned();
    }
    let path = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.to_owned()
    };
    path.replace('\\', "/")
}

/// Characters a POSIX shell takes literally in an unquoted word.
fn is_posix_safe(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '-' | '.' | '/' | ':' | '+' | '@' | '%' | ',' | '=')
}

/// Characters PowerShell takes literally in an unquoted command word. Narrower than POSIX: a bare
/// `,` makes an array, a bare `@` starts splatting or an array, and `%`, `+` and `=` are operators
/// in some positions, so a path with any of them (or any non-ASCII letter) is quoted instead.
fn is_powershell_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':')
}

/// PowerShell ends (or, doubled, escapes) a single-quoted string at the ASCII apostrophe and at
/// each of these typographic single quotes, so a profile folder such as `C:/Users/O’Brien` breaks
/// out of the string unless every one of them is doubled.
fn is_powershell_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

/// `arg` as a PowerShell single-quoted string: nothing inside is special except the quote
/// characters, and doubling a quote character produces that character.
fn powershell_single_quoted(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for c in arg.chars() {
        out.push(c);
        if is_powershell_single_quote(c) {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Quote `arg` as one word for `flavor`, leaving it bare when that is safe.
fn quote(arg: &str, flavor: ShellFlavor) -> String {
    match flavor {
        ShellFlavor::Posix if !arg.is_empty() && arg.chars().all(is_posix_safe) => arg.to_owned(),
        ShellFlavor::Posix => format!("'{}'", arg.replace('\'', r"'\''")),
        // PowerShell only runs a quoted path when it is invoked with `&`.
        ShellFlavor::PowerShell if !arg.is_empty() && arg.chars().all(is_powershell_safe) => {
            arg.to_owned()
        }
        ShellFlavor::PowerShell => format!("& {}", powershell_single_quoted(arg)),
    }
}

/// The exact `statusLine.command` string for the binary at `exe`.
pub fn command_for_exe(exe: &str, windows: bool, flavor: ShellFlavor) -> String {
    quote(&normalize_exe_path(exe, windows), flavor)
}

/// Decode the first shell word of `command` (POSIX rules: quotes and
/// backslash escapes, `& ` prefix tolerated for PowerShell).
fn first_word_unquoted(command: &str) -> Option<String> {
    let mut rest = command.trim_start();
    if let Some(after) = rest.strip_prefix('&') {
        rest = after.trim_start();
    }
    let mut word = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => break,
            '\'' => {
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                            word.extend(chars.next());
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => word.extend(chars.next()),
            _ => word.push(c),
        }
    }
    (!word.is_empty()).then_some(word)
}

/// The first whitespace-delimited token with surrounding quotes removed and
/// backslashes left alone (what a hand-written Windows path looks like).
fn first_word_raw(command: &str) -> Option<String> {
    let mut rest = command.trim_start();
    if let Some(after) = rest.strip_prefix('&') {
        rest = after.trim_start();
    }
    let token = rest.split_whitespace().next()?;
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    (!token.is_empty()).then(|| token.to_owned())
}

fn stem_is_ours(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    stem == BIN_STEM
}

/// True when `command` runs the Cooldown statusline binary (from any
/// location). Used so install never records ourselves as the command to chain
/// to, and so uninstall can tell our entry from someone else's.
pub fn is_our_command(command: &str) -> bool {
    [first_word_unquoted(command), first_word_raw(command)]
        .into_iter()
        .flatten()
        .any(|w| stem_is_ours(&w))
}

/// Locate Git Bash's `bash.exe` on Windows, the way Claude Code does: the
/// `CLAUDE_CODE_GIT_BASH_PATH` override, then the install that owns a
/// `git.exe` on `PATH`, then the standard install folders. Environment and
/// filesystem access are injected so the search is unit-testable.
pub fn find_git_bash(
    get_env: &dyn Fn(&str) -> Option<OsString>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(explicit) = get_env(ENV_GIT_BASH).filter(|v| !v.is_empty()) {
        let explicit = PathBuf::from(explicit);
        if exists(&explicit) {
            return Some(explicit);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = get_env("PATH") {
        for dir in std::env::split_paths(&path) {
            if exists(&dir.join("git.exe")) {
                if let Some(root) = dir.parent() {
                    candidates.push(root.join("bin").join("bash.exe"));
                    candidates.push(root.join("usr").join("bin").join("bash.exe"));
                }
            }
        }
    }
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(base) = get_env(var).filter(|v| !v.is_empty()) {
            candidates.push(PathBuf::from(base).join("Git").join("bin").join("bash.exe"));
        }
    }
    if let Some(local) = get_env("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        candidates.push(
            PathBuf::from(local)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        );
    }
    candidates.into_iter().find(|c| exists(c))
}

/// [`find_git_bash`] against the real environment and filesystem.
pub fn detect_git_bash() -> Option<PathBuf> {
    find_git_bash(&|k| std::env::var_os(k), &|p| p.is_file())
}

/// Shell syntax to emit for this machine: Posix everywhere except a Windows
/// box without Git Bash.
pub fn default_flavor(windows: bool, git_bash_found: bool) -> ShellFlavor {
    if windows && !git_bash_found {
        ShellFlavor::PowerShell
    } else {
        ShellFlavor::Posix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn windows_paths_use_forward_slashes() {
        assert_eq!(
            normalize_exe_path(
                r"C:\Users\Me\AppData\Local\Cooldown\cooldown-statusline.exe",
                true
            ),
            "C:/Users/Me/AppData/Local/Cooldown/cooldown-statusline.exe"
        );
        assert_eq!(
            normalize_exe_path("C:/already/fine.exe", true),
            "C:/already/fine.exe"
        );
    }

    #[test]
    fn verbatim_prefixes_are_stripped_on_windows() {
        assert_eq!(
            normalize_exe_path(r"\\?\C:\Program Files\Cooldown\x.exe", true),
            "C:/Program Files/Cooldown/x.exe"
        );
        assert_eq!(
            normalize_exe_path(r"\\?\UNC\server\share\x.exe", true),
            "//server/share/x.exe"
        );
    }

    #[test]
    fn posix_paths_are_not_touched() {
        assert_eq!(
            normalize_exe_path("/opt/we\\ird/x", false),
            "/opt/we\\ird/x"
        );
    }

    #[test]
    fn bare_path_when_safe() {
        assert_eq!(
            command_for_exe(
                r"C:\Users\Me\bin\cooldown-statusline.exe",
                true,
                ShellFlavor::Posix
            ),
            "C:/Users/Me/bin/cooldown-statusline.exe"
        );
        assert_eq!(
            command_for_exe(
                "/usr/local/bin/cooldown-statusline",
                false,
                ShellFlavor::Posix
            ),
            "/usr/local/bin/cooldown-statusline"
        );
        assert_eq!(
            command_for_exe(r"C:\Users\Me\bin\x.exe", true, ShellFlavor::PowerShell),
            "C:/Users/Me/bin/x.exe"
        );
    }

    #[test]
    fn spaces_are_quoted_per_shell() {
        let exe = r"C:\Program Files\Cooldown\cooldown-statusline.exe";
        assert_eq!(
            command_for_exe(exe, true, ShellFlavor::Posix),
            "'C:/Program Files/Cooldown/cooldown-statusline.exe'"
        );
        assert_eq!(
            command_for_exe(exe, true, ShellFlavor::PowerShell),
            "& 'C:/Program Files/Cooldown/cooldown-statusline.exe'"
        );
    }

    #[test]
    fn apostrophes_and_metacharacters_are_escaped() {
        assert_eq!(
            command_for_exe("/Users/o'brien/bin/x", false, ShellFlavor::Posix),
            r"'/Users/o'\''brien/bin/x'"
        );
        assert_eq!(
            command_for_exe("/Users/o'brien/bin/x", false, ShellFlavor::PowerShell),
            "& '/Users/o''brien/bin/x'"
        );
        assert_eq!(
            command_for_exe("/tmp/$HOME;rm -rf/x", false, ShellFlavor::Posix),
            "'/tmp/$HOME;rm -rf/x'"
        );
    }

    /// What PowerShell's tokenizer makes of a single-quoted string: it ends at the first quote
    /// character (ASCII or typographic) that is not followed by another, and a doubled pair is the
    /// second character. Returns the string and whatever follows it.
    fn powershell_unquote(text: &str) -> (String, String) {
        let mut chars = text.chars().peekable();
        assert_eq!(chars.next(), Some('\''), "{text}");
        let mut out = String::new();
        while let Some(c) = chars.next() {
            if is_powershell_single_quote(c) {
                match chars.peek() {
                    Some(&next) if is_powershell_single_quote(next) => {
                        chars.next();
                        out.push(next);
                        continue;
                    }
                    _ => return (out, chars.collect()),
                }
            }
            out.push(c);
        }
        panic!("unterminated string: {text}");
    }

    #[test]
    fn powershell_typographic_quotes_are_doubled_like_the_ascii_one() {
        // C:/Users/O’Brien: U+2019 would otherwise end the string and run the rest as code.
        assert_eq!(
            command_for_exe(
                "C:\\Users\\O\u{2019}Brien\\AppData\\cooldown-statusline.exe",
                true,
                ShellFlavor::PowerShell
            ),
            "& 'C:/Users/O\u{2019}\u{2019}Brien/AppData/cooldown-statusline.exe'"
        );
        assert_eq!(
            command_for_exe("/Users/o'brien/x", false, ShellFlavor::PowerShell),
            "& '/Users/o''brien/x'"
        );
        for quote in ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'] {
            let exe = format!("C:/Users/a{quote}b/cooldown-statusline.exe");
            assert_eq!(
                command_for_exe(&exe, true, ShellFlavor::PowerShell),
                format!("& 'C:/Users/a{quote}{quote}b/cooldown-statusline.exe'"),
                "U+{:04X}",
                u32::from(quote)
            );
        }
    }

    #[test]
    fn powershell_reads_the_quoted_path_back_exactly() {
        // Mixed, adjacent and leading/trailing quote characters of all five kinds.
        for name in [
            "O\u{2019}Brien",
            "it's",
            "\u{2018}\u{2019}\u{201A}\u{201B}'",
            "'\u{2019}",
            "x\u{201B}",
            "a\u{2019}\u{2019}b",
            "a'\u{2019}b",
        ] {
            let exe = format!("C:/Users/{name}/cooldown-statusline.exe");
            let command = command_for_exe(&exe, true, ShellFlavor::PowerShell);
            let literal = command.strip_prefix("& ").expect("quoted with &");
            let (path, rest) = powershell_unquote(literal);
            assert_eq!(path, exe, "{command}");
            assert_eq!(rest, "", "nothing may follow the closing quote: {command}");
            assert!(is_our_command(&command), "{command}");
        }
    }

    #[test]
    fn powershell_operators_in_a_path_are_quoted() {
        for (exe, quoted) in [
            (r"C:\Users\a,b\x.exe", "& 'C:/Users/a,b/x.exe'"),
            (r"C:\Users\me@corp\x.exe", "& 'C:/Users/me@corp/x.exe'"),
            (r"C:\Users\a%b\x.exe", "& 'C:/Users/a%b/x.exe'"),
            (r"C:\Users\a+b\x.exe", "& 'C:/Users/a+b/x.exe'"),
            (r"C:\Users\a=b\x.exe", "& 'C:/Users/a=b/x.exe'"),
            (r"C:\Users\a;b\x.exe", "& 'C:/Users/a;b/x.exe'"),
            (r"C:\Users\$env\x.exe", "& 'C:/Users/$env/x.exe'"),
            (r"C:\Users\(a)\x.exe", "& 'C:/Users/(a)/x.exe'"),
            (r"C:\Users\a`b\x.exe", "& 'C:/Users/a`b/x.exe'"),
            ("C:\\Users\\Zo\u{eb}\\x.exe", "& 'C:/Users/Zo\u{eb}/x.exe'"),
            (r"C:\Users\a b\x.exe", "& 'C:/Users/a b/x.exe'"),
        ] {
            assert_eq!(
                command_for_exe(exe, true, ShellFlavor::PowerShell),
                quoted,
                "{exe}"
            );
        }
        // Plain paths stay bare, and an empty one is quoted rather than vanishing.
        assert_eq!(
            command_for_exe(r"C:\Users\me\bin\x_y-1.exe", true, ShellFlavor::PowerShell),
            "C:/Users/me/bin/x_y-1.exe"
        );
        assert_eq!(quote("", ShellFlavor::PowerShell), "& ''");
    }

    #[test]
    fn posix_quoting_is_unchanged_by_the_powershell_rules() {
        // Bare in POSIX, quoted in PowerShell.
        assert_eq!(
            command_for_exe("/Users/a,b@c/x", false, ShellFlavor::Posix),
            "/Users/a,b@c/x"
        );
        // The typographic quote is an ordinary character to a POSIX shell.
        assert_eq!(
            command_for_exe("/Users/O\u{2019}Brien/x", false, ShellFlavor::Posix),
            "'/Users/O\u{2019}Brien/x'"
        );
    }

    #[test]
    fn built_commands_are_recognised_as_ours() {
        for (exe, windows, flavor) in [
            (
                "/usr/local/bin/cooldown-statusline",
                false,
                ShellFlavor::Posix,
            ),
            (
                "/Users/o'brien/bin/cooldown-statusline",
                false,
                ShellFlavor::Posix,
            ),
            (
                r"C:\Program Files\R\cooldown-statusline.exe",
                true,
                ShellFlavor::Posix,
            ),
            (
                r"C:\Program Files\R\cooldown-statusline.exe",
                true,
                ShellFlavor::PowerShell,
            ),
            (
                r"C:\x\cooldown-statusline.exe",
                true,
                ShellFlavor::PowerShell,
            ),
        ] {
            let cmd = command_for_exe(exe, windows, flavor);
            assert!(is_our_command(&cmd), "{cmd}");
        }
    }

    #[test]
    fn recognises_hand_written_variants_of_our_command() {
        for cmd in [
            "cooldown-statusline",
            "COOLDOWN-STATUSLINE.EXE",
            "\"/opt/x y/cooldown-statusline\" hook",
            "'/opt/x y/cooldown-statusline' --flag",
            r"C:\Users\me\cooldown-statusline.exe",
            r#""C:\Program Files\R\cooldown-statusline.exe""#,
            "  & 'C:/a b/cooldown-statusline.exe'",
            "./cooldown-statusline hook",
        ] {
            assert!(is_our_command(cmd), "{cmd}");
        }
    }

    #[test]
    fn does_not_mistake_other_commands_for_ours() {
        for cmd in [
            "~/.claude/statusline.sh",
            "jq -r '.model.display_name'",
            "powershell -NoProfile -File C:/Users/me/.claude/statusline.ps1",
            "ccstatus",
            "my-cooldown-statusline-wrapper.sh",
            "/opt/cooldown-statusline-old/run.sh",
            "echo cooldown-statusline",
            "",
            "   ",
        ] {
            assert!(!is_our_command(cmd), "{cmd}");
        }
    }

    #[test]
    fn default_flavor_depends_on_git_bash_only_on_windows() {
        assert_eq!(default_flavor(false, false), ShellFlavor::Posix);
        assert_eq!(default_flavor(true, true), ShellFlavor::Posix);
        assert_eq!(default_flavor(true, false), ShellFlavor::PowerShell);
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn git_bash_explicit_override_wins() {
        let env = env_of(&[(ENV_GIT_BASH, "/custom/bash.exe"), ("ProgramFiles", "/pf")]);
        let exists =
            |p: &Path| p == Path::new("/custom/bash.exe") || p == Path::new("/pf/Git/bin/bash.exe");
        assert_eq!(
            find_git_bash(&env, &exists),
            Some(PathBuf::from("/custom/bash.exe"))
        );
    }

    #[test]
    fn git_bash_found_from_git_exe_on_path() {
        // join_paths uses the platform's separator, so this holds on Windows too.
        let path = std::env::join_paths(["/nowhere", "/tools/Git/cmd"]).unwrap();
        let path = path.to_string_lossy().into_owned();
        let env = env_of(&[("PATH", path.as_str())]);
        let exists = |p: &Path| {
            p == Path::new("/tools/Git/cmd/git.exe") || p == Path::new("/tools/Git/bin/bash.exe")
        };
        assert_eq!(
            find_git_bash(&env, &exists),
            Some(PathBuf::from("/tools/Git/bin/bash.exe"))
        );
    }

    #[test]
    fn git_bash_found_in_standard_folders() {
        let env = env_of(&[("ProgramFiles", "/pf"), ("LOCALAPPDATA", "/local")]);
        let in_pf = |p: &Path| p == Path::new("/pf/Git/bin/bash.exe");
        assert_eq!(
            find_git_bash(&env, &in_pf),
            Some(PathBuf::from("/pf/Git/bin/bash.exe"))
        );
        let in_local = |p: &Path| p == Path::new("/local/Programs/Git/bin/bash.exe");
        assert_eq!(
            find_git_bash(&env, &in_local),
            Some(PathBuf::from("/local/Programs/Git/bin/bash.exe"))
        );
    }

    #[test]
    fn git_bash_absent_means_none() {
        let env = env_of(&[("ProgramFiles", "/pf"), ("PATH", "/bin")]);
        assert_eq!(find_git_bash(&env, &|_| false), None);
        // A stale override that does not exist is ignored, not trusted.
        let env = env_of(&[(ENV_GIT_BASH, "/gone/bash.exe")]);
        assert_eq!(find_git_bash(&env, &|_| false), None);
    }
}
