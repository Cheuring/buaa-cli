use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use super::task::State;

pub fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let temp = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    Ok(())
}

pub fn load_state(dir: &Path) -> io::Result<State> {
    let mut state: State = match read_json(&dir.join("state.json")) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => State::default(),
        Err(error) => return Err(error),
    };
    if state.version != 1 {
        return Err(io::Error::other("Unsupported task state version"));
    }
    state.recover();
    Ok(state)
}

/// OS locks are released on process exit, including crashes. Never unlink this
/// file: replacing its inode could allow two daemons to own different locks.
pub fn lock(dir: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("server.lock"))?;
    file.try_lock().map_err(io::Error::other)?;
    Ok(file)
}
