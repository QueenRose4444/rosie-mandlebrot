//! Where default files go.
//!
//! Defaults are not relative to the working directory, which a terminal
//! controls and a double-click does not. One binary decides at startup:
//!
//! * **portable** — `--portable`, `MANDELBROT_PORTABLE=1`, or a `portable.txt`
//!   beside the executable: everything lives beside the executable, so a
//!   folder on a USB stick carries its runs with it;
//! * **installed** — otherwise: the user's data directory.
//!
//! The release zip ships `portable.txt`, so an installed copy and a portable
//! one are the same binary. Paths given on the command line are untouched.

use std::path::{Path, PathBuf};

pub const PORTABLE_SENTINEL: &str = "portable.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Portable { root: PathBuf },
    Installed { root: PathBuf },
}

impl Layout {
    /// Decide once, at startup.
    pub fn detect(portable_flag: bool) -> Layout {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let env_flag = std::env::var("MANDELBROT_PORTABLE").is_ok_and(|v| v == "1");
        Self::decide(portable_flag || env_flag, exe_dir, data_dir())
    }

    /// The decision itself, separate from the environment so it can be tested.
    pub fn decide(explicit: bool, exe_dir: Option<PathBuf>, data_dir: Option<PathBuf>) -> Layout {
        let sentinel = exe_dir.as_ref().is_some_and(|d| d.join(PORTABLE_SENTINEL).is_file());
        match (explicit || sentinel, exe_dir, data_dir) {
            (true, Some(dir), _) => Layout::Portable { root: dir },
            (_, _, Some(data)) => Layout::Installed { root: data },
            (_, Some(dir), None) => Layout::Portable { root: dir },
            (_, None, None) => Layout::Portable { root: PathBuf::from(".") },
        }
    }

    pub fn root(&self) -> &Path {
        match self {
            Layout::Portable { root } | Layout::Installed { root } => root,
        }
    }

    pub fn output(&self) -> PathBuf {
        self.root().join("out")
    }

    pub fn state(&self) -> PathBuf {
        self.root().join("state").join("state.db")
    }

    pub fn logs(&self) -> PathBuf {
        self.root().join("logs")
    }

    pub fn name(&self) -> &'static str {
        match self {
            Layout::Portable { .. } => "portable",
            Layout::Installed { .. } => "installed",
        }
    }
}

/// The per-user data directory for this app, from the environment alone.
fn data_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("LOCALAPPDATA").map(|d| d.join("Rosie_Mandlebrot"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/Rosie_Mandlebrot"))
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".local/share")))
            .map(|d| d.join("rosie-mandlebrot"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mb-layout-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_sentinel_beside_the_exe_makes_it_portable() {
        let exe = tmp("sentinel");
        std::fs::write(exe.join(PORTABLE_SENTINEL), "").unwrap();
        let l = Layout::decide(false, Some(exe.clone()), Some(PathBuf::from("/data")));
        assert_eq!(l, Layout::Portable { root: exe.clone() });
        assert_eq!(l.state(), exe.join("state").join("state.db"));
    }

    #[test]
    fn without_one_it_uses_the_data_directory() {
        let exe = tmp("installed");
        let l = Layout::decide(false, Some(exe), Some(PathBuf::from("/data")));
        assert_eq!(l, Layout::Installed { root: PathBuf::from("/data") });
        assert_eq!(l.output(), PathBuf::from("/data").join("out"));
    }

    #[test]
    fn the_flag_wins_and_no_data_dir_falls_back_to_the_exe() {
        let exe = tmp("flag");
        assert_eq!(Layout::decide(true, Some(exe.clone()), Some("/data".into())), Layout::Portable { root: exe.clone() });
        assert_eq!(Layout::decide(false, Some(exe.clone()), None), Layout::Portable { root: exe });
    }
}
