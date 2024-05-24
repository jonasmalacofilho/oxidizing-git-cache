use std::io::Result;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncRead;
use tokio::process::{ChildStdout, Command};

// FIXME: duplicates functionality in crate::Repos;
pub struct Repo<G = GitCli> {
    upstream: PathBuf,
    local: PathBuf,
    _phantom: PhantomData<G>,
}

impl<G> Repo<G> {
    pub fn new(upstream: PathBuf, local: PathBuf) -> Self {
        Self {
            upstream,
            local,
            _phantom: PhantomData,
        }
    }
}

impl<G: GitOps> Repo<G> {
    pub fn advertise_refs(&mut self) -> Result<impl AsyncRead + Send + Sync + 'static> {
        G::advertise_refs(self.local.clone())
    }
}

pub trait GitOps: Send + Sync + 'static {
    // TODO: take a ref to local.
    fn advertise_refs(local: PathBuf) -> Result<impl AsyncRead + Send + Sync + 'static>;
}

pub struct GitCli {}

impl GitOps for GitCli {
    fn advertise_refs(local: PathBuf) -> Result<ChildStdout> {
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg("--advertise-refs")
            .arg(local) // FIXME bytes or path?
            .stdout(Stdio::piped())
            .spawn()?;
        Ok(child.stdout.take().unwrap()) // FIXME (or not) unwrap
    }
}

pub struct GitMock {}

impl GitOps for GitMock {
    fn advertise_refs(_local: PathBuf) -> Result<impl AsyncRead + Send + Sync + 'static> {
        Ok("mock git-upload-pack output".as_bytes())
    }
}
