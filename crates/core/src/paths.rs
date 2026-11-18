//! Where Dubplate keeps its files, following each platform's conventions.
//!
//! | | config | data (models, library, runtime) | cache |
//! |---|---|---|---|
//! | Linux | `$XDG_CONFIG_HOME` or `~/.config` + `/dubplate` | `$XDG_DATA_HOME` or `~/.local/share` + `/dubplate` | `$XDG_CACHE_HOME` or `~/.cache` + `/dubplate` |
//! | Windows | `%APPDATA%\Dubplate` | `%LOCALAPPDATA%\Dubplate` | `%LOCALAPPDATA%\Dubplate\cache` |
//! | macOS | `~/Library/Application Support/Dubplate` | same | `~/Library/Caches/Dubplate` |
//!
//! Linux keeps the folder names earlier versions used, so nothing moves for existing installs.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

/// An absolute path from an environment variable, or None (XDG says to ignore relative ones).
fn absolute(env: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<PathBuf> {
    env(key).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// Resolves the folders for `os` from an environment lookup. Separate from [`dirs`] so every platform's
/// rules can be tested anywhere.
pub fn resolve(os: Os, env: &dyn Fn(&str) -> Option<String>) -> Dirs {
    let home = || absolute(env, "HOME").or_else(|| absolute(env, "USERPROFILE")).unwrap_or_else(|| PathBuf::from("."));
    match os {
        Os::Linux => {
            let under = |var: &str, fallback: &str| {
                absolute(env, var).unwrap_or_else(|| home().join(fallback)).join("dubplate")
            };
            Dirs {
                config: under("XDG_CONFIG_HOME", ".config"),
                data: under("XDG_DATA_HOME", ".local/share"),
                cache: under("XDG_CACHE_HOME", ".cache"),
            }
        }
        Os::Windows => {
            let roaming = absolute(env, "APPDATA").unwrap_or_else(|| home().join("AppData/Roaming"));
            let local = absolute(env, "LOCALAPPDATA").unwrap_or_else(|| home().join("AppData/Local"));
            Dirs {
                config: roaming.join("Dubplate"),
                data: local.join("Dubplate"),
                cache: local.join("Dubplate").join("cache"),
            }
        }
        Os::MacOs => {
            let support = home().join("Library/Application Support/Dubplate");
            Dirs { config: support.clone(), data: support, cache: home().join("Library/Caches/Dubplate") }
        }
    }
}

/// This machine's folders.
pub fn dirs() -> Dirs {
    resolve(Os::current(), &|k| std::env::var(k).ok())
}

pub fn config_dir() -> PathBuf {
    dirs().config
}
pub fn data_dir() -> PathBuf {
    dirs().data
}
pub fn cache_dir() -> PathBuf {
    dirs().cache
}

/// The user's home folder, or "." if the environment doesn't say.
pub fn home_dir() -> PathBuf {
    let env = |k: &str| std::env::var(k).ok();
    absolute(&env, "HOME").or_else(|| absolute(&env, "USERPROFILE")).unwrap_or_else(|| PathBuf::from("."))
}

/// Shared library name for ONNX Runtime on this platform.
pub fn onnx_library_name() -> &'static str {
    match Os::current() {
        Os::Windows => "onnxruntime.dll",
        Os::MacOs => "libonnxruntime.dylib",
        Os::Linux => "libonnxruntime.so",
    }
}

/// Folders to look in for helper programs (yt-dlp, ffmpeg, lame): our own `tools` folder first, then
/// PATH, then the places package managers put things. A program started from Finder or a launcher gets a
/// bare PATH that leaves out Homebrew, so those places are listed explicitly.
pub fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![data_dir().join("tools")];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(d));
    }
    dirs.push(home_dir().join(".local/bin"));
    dirs.push(home_dir().join("bin"));
    if Os::current() == Os::Windows {
        // Where winget, Chocolatey and Scoop put command line programs.
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft").join("WinGet").join("Links"));
        }
        dirs.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
        dirs.push(home_dir().join("scoop").join("shims"));
    }
    dirs
}

/// A command to run, which on Windows doesn't flash a console window up (the programs we start are
/// command line tools, and we are not).
pub fn command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    } // CREATE_NO_WINDOW
    c
}

/// The program called `name` in the first of `dirs` that has it, else just `name` (so the system gets to
/// say it can't find it).
pub fn find_in(name: &str, dirs: &[PathBuf]) -> PathBuf {
    let file =
        if Os::current() == Os::Windows && !name.ends_with(".exe") { format!("{name}.exe") } else { name.to_owned() };
    dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(name))
}

/// A helper program to run: `Command::new(paths::tool("ffmpeg"))`.
pub fn tool(name: &str) -> PathBuf {
    find_in(name, &tool_dirs())
}

/// True when the helper program is installed somewhere we look.
pub fn has_tool(name: &str) -> bool {
    tool(name).is_absolute()
}

/// A PATH value with those folders on it, for programs we start that start others.
pub fn tool_path_env() -> std::ffi::OsString {
    let mut seen = Vec::new();
    for d in tool_dirs() {
        if d.is_dir() && !seen.contains(&d) {
            seen.push(d);
        }
    }
    std::env::join_paths(seen).unwrap_or_default()
}

/// True when `p` is inside `dir` (without touching the disk).
pub fn is_inside(p: &Path, dir: &Path) -> bool {
    p.starts_with(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn helper_programs_are_found_in_the_first_folder_that_has_them() {
        let base = std::env::temp_dir().join(format!("dubplate-tools-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let file = if Os::current() == Os::Windows { "yt-dlp.exe" } else { "yt-dlp" };
        std::fs::write(b.join(file), b"x").unwrap();
        assert_eq!(find_in("yt-dlp", &[a.clone(), b.clone()]), b.join(file));
        std::fs::write(a.join(file), b"x").unwrap();
        assert_eq!(find_in("yt-dlp", &[a.clone(), b.clone()]), a.join(file), "the earlier folder wins");
        assert_eq!(find_in("nothing-here", &[a, b]), PathBuf::from("nothing-here"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn our_own_tools_folder_comes_first_and_homebrew_is_always_looked_in() {
        let d = tool_dirs();
        assert!(d[0].ends_with("tools"));
        assert!(d.iter().any(|p| p == Path::new("/opt/homebrew/bin")));
    }

    #[test]
    fn linux_keeps_the_old_locations() {
        let d = resolve(Os::Linux, &env(&[("HOME", "/home/dj")]));
        assert_eq!(d.config, Path::new("/home/dj/.config/dubplate"));
        assert_eq!(d.data, Path::new("/home/dj/.local/share/dubplate"));
        assert_eq!(d.cache, Path::new("/home/dj/.cache/dubplate"));
    }

    #[test]
    fn linux_honours_xdg_but_not_relative_values() {
        let d = resolve(
            Os::Linux,
            &env(&[("HOME", "/home/dj"), ("XDG_DATA_HOME", "/data"), ("XDG_CACHE_HOME", "relative/cache")]),
        );
        assert_eq!(d.data, Path::new("/data/dubplate"));
        assert_eq!(d.cache, Path::new("/home/dj/.cache/dubplate"));
    }

    #[test]
    fn windows_uses_appdata() {
        let d = resolve(
            Os::Windows,
            &env(&[("APPDATA", "/c/Users/dj/AppData/Roaming"), ("LOCALAPPDATA", "/c/Users/dj/AppData/Local")]),
        );
        assert_eq!(d.config, Path::new("/c/Users/dj/AppData/Roaming/Dubplate"));
        assert_eq!(d.data, Path::new("/c/Users/dj/AppData/Local/Dubplate"));
        assert_eq!(d.cache, Path::new("/c/Users/dj/AppData/Local/Dubplate/cache"));
        // Without them, fall back to the profile folder.
        let d = resolve(Os::Windows, &env(&[("USERPROFILE", "/c/Users/dj")]));
        assert_eq!(d.config, Path::new("/c/Users/dj/AppData/Roaming/Dubplate"));
    }

    #[test]
    fn macos_uses_library() {
        let d = resolve(Os::MacOs, &env(&[("HOME", "/Users/dj")]));
        assert_eq!(d.config, Path::new("/Users/dj/Library/Application Support/Dubplate"));
        assert_eq!(d.data, d.config);
        assert_eq!(d.cache, Path::new("/Users/dj/Library/Caches/Dubplate"));
    }

    #[test]
    fn nothing_panics_without_an_environment() {
        for os in [Os::Linux, Os::Windows, Os::MacOs] {
            let d = resolve(os, &env(&[]));
            assert!(d.config.ends_with("dubplate") || d.config.ends_with("Dubplate"));
        }
    }
}
