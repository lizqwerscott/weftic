use std::path::PathBuf;

use serde::Deserialize;

const DEFAULT_DIR: &str = "./data";
const DB_FILE: &str = "weftic.db";

#[derive(Debug, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_dir")]
    pub dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { dir: default_dir() }
    }
}

impl StorageConfig {
    pub fn db_path(&self) -> PathBuf {
        self.dir.join(DB_FILE)
    }
}

fn default_dir() -> PathBuf {
    PathBuf::from(DEFAULT_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dir_is_data() {
        assert_eq!(StorageConfig::default().dir, PathBuf::from("./data"));
    }

    #[test]
    fn db_path_is_the_database_file_inside_the_dir() {
        let config = StorageConfig {
            dir: PathBuf::from("/var/weftic"),
        };

        assert_eq!(config.db_path(), PathBuf::from("/var/weftic/weftic.db"));
    }
}
