use std::io::Result;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncRead;
use tokio::process::Command;

#[cfg(test)]
use mockall::automock;

#[derive(Default, Clone)]
pub struct Git {}

#[cfg_attr(test, automock)]
impl Git {
    pub fn advertise_refs(
        &self,
        local: PathBuf,
    ) -> Result<Box<dyn AsyncRead + Send + Sync + 'static>> {
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg("--advertise-refs")
            .arg(local) // FIXME bytes or path?
            .stdout(Stdio::piped())
            .spawn()?;
        Ok(Box::new(child.stdout.take().unwrap())) // FIXME (or not) unwrap
    }
}
