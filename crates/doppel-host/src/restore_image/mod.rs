//! Downloading a macOS restore image with `curl`, resuming an interrupted
//! download. What to download and where is [`plan`]'s.

pub mod plan;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use nix::fcntl::{Flock, FlockArg};
use plan::{Downloaded, LatestImage};

const CURL: &str = "/usr/bin/curl";
/// How often progress is reported.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("another download into {0} is running; wait for it to end, or stop it, and try again")]
    Busy(PathBuf),
    #[error("can't run curl: {0}")]
    Spawn(#[source] io::Error),
    #[error("downloading {url} failed ({status}): {why}")]
    Failed {
        url: String,
        status: String,
        why: String,
    },
}

impl LatestImage {
    /// Downloads the image into `dir`, resuming a partial download left
    /// there, and returns the complete file. One already complete is
    /// returned as it is. Reports progress every few seconds.
    ///
    /// Interrupted, it leaves [`LatestImage::partial`], which the next
    /// call resumes.
    ///
    /// # Errors
    /// [`DownloadError`] when `dir` can't be written or curl fails.
    pub fn download(
        &self,
        dir: &Path,
        mut progress: impl FnMut(Downloaded),
    ) -> Result<PathBuf, DownloadError> {
        let file = self.file(dir);
        if file.is_file() {
            return Ok(file);
        }
        fs::create_dir_all(dir).map_err(|source| DownloadError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let partial = self.partial(dir);
        let lock_path = self.lock(dir);
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| DownloadError::Io { path, source }
        };
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(io(&lock_path))?;
        let lock = Flock::lock(lock, FlockArg::LockExclusiveNonblock)
            .map_err(|_| DownloadError::Busy(partial.clone()))?;
        // curl's stdin shares the lock: it stays held while curl runs, even
        // if this process is killed and curl outlives it.
        let held = lock.try_clone().map_err(io(&lock_path))?;
        let total = self.size();
        let mut child = Command::new(CURL)
            .args(["--fail", "--location", "--silent", "--show-error"])
            .args(["--retry", "5", "--continue-at", "-", "--output"])
            .arg(&partial)
            .arg(self.url.as_str())
            .stdin(Stdio::from(held))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(DownloadError::Spawn)?;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(DownloadError::Spawn)? {
                break status;
            }
            if let Ok(meta) = fs::metadata(&partial) {
                progress(Downloaded {
                    bytes: meta.len(),
                    of: total,
                });
            }
            std::thread::sleep(PROGRESS_EVERY);
        };
        if !status.success() {
            let mut why = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                // What curl said is the reason; an unreadable pipe leaves it empty.
                let _ = io::Read::read_to_string(&mut stderr, &mut why);
            }
            return Err(DownloadError::Failed {
                url: self.url.to_string(),
                status: status.to_string(),
                why: why.trim().to_owned(),
            });
        }
        fs::rename(&partial, &file).map_err(io(&file))?;
        drop(lock);
        // Only a lock: one left behind is harmless.
        let _ = fs::remove_file(&lock_path);
        Ok(file)
    }

    /// The image's size as the server reports it, when it does.
    fn size(&self) -> Option<u64> {
        let output = Command::new(CURL)
            .args(["--fail", "--location", "--silent", "--head"])
            .args([
                "--output",
                "/dev/null",
                "--write-out",
                "%header{content-length}",
            ])
            .arg(self.url.as_str())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }
}
