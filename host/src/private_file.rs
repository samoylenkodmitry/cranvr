//! Files that only the user may read, such as the runtime's token and the host's key.

use std::{
    fs::{self, OpenOptions, Permissions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

/// The mode of a file that only the user may read and write
pub const MODE: u32 = 0o600;

/// Replaces the file at `path` with `bytes`, readable and writable by the user alone. The bytes go
/// into a new file of that mode beside it, which then takes the place of the old one. So no reader
/// sees the bytes under a wider mode, or half written.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut new = path.as_os_str().to_owned();
    new.push(".new");
    let new = Path::new(&new);
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(MODE).open(new)?;
    // The mode of the open applies to a new file only: a file left by an earlier run keeps its own
    file.set_permissions(Permissions::from_mode(MODE))?;
    file.write_all(bytes)?;
    fs::rename(new, path)
}

#[cfg(test)]
#[path = "tests/private_file.rs"]
mod tests;
