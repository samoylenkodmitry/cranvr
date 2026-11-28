use super::{write, MODE};
use std::{fs, os::unix::fs::PermissionsExt};

// A file that an older host left readable by others gets the user's mode with its new bytes, and a
// reader that opened the old file keeps the old bytes whole
#[test]
fn a_private_file_replaces_a_wider_one_and_leaves_no_temporary_file() {
    let dir = std::env::temp_dir().join(format!("cranvr-private-file-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("runtime-control.txt");
    fs::write(&path, "old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let reader = fs::File::open(&path).unwrap();
    write(&path, b"9 token 874 914 120\n").unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, MODE);
    assert_eq!(fs::read_to_string(&path).unwrap(), "9 token 874 914 120\n");
    assert_eq!(std::io::read_to_string(reader).unwrap(), "old");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "a temporary file stayed");
    fs::remove_dir_all(&dir).ok();
}
