//! XDG path resolution (requirements §7, architecture §8).
//!
//! All engine path decisions flow through [`Paths`] — overridable wholesale
//! for tests so no engine code constructs XDG locations by hand.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Resolved engine paths.
// clippy::struct_field_names: the shared suffix IS the concept (three XDG
/// bases); renaming to satisfy the lint would obscure it.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    config_base: PathBuf,
    cache_base: PathBuf,
    data_base: PathBuf,
}

impl Paths {
    /// Builds paths from the XDG environment
    /// (`XDG_CONFIG_HOME`/`XDG_CACHE_HOME`/`XDG_DATA_HOME` with `~/.config`
    /// etc. as defaults), each suffixed with `kestrel/`.
    ///
    /// # Errors
    /// Fails when a set env var is empty or non-absolute (XDG spec
    /// violation) — fail fast rather than silently writing elsewhere.
    pub fn from_xdg() -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "$HOME is not set; cannot resolve XDG defaults".to_string())?;
        let config_root = xdg_dir("XDG_CONFIG_HOME", home.join(".config"))?;
        let cache_root = xdg_dir("XDG_CACHE_HOME", home.join(".cache"))?;
        let data_root = xdg_dir("XDG_DATA_HOME", home.join(".local/share"))?;
        Ok(Self {
            config_base: config_root.join("kestrel"),
            cache_base: cache_root.join("kestrel"),
            data_base: data_root.join("kestrel"),
        })
    }

    /// Test/isolation override: all roots under `root/{config,cache,data}`.
    #[must_use]
    pub fn nested_under(root: &Path) -> Self {
        Self {
            config_base: root.join("config"),
            cache_base: root.join("cache"),
            data_base: root.join("data"),
        }
    }

    /// `$XDG_CONFIG_HOME/kestrel/config.toml`
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_base.join("config.toml")
    }

    /// `$XDG_CACHE_HOME/kestrel/cache.db`
    #[must_use]
    pub fn cache_db(&self) -> PathBuf {
        self.cache_base.join("cache.db")
    }

    /// `$XDG_DATA_HOME/kestrel/data.db`
    #[must_use]
    pub fn data_db(&self) -> PathBuf {
        self.data_base.join("data.db")
    }

    /// `$XDG_DATA_HOME/kestrel/blobs/`
    #[must_use]
    pub fn blob_root(&self) -> PathBuf {
        self.data_base.join("blobs")
    }

    /// `$XDG_DATA_HOME/kestrel/blobs/tmp/`
    #[must_use]
    pub fn blob_tmp(&self) -> PathBuf {
        self.data_base.join("blobs").join("tmp")
    }

    /// `$XDG_DATA_HOME/kestrel/index/`
    #[must_use]
    pub fn index_dir(&self) -> PathBuf {
        self.data_base.join("index")
    }

    /// Creates every required directory with 0700 permissions
    /// (threat model §4.3). Idempotent.
    ///
    /// # Errors
    /// Propagates filesystem errors.
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            &self.config_base,
            &self.cache_base,
            &self.data_base,
            &self.blob_tmp(),
        ] {
            fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }
}

fn xdg_dir(var: &str, default: PathBuf) -> Result<PathBuf, String> {
    match std::env::var_os(var) {
        None => Ok(default),
        Some(v) if v.is_empty() => Err(format!("${var} is set but empty")),
        Some(v) => {
            let p = PathBuf::from(v);
            if p.is_absolute() {
                Ok(p)
            } else {
                Err(format!("${var} must be absolute: {}", p.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, unsafe_code)]

    use super::*;

    #[test]
    fn nested_override_layout() {
        let p = Paths::nested_under(Path::new("/tmp/kestrel-test"));
        assert_eq!(
            p.config_file(),
            Path::new("/tmp/kestrel-test/config/config.toml")
        );
        assert_eq!(p.cache_db(), Path::new("/tmp/kestrel-test/cache/cache.db"));
        assert_eq!(p.data_db(), Path::new("/tmp/kestrel-test/data/data.db"));
        assert_eq!(p.blob_root(), Path::new("/tmp/kestrel-test/data/blobs"));
        assert_eq!(p.index_dir(), Path::new("/tmp/kestrel-test/data/index"));
        assert_eq!(p.blob_tmp(), Path::new("/tmp/kestrel-test/data/blobs/tmp"));
    }

    #[test]
    fn xdg_env_is_honored_and_validated() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY(test): nextest runs each test in its own process; no
        // concurrent env access. Vars removed before assertions complete.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
        assert!(Paths::from_xdg().is_ok());
        // Relative override rejected (fail fast).
        unsafe { std::env::set_var("XDG_CACHE_HOME", "relative/path") };
        assert!(Paths::from_xdg().is_err());
        unsafe { std::env::remove_var("XDG_CACHE_HOME") };
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
}
