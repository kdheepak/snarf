use std::fs;
use std::path::Path;

use color_eyre::eyre;

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> eyre::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, contents)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_ID: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn replaces_file_and_removes_temp_file() {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("fs-atomic-tests")
            .join(format!("atomic-{}-{id}", std::process::id()));
        let path = root.join("state.json");

        super::write(&path, "first\n").expect("first write succeeds");
        super::write(&path, "second\n").expect("second write succeeds");

        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        assert!(!path.with_extension("json.tmp").exists());
    }
}
