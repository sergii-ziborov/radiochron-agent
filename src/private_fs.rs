use std::path::Path;

#[cfg(unix)]
pub fn restrict_directory(path: &Path) -> std::io::Result<()> {
    restrict_mode(path, 0o700)
}

#[cfg(not(unix))]
pub fn restrict_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn restrict_file(path: &Path) -> std::io::Result<()> {
    restrict_mode(path, 0o600)
}

#[cfg(not(unix))]
pub fn restrict_file(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn restrict_executable(path: &Path) -> std::io::Result<()> {
    restrict_mode(path, 0o700)
}

#[cfg(unix)]
fn restrict_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
pub fn restrict_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
