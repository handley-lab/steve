//! Built-in pet cache ownership.
//!
//! Built-in pet spritesheets are read from the versioned cache under STEVE_HOME.
//! A missing or invalid cache is an error. This module does not fetch assets.
//! Custom pets remain local files outside this cache.

use std::path::Path;
use std::path::PathBuf;

#[cfg(test)]
use std::fs;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;

use super::catalog;

const PET_PACK_VERSION: &str = "v1";
const PET_PACK_DIR: &str = "cache/tui-pets";

pub(crate) fn builtin_spritesheet_path(codex_home: &Path, file: &str) -> PathBuf {
    pack_dir(codex_home).join("assets").join(file)
}

/// Require a structurally valid cached spritesheet for a built-in pet.
///
/// A missing cache is an error. This does not fetch a replacement.
pub(crate) fn ensure_builtin_pet(codex_home: &Path, pet: catalog::BuiltinPet) -> Result<()> {
    let destination = builtin_spritesheet_path(codex_home, pet.spritesheet_file);
    validate_cached_spritesheet(&destination)
        .with_context(|| format!("built-in pet {} is not cached locally", pet.id))
}

fn pack_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(PET_PACK_DIR).join(PET_PACK_VERSION)
}

fn validate_cached_spritesheet(path: &Path) -> Result<()> {
    let (width, height) =
        image::image_dimensions(path).with_context(|| format!("read {}", path.display()))?;
    if width != catalog::SPRITESHEET_WIDTH || height != catalog::SPRITESHEET_HEIGHT {
        bail!(
            "invalid pet spritesheet dimensions for {}: expected {}x{}, got {}x{}",
            path.display(),
            catalog::SPRITESHEET_WIDTH,
            catalog::SPRITESHEET_HEIGHT,
            width,
            height
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_test_pack(codex_home: &Path) {
    let assets_dir = pack_dir(codex_home).join("assets");
    fs::create_dir_all(&assets_dir).unwrap();
    for pet in catalog::BUILTIN_PETS {
        let path = assets_dir.join(pet.spritesheet_file);
        catalog::write_test_spritesheet(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_builtin_pet_does_not_build_a_cdn_url() {
        let dir = tempfile::tempdir().unwrap();
        let pet = catalog::builtin_pet("dewey").unwrap();

        let message = ensure_builtin_pet(dir.path(), pet)
            .expect_err("missing cache")
            .to_string();

        assert!(!message.contains("oaistatic.com"));
        assert!(!message.contains("https://"));
    }

    #[test]
    fn cached_builtin_pet_is_accepted_without_a_download() {
        let dir = tempfile::tempdir().unwrap();
        write_test_pack(dir.path());
        let pet = catalog::builtin_pet("dewey").unwrap();

        ensure_builtin_pet(dir.path(), pet).expect("cached spritesheet");
    }

    #[test]
    fn write_test_pack_installs_all_builtins() {
        let dir = tempfile::tempdir().unwrap();

        write_test_pack(dir.path());

        for pet in catalog::BUILTIN_PETS {
            let path = builtin_spritesheet_path(dir.path(), pet.spritesheet_file);
            assert!(path.is_file());
            validate_cached_spritesheet(&path).unwrap();
        }
    }
}
