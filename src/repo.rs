use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum::http::Uri;
use axum::{body::Bytes, http::HeaderValue};
use tokio::fs;
use tokio::sync::Mutex;

use crate::error::{Error, Result};

#[cfg(not(test))]
use crate::git::{Git, GitAsyncRead};
#[cfg(test)]
use crate::git::{GitAsyncRead, MockGit as Git};

#[derive(Debug)]
pub struct Index {
    git: Arc<Git>,
    index: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<Repo>>>>>,
    cache_dir: PathBuf,
}

impl Index {
    pub fn new(cache_dir: PathBuf, git: Git) -> Self {
        Self {
            git: Arc::new(git),
            index: Default::default(),
            cache_dir,
        }
    }

    pub async fn open(&self, upstream: Uri) -> Result<Arc<Mutex<Repo>>> {
        let host = Path::new(upstream.host().ok_or(Error::NotFound)?);
        let path = Path::new(&upstream.path()[1..]);

        // Guard against path traversal attacks, as well as any other "strange" path components
        // that may cause issues.
        let mut local = self.cache_dir.clone();
        for part in [host, path] {
            for comp in part.components() {
                match comp {
                    Component::Normal(c) => local.push(c),
                    comp => {
                        tracing::warn!(?host, ?path, "disallowed component present: {comp:?}");
                        return Err(Error::NotFound);
                    }
                };
            }
        }
        local.set_extension("git");

        let mut index = self.index.lock().await;

        match index.entry(local.clone()) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                fs::create_dir_all(&local)
                    .await
                    .context("failed to create directory for repository")?;

                self.git.init(local.clone()).await?;

                let repo = Arc::new(Mutex::new(Repo {
                    git: self.git.clone(),
                    upstream: upstream.clone(),
                    local,
                }));

                e.insert(repo.clone());

                Ok(repo)
            }
        }
    }
}

#[derive(Debug)]
pub struct Repo {
    git: Arc<Git>,
    upstream: Uri,
    local: PathBuf,
}

impl Repo {
    pub async fn authenticate_with_head(
        &self,
        auth: Option<HeaderValue>,
    ) -> Result<Option<String>> {
        // Assume we (the server) has a modern git that supports symrefs.
        self.git
            .authenticate_with_head(self.upstream.clone(), auth)
            .await
    }

    pub async fn fetch(
        &mut self,
        remote_head: Option<String>,
        auth: Option<HeaderValue>,
    ) -> Result<()> {
        if let Some(remote_head) = remote_head {
            tokio::fs::write(self.local.join("HEAD"), format!("ref: {remote_head}"))
                .await
                .context("failed to update HEAD")?;
        }

        self.git
            .fetch(self.upstream.clone(), self.local.clone(), auth)
            .await
    }

    pub fn advertise_refs(&self) -> Result<GitAsyncRead> {
        self.git.advertise_refs(self.local.clone())
    }

    pub async fn upload_pack(&self, input: Bytes) -> Result<GitAsyncRead> {
        self.git.upload_pack(self.local.clone(), input).await
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn path_sanitization() {
        let cache_dir = tempdir().unwrap().into_path();

        let mut mock_git = Git::default();
        mock_git.expect_init().returning(|_| Ok(()));

        let index = Index::new(cache_dir, mock_git);

        assert!(index
            .open(Uri::from_static("https://example.com//a/b"))
            .await
            .is_err());

        assert!(index
            .open(Uri::from_static("https://example.com/../a/b"))
            .await
            .is_err());

        assert!(index
            .open(Uri::from_static("https://example.com/a/../b"))
            .await
            .is_err());

        assert!(index
            .open(Uri::from_static("https://example.com/./a/b.git"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn mutual_exclusion() {
        let cache_dir = tempdir().unwrap().into_path();

        let mut mock_git = Git::default();
        mock_git.expect_init().times(2).returning(|_| Ok(()));

        let index = Index::new(cache_dir, mock_git);

        let a = index
            .open("https://example.com/a/b/c".parse().unwrap())
            .await
            .unwrap();
        let b = index
            .open("https://example.com/a/b/c.git".parse().unwrap())
            .await
            .unwrap();
        let c = index
            .open("https://example.com/X/Y/Z.git".parse().unwrap())
            .await
            .unwrap();

        let lock_a = a.lock().await;
        assert!(b.try_lock().is_err());
        assert!(c.try_lock().is_ok());
        drop(lock_a);
    }
}
