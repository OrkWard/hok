use std::path::{Component, Path, PathBuf};

use crate::{error::Fallible, internal, package::Package, Error, Session};

/// Link persistent files and directories defined by a package.
pub fn link(session: &Session, package: &Package) -> Fallible<()> {
    let persists = match package.manifest().persist() {
        Some(persists) => persists,
        None => return Ok(()),
    };
    let config = session.config();
    let version = if config.no_junction() {
        package.version()
    } else {
        "current"
    };
    let app_dir = config
        .root_path()
        .join("apps")
        .join(package.name())
        .join(version);
    let persist_dir = config.root_path().join("persist").join(package.name());

    for definition in persists {
        if definition.is_empty() {
            continue;
        }
        let source_relative = safe_relative(definition[0])?;
        let target_relative = safe_relative(definition.get(1).copied().unwrap_or(definition[0]))?;
        let source = app_dir.join(source_relative);
        let target = persist_dir.join(target_relative);

        if source.exists() {
            if target.exists() {
                remove_path(&source)?;
            } else {
                internal::fs::ensure_dir(target.parent().unwrap())?;
                std::fs::rename(&source, &target)?;
            }
        }

        if !target.exists() {
            internal::fs::ensure_dir(&target)?;
        }
        internal::fs::ensure_dir(source.parent().unwrap())?;
        internal::fs::symlink(&target, &source)?;
    }

    Ok(())
}

fn safe_relative(path: &str) -> Fallible<PathBuf> {
    let path = Path::new(path);
    if path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err(Error::Custom(format!(
            "unsafe persistent path '{}'",
            path.display()
        )));
    }
    Ok(path.to_owned())
}

fn remove_path(path: &Path) -> Fallible<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        internal::fs::remove_symlink(path)?;
    } else if metadata.is_dir() {
        internal::fs::remove_dir(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Remove persistent links for a package.
pub fn unlink(session: &Session, package: &Package) -> Fallible<()> {
    assert!(package.is_installed());

    if let Some(persists) = package.manifest().persist() {
        let config = session.config();
        let mut app_path = config.root_path().join("apps");
        app_path.push(package.name());

        let version = if config.no_junction() {
            package.installed_version().unwrap()
        } else {
            "current"
        };

        let persist_path = app_path.join(version);
        for persist in persists {
            assert!(!persist.is_empty());

            let src = internal::path::normalize_path(persist_path.join(persist[0]));
            internal::fs::remove_symlink(src)?;
        }
    }
    Ok(())
}
