//! File-backed [`TestPondMark`]: `<data>/setup/test-pond`, beside the admin card, where no route
//! reads or writes and My Pond's file list never shows it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use pond_core::security::ports::test_pond::TestPondMark;

use crate::file_setup_code_store::SETUP_DIR;

pub const TEST_POND_FILE: &str = "test-pond";

const MARK_TEXT: &[u8] =
    b"This is a Jarida test Pond: its debug switch may show household content in the logs.\n";

pub struct FileTestPondMark {
    dir: PathBuf,
    path: PathBuf,
}

impl FileTestPondMark {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let dir = data_dir.into().join(SETUP_DIR);
        let path = dir.join(TEST_POND_FILE);
        Self { dir, path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A link is never a mark: anyone able to plant one could otherwise point it at any file.
    pub fn is_marked(&self) -> Result<bool> {
        match fs::symlink_metadata(&self.path) {
            Ok(meta) => Ok(meta.file_type().is_file()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    pub fn set(&self, on: bool) -> Result<()> {
        if !on {
            return match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).with_context(|| format!("removing {}", self.path.display())),
            };
        }
        if self.is_marked()? {
            return Ok(());
        }
        self.ensure_dir()?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&self.path)
            .and_then(|mut file| {
                file.write_all(MARK_TEXT)?;
                file.sync_all()
            })
            .with_context(|| format!("writing {}", self.path.display()))
    }

    fn ensure_dir(&self) -> Result<()> {
        match fs::symlink_metadata(&self.dir) {
            Ok(meta) if meta.file_type().is_dir() => return Ok(()),
            Ok(_) => bail!("{} is not a directory", self.dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))
    }
}

#[async_trait]
impl TestPondMark for FileTestPondMark {
    async fn is_test_pond(&self) -> Result<bool> {
        self.is_marked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_pond_is_a_home_pond_until_the_host_marks_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mark = FileTestPondMark::new(tmp.path());
        assert!(!mark.is_test_pond().await.unwrap());

        mark.set(true).unwrap();
        assert!(mark.is_test_pond().await.unwrap());
        mark.set(true).unwrap();
        assert!(mark.is_test_pond().await.unwrap());

        mark.set(false).unwrap();
        assert!(!mark.is_test_pond().await.unwrap());
        mark.set(false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_mark_is_readable_only_by_the_ponds_account() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mark = FileTestPondMark::new(tmp.path());
        mark.set(true).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            (mode(mark.path()), mode(&tmp.path().join(SETUP_DIR))),
            (0o600, 0o700)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_marks_place_is_not_a_mark() {
        let tmp = tempfile::tempdir().unwrap();
        let mark = FileTestPondMark::new(tmp.path());
        fs::create_dir_all(tmp.path().join(SETUP_DIR)).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::write(&elsewhere, "x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, mark.path()).unwrap();

        assert!(!mark.is_marked().unwrap());
    }
}
