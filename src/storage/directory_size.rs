use super::StorageService;
use crate::{
    error::{AppError, AppResult},
    storage_transaction::SYSTEM_DIR,
};

impl StorageService {
    pub(crate) async fn directory_size(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<u64> {
        let _permit = self.acquire_io().await?;
        let root = self.resolve_existing(relative).await?;
        if !self.metadata(&root).await?.is_dir() {
            return Err(AppError::NotFound);
        }
        let mut pending = vec![(root.relative().to_owned(), 0_usize)];
        let mut count = 0_usize;
        let mut total = 0_u64;
        while let Some((relative, depth)) = pending.pop() {
            if depth > 128 {
                return Err(AppError::BadRequest(
                    "目录层级过深，请选择较小的子目录".into(),
                ));
            }
            let directory = self.resolve_existing(&relative).await?;
            for entry in self.read_directory(&directory).await? {
                count += 1;
                if count > max_entries {
                    return Err(AppError::BadRequest(
                        "目录条目过多，请选择较小的子目录计算".into(),
                    ));
                }
                let name = entry.name;
                if name.eq_ignore_ascii_case(SYSTEM_DIR) {
                    continue;
                }
                if entry.metadata.is_dir() {
                    let name = name
                        .to_str()
                        .ok_or_else(|| AppError::BadRequest("目录名称无法读取".into()))?;
                    let child = if relative.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{relative}/{name}")
                    };
                    pending.push((child, depth + 1));
                } else if entry.metadata.is_file() {
                    total = total
                        .checked_add(entry.metadata.len())
                        .ok_or_else(|| AppError::internal("directory size overflow"))?;
                }
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;
    use tokio::fs;

    #[tokio::test]
    async fn measures_nested_files_and_empty_folders_without_directory_metadata_bytes() {
        let fixture = TestDirectory::new("directory-size");
        let storage = StorageService::new(fixture.path().into(), 100, 1, 100, 0)
            .await
            .unwrap();
        fs::create_dir_all(fixture.path().join("notes/empty"))
            .await
            .unwrap();
        fs::write(fixture.path().join("notes/a.txt"), b"hello")
            .await
            .unwrap();
        fs::write(fixture.path().join("notes/empty/b.txt"), b"world!")
            .await
            .unwrap();
        fs::create_dir(fixture.path().join("empty")).await.unwrap();
        assert_eq!(storage.directory_size("notes", 100).await.unwrap(), 11);
        assert_eq!(storage.directory_size("empty", 100).await.unwrap(), 0);
        assert_eq!(storage.directory_size("", 100).await.unwrap(), 11);
        assert!(matches!(
            storage.directory_size("notes", 1).await,
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            storage.directory_size("notes/a.txt", 100).await,
            Err(AppError::NotFound)
        ));
    }
}
