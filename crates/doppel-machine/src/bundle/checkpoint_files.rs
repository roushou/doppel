//! A checkpoint's files in its bundle, and whether they are all there.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::ImageFiles;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointFiles {
    pub dir: PathBuf,
    pub image: ImageFiles,
    pub memory: PathBuf,
}

/// Whether a checkpoint is there: no folder, a folder without its memory or
/// its disk, or all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointPresence {
    Absent,
    Partial,
    Complete,
}

impl CheckpointFiles {
    pub(super) fn in_dir(dir: PathBuf) -> Self {
        Self {
            image: ImageFiles::in_dir(&dir),
            memory: dir.join("memory.vzsave"),
            dir,
        }
    }

    /// The paths whose existence decides its [`CheckpointPresence`].
    #[must_use]
    pub fn probes(&self) -> Vec<PathBuf> {
        vec![
            self.dir.clone(),
            self.memory.clone(),
            self.image.disk.clone(),
        ]
    }

    /// Its presence, given which of its [`probes`](Self::probes) exist.
    #[must_use]
    pub fn presence(&self, present: &BTreeSet<PathBuf>) -> CheckpointPresence {
        if !present.contains(&self.dir) {
            CheckpointPresence::Absent
        } else if present.contains(&self.memory) && present.contains(&self.image.disk) {
            CheckpointPresence::Complete
        } else {
            CheckpointPresence::Partial
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_is_complete_only_with_its_memory_and_disk() {
        let files = CheckpointFiles::in_dir(PathBuf::from("/b/checkpoints/ready"));
        let with = |paths: &[&PathBuf]| paths.iter().map(|p| (*p).clone()).collect();
        assert_eq!(files.presence(&BTreeSet::new()), CheckpointPresence::Absent);
        assert_eq!(
            files.presence(&with(&[&files.dir])),
            CheckpointPresence::Partial
        );
        assert_eq!(
            files.presence(&with(&[&files.dir, &files.memory])),
            CheckpointPresence::Partial
        );
        assert_eq!(
            files.presence(&with(&[&files.dir, &files.memory, &files.image.disk])),
            CheckpointPresence::Complete
        );
    }
}
