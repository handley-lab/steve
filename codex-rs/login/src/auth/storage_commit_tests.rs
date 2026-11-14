#![cfg(test)]

use super::*;

#[test]
fn deleting_absent_file_store_does_not_create_home() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("missing");
    let storage = create_auth_storage(
        home.clone(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    assert!(!storage.delete()?);
    assert!(!home.try_exists()?);
    Ok(())
}

#[test]
fn write_lock_coordinates_with_existing_auth_lock() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let storage = create_auth_storage(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    let _guard = storage.lock_for_write()?;
    let other_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.path().join("auth.lock"))?;
    assert!(matches!(
        other_writer.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    Ok(())
}
