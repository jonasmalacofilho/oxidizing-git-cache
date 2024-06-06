use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::{collections::hash_map::Entry, path::Path};

use anyhow::Context;
use axum::body::Bytes;
use axum::http::Uri;
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
        // FIXME: upstream must be sanitized:
        // - upstreams that escape out of the cache-dir
        // - upstreams that turn into subpaths of existing cached repos
        // - upstreams that result in absolute-looking paths being feed into Path::join

        let local = self
            .cache_dir
            .join(upstream.host().ok_or(Error::NotFound)?)
            .join(&upstream.path()[1..])
            .with_extension("git");

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
    pub async fn fetch(&mut self) -> Result<()> {
        // Assume we (the server) has a modern git that supports symrefs.
        let remote_head = self.git.remote_head(self.upstream.clone()).await?;
        tokio::fs::write(self.local.join("HEAD"), remote_head)
            .await
            .context("failed to update HEAD")?;

        self.git
            .fetch(self.upstream.clone(), self.local.clone())
            .await
    }

    pub fn advertise_refs(&mut self) -> Result<GitAsyncRead> {
        self.git.advertise_refs(self.local.clone())
    }

    pub async fn upload_pack(&mut self, input: Bytes) -> Result<GitAsyncRead> {
        self.git.upload_pack(self.local.clone(), input).await
    }
}
