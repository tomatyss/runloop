use std::any::Any;

use async_trait::async_trait;
use wasi_common::dir::{OpenResult as WasiOpenResult, ReaddirCursor, ReaddirEntity, WasiDir};
use wasi_common::file::{FdFlags, Filestat, OFlags};
use wasi_common::{Error, ErrorExt, SystemTimeSpec};

pub struct CapabilityDir {
    inner: Box<dyn WasiDir>,
    allow_write: bool,
}

impl CapabilityDir {
    pub fn read_only(inner: Box<dyn WasiDir>) -> Self {
        Self {
            inner,
            allow_write: false,
        }
    }

    pub fn read_write(inner: Box<dyn WasiDir>) -> Self {
        Self {
            inner,
            allow_write: true,
        }
    }

    fn deny(&self) -> Error {
        Error::perm().context("fs capability denied")
    }
}

#[async_trait]
impl WasiDir for CapabilityDir {
    fn as_any(&self) -> &dyn Any {
        self.inner.as_any()
    }

    async fn open_file(
        &self,
        follow_symlinks: bool,
        path: &str,
        oflags: OFlags,
        read: bool,
        write: bool,
        fdflags: FdFlags,
    ) -> Result<WasiOpenResult, Error> {
        if !self.allow_write && (write || oflags.contains(OFlags::CREATE | OFlags::TRUNCATE)) {
            return Err(self.deny());
        }
        if std::path::Path::new(path)
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(self.deny());
        }
        self.inner
            .open_file(follow_symlinks, path, oflags, read, write, fdflags)
            .await
    }

    async fn create_dir(&self, path: &str) -> Result<(), Error> {
        if !self.allow_write {
            return Err(self.deny());
        }
        self.inner.create_dir(path).await
    }

    async fn readdir(
        &self,
        cursor: ReaddirCursor,
    ) -> Result<Box<dyn Iterator<Item = Result<ReaddirEntity, Error>> + Send>, Error> {
        self.inner.readdir(cursor).await
    }

    async fn symlink(&self, old_path: &str, new_path: &str) -> Result<(), Error> {
        if !self.allow_write {
            return Err(self.deny());
        }
        self.inner.symlink(old_path, new_path).await
    }

    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        if !self.allow_write {
            return Err(self.deny());
        }
        self.inner.remove_dir(path).await
    }

    async fn unlink_file(&self, path: &str) -> Result<(), Error> {
        if !self.allow_write {
            return Err(self.deny());
        }
        self.inner.unlink_file(path).await
    }

    async fn read_link(&self, path: &str) -> Result<std::path::PathBuf, Error> {
        self.inner.read_link(path).await
    }

    async fn get_filestat(&self) -> Result<Filestat, Error> {
        self.inner.get_filestat().await
    }

    async fn get_path_filestat(
        &self,
        path: &str,
        follow_symlinks: bool,
    ) -> Result<Filestat, Error> {
        self.inner.get_path_filestat(path, follow_symlinks).await
    }

    async fn rename(
        &self,
        path: &str,
        dest_dir: &dyn WasiDir,
        dest_path: &str,
    ) -> Result<(), Error> {
        if !self.allow_write {
            return Err(self.deny());
        }
        self.inner.rename(path, dest_dir, dest_path).await
    }

    async fn hard_link(
        &self,
        path: &str,
        target_dir: &dyn WasiDir,
        target_path: &str,
    ) -> Result<(), Error> {
        if !self.allow_write {
            return Err(self.deny());
        }
        self.inner.hard_link(path, target_dir, target_path).await
    }

    async fn set_times(
        &self,
        path: &str,
        atime: Option<SystemTimeSpec>,
        mtime: Option<SystemTimeSpec>,
        follow_symlinks: bool,
    ) -> Result<(), Error> {
        if !self.allow_write {
            return Err(self.deny());
        }
        self.inner
            .set_times(path, atime, mtime, follow_symlinks)
            .await
    }
}
