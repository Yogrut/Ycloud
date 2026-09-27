//! A process-wide lock for one deployment's configuration/data directory.
use anyhow::Context;
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

pub(crate) struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    pub(crate) fn acquire(config_path: &Path) -> anyhow::Result<Self> {
        let parent = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).context("无法创建 Ycloud 配置目录")?;
        let path = parent.join(".ycloud-instance.lock");
        Ok(Self {
            _file: Self::acquire_file(&path)?,
        })
    }

    pub(crate) fn acquire_file(path: &Path) -> anyhow::Result<File> {
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if !metadata.is_file() || crate::storage::is_link_or_reparse_point(&metadata) {
                anyhow::bail!("Ycloud 实例锁必须是普通文件");
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .context("无法打开 Ycloud 实例锁")?;
        file.try_lock()
            .context("Ycloud 数据目录正在使用；请停止另一个实例后重试")?;
        // Never unlink the lock: on Unix that could create two independent locks.
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_is_reusable_after_its_owner_exits() {
        let root = std::env::temp_dir().join(format!("ycloud-instance-{}", uuid::Uuid::new_v4()));
        let config = root.join("config.json");
        let lock = InstanceLock::acquire(&config).unwrap();
        assert!(InstanceLock::acquire(&config).is_err());
        drop(lock);
        drop(InstanceLock::acquire(&config).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
