//! The accounts' Google profile photos as the app last fetched them: one file per account,
//! named by its address, in `profile-pictures/` in the state folder. The folder is device-wide,
//! so clearing an account's mail leaves the account's photo, and signing out deletes it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Error;

/// The folder's name in the state folder.
const DIR: &str = "profile-pictures";

static NEXT_PARTIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct ProfilePictures {
    dir: PathBuf,
}

impl ProfilePictures {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(DIR),
        }
    }

    fn path(&self, email: &str) -> Result<PathBuf, Error> {
        if email.is_empty() || email == "." || email == ".." || email.contains(['/', '\\', '\0']) {
            return Err(Error::ProfilePicture {
                path: self.dir.clone(),
                source: io::Error::new(io::ErrorKind::InvalidInput, "invalid account address"),
            });
        }
        Ok(self.dir.join(email))
    }

    /// `email`'s photo, or `None` without one.
    pub(crate) fn get(&self, email: &str) -> Result<Option<Vec<u8>>, Error> {
        let path = self.path(email)?;
        match std::fs::read(&path) {
            Ok(photo) => Ok(Some(photo)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::ProfilePicture { path, source }),
        }
    }

    /// Saves `photo` as `email`'s photo through a temporary file, so a crash never leaves half of
    /// one. `None` deletes it.
    pub(crate) fn set(&self, email: &str, photo: Option<&[u8]>) -> Result<(), Error> {
        let path = self.path(email)?;
        let failed = |source| Error::ProfilePicture {
            path: path.clone(),
            source,
        };
        let Some(photo) = photo else {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(failed(error)),
                _ => Ok(()),
            };
        };
        let n = NEXT_PARTIAL.fetch_add(1, Ordering::Relaxed);
        let partial = self.dir.join(format!("{email}.{n}.partial"));
        std::fs::create_dir_all(&self.dir).map_err(failed)?;
        std::fs::write(&partial, photo).map_err(failed)?;
        if let Err(error) = std::fs::rename(&partial, &path) {
            let _ = std::fs::remove_file(&partial);
            return Err(failed(error));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths_and_isolates_concurrent_writes() {
        let dir = tempfile::tempdir().unwrap();
        let pictures = ProfilePictures::new(dir.path());
        for bad in ["", ".", "..", "../escape", "a/b", "a\\b", "a\0b"] {
            assert!(
                matches!(pictures.get(bad), Err(Error::ProfilePicture { .. })),
                "{bad:?}"
            );
            assert!(
                matches!(
                    pictures.set(bad, Some(b"x")),
                    Err(Error::ProfilePicture { .. })
                ),
                "{bad:?}"
            );
        }

        std::thread::scope(|scope| {
            for i in 0..8_u8 {
                let pictures = &pictures;
                scope.spawn(move || {
                    let data = vec![i; 64];
                    pictures.set("ada@example.com", Some(&data)).unwrap();
                });
            }
        });
        let final_photo = pictures.get("ada@example.com").unwrap().unwrap();
        assert_eq!(final_photo.len(), 64);
        assert!(final_photo.iter().all(|&b| b == final_photo[0]));
        let entries: Vec<String> = std::fs::read_dir(dir.path().join(DIR))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            ["ada@example.com"],
            "no .partial files left behind"
        );
    }
}
