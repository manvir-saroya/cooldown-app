//! Finding and starting the `codex` command line, on macOS, Linux and especially Windows.
//!
//! Ported in idea from the MIT-licensed `burnrate` (`src/providers/mod.rs`: `resolve_cli`,
//! `strip_credential_env`) and, for Windows launcher resolution, from `Claude-Code-Usage-Monitor`
//! (see `THIRD_PARTY_NOTICES.md`).
//!
//! - [`resolve_cli`] is a pure function: it takes the `PATH` directories, the `PATHEXT` list and a
//!   file-exists predicate as arguments, so every platform's rules are unit-tested on any machine.
//! - On Windows it tries `codex.cmd`, `codex.ps1`, then `codex.exe` in every `PATH` directory, then
//!   asks `where.exe codex` (a GUI app often has a thinner `PATH` than a terminal).
//! - A `.ps1` launcher is run through `powershell.exe -File`. A `.cmd` shim is started directly:
//!   Rust's standard library runs batch files through `cmd.exe` with safe argument quoting.
//! - Windows children start with `CREATE_NO_WINDOW` so no console window flashes.
//! - Credential-bearing environment variables are removed from the child's environment, so an
//!   inherited `OPENAI_API_KEY` cannot switch Codex from the user's ChatGPT login to API-key auth.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use tokio::process::Command;

/// Windows process creation flag: do not create a console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Environment variables that make the provider CLIs use a token from the environment instead of
/// the user's own login store. A spawned Codex must never see them.
pub const CREDENTIAL_ENV_OVERRIDES: [&str; 5] = [
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
];

/// Remove [`CREDENTIAL_ENV_OVERRIDES`] from a child's environment.
pub fn strip_credential_env(command: &mut Command) {
    for key in CREDENTIAL_ENV_OVERRIDES {
        command.env_remove(key);
    }
}

/// Keep a Windows child from opening a console window. A no-op elsewhere.
pub fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// A program and the arguments that must come before the real ones (`-File x.ps1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    /// The executable to start.
    pub program: PathBuf,
    /// Arguments that go before the caller's own.
    pub prefix_args: Vec<OsString>,
}

impl Launcher {
    /// Start `program` directly.
    pub fn direct(program: impl Into<PathBuf>) -> Self {
        Launcher {
            program: program.into(),
            prefix_args: Vec::new(),
        }
    }

    /// The launcher for a resolved path: `.ps1` scripts go through `powershell.exe`, everything
    /// else (including `.cmd` and `.bat`) is started directly.
    pub fn for_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if extension_is(&path, "ps1") {
            Launcher {
                program: PathBuf::from("powershell.exe"),
                prefix_args: [
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ]
                .iter()
                .map(OsString::from)
                .chain([path.into_os_string()])
                .collect(),
            }
        } else {
            Launcher::direct(path)
        }
    }

    /// A [`Command`] for this launcher with `args` appended. It has no window on Windows, the
    /// credential variables removed, and `kill_on_drop` set; the caller adds the stdio setup.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix_args).args(args);
        command.kill_on_drop(true);
        strip_credential_env(&mut command);
        hide_console_window(&mut command);
        command
    }
}

fn extension_is(path: &Path, wanted: &str) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case(wanted))
}

/// The inputs of [`resolve_cli`]: where to look and which extensions count as executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliEnv {
    /// Directories to search, in order (the `PATH` entries first, then well-known extras).
    pub path_dirs: Vec<PathBuf>,
    /// The `PATHEXT` entries, lowercase with a leading dot (Windows only).
    pub pathext: Vec<String>,
    /// Apply Windows rules (extension search, no executable bit).
    pub windows: bool,
}

impl CliEnv {
    /// Build from raw `PATH` / `PATHEXT` values plus extra directories to try last.
    pub fn new(
        path: Option<&OsStr>,
        pathext: Option<&OsStr>,
        windows: bool,
        extra_dirs: &[PathBuf],
    ) -> Self {
        let mut path_dirs: Vec<PathBuf> = Vec::new();
        let candidates = path
            .into_iter()
            .flat_map(std::env::split_paths)
            .chain(extra_dirs.iter().cloned());
        for dir in candidates {
            if !dir.as_os_str().is_empty() && !path_dirs.contains(&dir) {
                path_dirs.push(dir);
            }
        }
        let pathext = pathext
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .map(|ext| ext.trim().to_ascii_lowercase())
                    .filter(|ext| ext.len() > 1 && ext.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        CliEnv {
            path_dirs,
            pathext,
            windows,
        }
    }

    /// The environment of this process, with the usual install locations added.
    pub fn from_process() -> Self {
        CliEnv::new(
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("PATHEXT").as_deref(),
            cfg!(windows),
            &common_bin_dirs(),
        )
    }

    /// `path_dirs` joined as a `PATH` value, to hand to the child so its own helpers (a node
    /// shim looking for `node`) resolve the same way.
    pub fn joined_path(&self) -> Option<OsString> {
        std::env::join_paths(&self.path_dirs).ok()
    }
}

/// Where user-level CLI installs usually live, for apps started with a thin `PATH`.
fn common_bin_dirs() -> Vec<PathBuf> {
    let mut dirs_out: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        // npm's default global prefix, %APPDATA%\npm.
        if let Some(config) = dirs::config_dir() {
            dirs_out.push(config.join("npm"));
        }
    } else {
        if let Some(home) = dirs::home_dir() {
            for sub in [
                ".local/bin",
                ".cargo/bin",
                ".npm-global/bin",
                ".volta/bin",
                ".bun/bin",
            ] {
                dirs_out.push(home.join(sub));
            }
        }
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            dirs_out.push(PathBuf::from(dir));
        }
    }
    dirs_out
}

/// The file names to try for `name`.
///
/// Elsewhere: just `name`. On Windows, when `name` has no extension: `.cmd`, `.ps1`, `.exe`, then
/// `.bat` / `.com` if `PATHEXT` lists them. (The extensionless `codex` that npm also installs is a
/// shell script and cannot be started on Windows, so it is never a candidate.)
pub fn candidate_file_names(name: &str, env: &CliEnv) -> Vec<String> {
    if !env.windows || Path::new(name).extension().is_some() {
        return vec![name.to_owned()];
    }
    let mut extensions: Vec<&str> = vec![".cmd", ".ps1", ".exe"];
    for extra in [".bat", ".com"] {
        if env.pathext.iter().any(|ext| ext == extra) {
            extensions.push(extra);
        }
    }
    extensions
        .into_iter()
        .map(|ext| format!("{name}{ext}"))
        .collect()
}

/// Find the CLI `name` ("codex", or an explicit path) and say how to start it.
///
/// Pure: the file system is reached only through `is_file`. For a bare name, every candidate file
/// name is tried in every `env.path_dirs` directory (all directories for `codex.cmd` before any
/// for `codex.ps1`, and so on). A name containing a path separator is used as a path.
pub fn resolve_cli(name: &str, env: &CliEnv, is_file: &dyn Fn(&Path) -> bool) -> Option<Launcher> {
    let files = candidate_file_names(name, env);
    let explicit = name.contains('/') || name.contains(std::path::MAIN_SEPARATOR);
    if explicit {
        return files
            .iter()
            .map(PathBuf::from)
            .find(|path| is_file(path))
            .map(Launcher::for_path);
    }
    files
        .iter()
        .flat_map(|file| env.path_dirs.iter().map(move |dir| dir.join(file)))
        .find(|path| is_file(path))
        .map(Launcher::for_path)
}

/// Rank for choosing among `where.exe` results: lower is better, `None` is unusable.
fn where_rank(path: &Path) -> Option<u8> {
    ["cmd", "ps1", "exe", "bat", "com"]
        .iter()
        .position(|ext| extension_is(path, ext))
        .map(|rank| rank as u8)
}

/// The paths printed by `where.exe` (one per line).
pub fn parse_where_output(stdout: &str) -> Vec<PathBuf> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Choose the best launcher among `where.exe` results: `.cmd`, then `.ps1`, `.exe`, `.bat`,
/// `.com`. Extensionless results (npm's shell shim) are ignored.
pub fn pick_where_candidate(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .filter_map(|path| where_rank(path).map(|rank| (rank, path)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, path)| path.clone())
}

/// A regular file that can be run: executable bit on Unix, any file on Windows.
pub fn is_runnable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Ask `where.exe` where `name` is. Windows only; `None` elsewhere or on any failure.
#[cfg(windows)]
fn where_exe(name: &str) -> Option<Launcher> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command as StdCommand, Stdio};

    let output = StdCommand::new("where.exe")
        .arg(name)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let found = parse_where_output(&String::from_utf8_lossy(&output.stdout));
    pick_where_candidate(&found).map(Launcher::for_path)
}

#[cfg(not(windows))]
fn where_exe(_name: &str) -> Option<Launcher> {
    None
}

/// Find the Codex launcher for this machine: `configured` (a path or a command name) or `codex`.
///
/// Blocking (file-system probes and, on Windows, `where.exe`); async callers run it in
/// `spawn_blocking`. Returns the resolved launcher and the environment used to find it.
pub fn locate_blocking(configured: Option<&Path>) -> Option<(Launcher, CliEnv)> {
    let env = CliEnv::from_process();
    let name = configured
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "codex".to_owned());
    let launcher = resolve_cli(&name, &env, &is_runnable_file).or_else(|| {
        if env.windows {
            where_exe(&name)
        } else {
            None
        }
    })?;
    Some((launcher, env))
}

/// Kill `child` and everything it started, then reap it.
///
/// On Windows a `.cmd` shim is `cmd.exe`, and the real Codex is a `node` grandchild that
/// `Child::kill` would leave running. `taskkill /T /F` ends the whole tree first (fast, and a
/// failure only means the plain kill below has to do). Elsewhere this is `Child::kill`.
pub async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let program = std::env::var_os("SystemRoot")
            .map(|root| Path::new(&root).join("System32").join("taskkill.exe"))
            .unwrap_or_else(|| PathBuf::from("taskkill.exe"));
        let mut taskkill = Command::new(program);
        taskkill
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        hide_console_window(&mut taskkill);
        if let Ok(mut running) = taskkill.spawn() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), running.wait()).await;
        }
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn files(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    fn windows_env(path: &str, pathext: &str) -> CliEnv {
        CliEnv {
            path_dirs: path.split(';').map(PathBuf::from).collect(),
            pathext: pathext
                .split(';')
                .filter(|e| !e.is_empty())
                .map(|e| e.to_ascii_lowercase())
                .collect(),
            windows: true,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_tree_ends_and_reaps_the_child() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        kill_tree(&mut child).await;
        assert!(child.try_wait().unwrap().is_some(), "reaped");
    }

    /// The case the function exists for: a `.cmd` shim whose own child must not outlive it.
    #[cfg(windows)]
    #[tokio::test]
    async fn kill_tree_also_ends_the_grandchild_of_a_cmd_shim() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild.pid");
        let shim = dir.path().join("fake-codex.cmd");
        std::fs::write(
            &shim,
            format!(
                "@echo off\r\npowershell.exe -NoProfile -Command \"$PID | Out-File -Encoding ascii '{}'; Start-Sleep 60\"\r\n",
                marker.display()
            ),
        )
        .unwrap();
        let mut child = Launcher::for_path(&shim)
            .command(std::iter::empty::<&str>())
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&marker)
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the grandchild never started");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        kill_tree(&mut child).await;
        assert!(child.try_wait().unwrap().is_some(), "shim reaped");
        let alive = |pid: u32| {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive(pid) {
            assert!(
                Instant::now() < deadline,
                "grandchild {pid} outlived the shim"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn unix_env(dirs: &[&str]) -> CliEnv {
        CliEnv {
            path_dirs: dirs.iter().map(PathBuf::from).collect(),
            pathext: Vec::new(),
            windows: false,
        }
    }

    #[test]
    fn windows_prefers_cmd_then_ps1_then_exe() {
        let env = windows_env("C:/a;C:/b", ".COM;.EXE;.BAT;.CMD");
        // .exe in the first directory, .cmd in the second: the extension order wins over PATH order.
        let found =
            resolve_cli("codex", &env, &files(&["C:/a/codex.exe", "C:/b/codex.cmd"])).unwrap();
        assert_eq!(found, Launcher::direct("C:/b/codex.cmd"));
        // Without a .cmd, .ps1 beats .exe.
        let found =
            resolve_cli("codex", &env, &files(&["C:/a/codex.exe", "C:/b/codex.ps1"])).unwrap();
        // Built with join, as the resolver does: on Windows that inserts a backslash, and a .ps1 path is
        // passed to PowerShell as a string argument, so the separator is compared literally.
        assert_eq!(
            found,
            Launcher::for_path(Path::new("C:/b").join("codex.ps1"))
        );
        // Only an exe.
        let found = resolve_cli("codex", &env, &files(&["C:/a/codex.exe"])).unwrap();
        assert_eq!(found, Launcher::direct("C:/a/codex.exe"));
    }

    #[test]
    fn windows_ignores_the_extensionless_npm_shim() {
        let env = windows_env("C:/npm", ".EXE;.CMD");
        assert_eq!(resolve_cli("codex", &env, &files(&["C:/npm/codex"])), None);
    }

    #[test]
    fn windows_uses_pathext_for_bat_and_com_only_when_listed() {
        let with = windows_env("C:/a", ".COM;.EXE;.BAT;.CMD");
        assert_eq!(
            resolve_cli("codex", &with, &files(&["C:/a/codex.bat"])),
            Some(Launcher::direct("C:/a/codex.bat"))
        );
        let without = windows_env("C:/a", ".EXE;.CMD");
        assert_eq!(
            resolve_cli("codex", &without, &files(&["C:/a/codex.bat"])),
            None
        );
        assert_eq!(
            candidate_file_names("codex", &with),
            [
                "codex.cmd",
                "codex.ps1",
                "codex.exe",
                "codex.bat",
                "codex.com"
            ]
        );
    }

    #[test]
    fn windows_name_with_extension_is_taken_as_is() {
        let env = windows_env("C:/a", ".EXE;.CMD");
        assert_eq!(candidate_file_names("codex.exe", &env), ["codex.exe"]);
        assert_eq!(
            resolve_cli(
                "codex.exe",
                &env,
                &files(&["C:/a/codex.exe", "C:/a/codex.cmd"])
            ),
            Some(Launcher::direct("C:/a/codex.exe"))
        );
    }

    #[test]
    fn unix_searches_directories_in_order_for_the_bare_name() {
        let env = unix_env(&["/first", "/second"]);
        assert_eq!(
            resolve_cli("codex", &env, &files(&["/second/codex", "/first/codex"])),
            Some(Launcher::direct("/first/codex"))
        );
        assert_eq!(
            resolve_cli("codex", &env, &files(&["/second/codex"])),
            Some(Launcher::direct("/second/codex"))
        );
        assert_eq!(resolve_cli("codex", &env, &files(&[])), None);
        // No extension games off Windows.
        assert_eq!(
            resolve_cli("codex", &env, &files(&["/first/codex.cmd"])),
            None
        );
    }

    #[test]
    fn explicit_paths_are_not_searched() {
        let env = unix_env(&["/first"]);
        assert_eq!(
            resolve_cli("/opt/tools/codex", &env, &files(&["/opt/tools/codex"])),
            Some(Launcher::direct("/opt/tools/codex"))
        );
        assert_eq!(
            resolve_cli("/opt/tools/codex", &env, &files(&["/first/codex"])),
            None
        );
    }

    #[test]
    fn ps1_goes_through_powershell_and_cmd_does_not() {
        let ps1 = Launcher::for_path("C:/npm/codex.ps1");
        assert_eq!(ps1.program, PathBuf::from("powershell.exe"));
        assert_eq!(
            ps1.prefix_args.last().unwrap(),
            &OsString::from("C:/npm/codex.ps1")
        );
        assert!(ps1.prefix_args.contains(&OsString::from("-File")));
        assert!(ps1.prefix_args.contains(&OsString::from("-NoProfile")));
        assert_eq!(
            Launcher::for_path("C:/npm/codex.CMD"),
            Launcher::direct("C:/npm/codex.CMD")
        );
        assert_eq!(
            Launcher::for_path("C:/x/codex.PS1").program,
            PathBuf::from("powershell.exe")
        );
    }

    #[test]
    fn where_output_is_parsed_and_ranked() {
        let out = "C:\\Users\\me\\AppData\\Roaming\\npm\\codex\r\nC:\\Users\\me\\AppData\\Roaming\\npm\\codex.cmd\r\nC:\\tools\\codex.exe\r\n\r\n";
        let found = parse_where_output(out);
        assert_eq!(found.len(), 3);
        assert_eq!(
            pick_where_candidate(&found),
            Some(PathBuf::from(
                "C:\\Users\\me\\AppData\\Roaming\\npm\\codex.cmd"
            ))
        );
        assert_eq!(pick_where_candidate(&[PathBuf::from("C:\\x\\codex")]), None);
        assert_eq!(pick_where_candidate(&[]), None);
        assert_eq!(
            pick_where_candidate(&[
                PathBuf::from("C:\\a\\codex.exe"),
                PathBuf::from("C:\\a\\codex.ps1")
            ]),
            Some(PathBuf::from("C:\\a\\codex.ps1"))
        );
    }

    #[test]
    fn cli_env_parses_path_and_pathext() {
        let path = std::env::join_paths(["/a", "/b", "/a", ""]).unwrap();
        let env = CliEnv::new(
            Some(path.as_os_str()),
            Some(OsStr::new(".COM; .Exe ;.BAT;;bad;.")),
            true,
            &[PathBuf::from("/extra"), PathBuf::from("/b")],
        );
        assert_eq!(
            env.path_dirs,
            [
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/extra")
            ]
        );
        assert_eq!(env.pathext, [".com", ".exe", ".bat"]);
        assert!(env.joined_path().is_some());
    }

    #[test]
    fn credential_variables_are_removed_from_the_child() {
        let launcher = Launcher::direct("/bin/echo");
        let command = launcher.command(["hello"]);
        let std_command = command.as_std();
        for key in CREDENTIAL_ENV_OVERRIDES {
            let entry = std_command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key));
            assert_eq!(entry, Some((OsStr::new(key), None)), "{key} must be unset");
        }
    }

    #[test]
    fn prefix_args_come_before_the_caller_args() {
        let launcher = Launcher::for_path("C:/npm/codex.ps1");
        let command = launcher.command(["app-server", "--listen", "stdio://"]);
        let args: Vec<_> = command.as_std().get_args().collect();
        let tail: Vec<_> = args.iter().rev().take(3).rev().collect();
        assert_eq!(
            tail,
            [
                &OsStr::new("app-server"),
                &OsStr::new("--listen"),
                &OsStr::new("stdio://")
            ]
        );
        assert_eq!(args.iter().position(|a| *a == OsStr::new("-File")), Some(5));
    }

    #[test]
    fn locate_finds_nothing_for_a_missing_command() {
        assert!(locate_blocking(Some(Path::new("definitely-not-a-real-command-xyz"))).is_none());
    }
}
