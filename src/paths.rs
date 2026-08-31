use std::fs;
use std::path::{Path, PathBuf};

const DOLGORAE_HOME_DIRECTORY: &str = ".dolgorae";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DolgoraeHome {
    root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DolgoraeHomeError {
    path: PathBuf,
    reason: String,
}

impl DolgoraeHomeError {
    fn new(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
        }
    }

    pub fn into_parts(self) -> (PathBuf, String) {
        (self.path, self.reason)
    }
}

impl DolgoraeHome {
    pub fn system() -> Result<Self, DolgoraeHomeError> {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| DolgoraeHomeError::new("HOME", "HOME is not set"))?;
        if !home.is_absolute() {
            return Err(DolgoraeHomeError::new(&home, "HOME is not absolute"));
        }
        let canonical_home = fs::canonicalize(&home).map_err(|error| {
            DolgoraeHomeError::new(&home, format!("HOME cannot be resolved: {error}"))
        })?;
        Self::from_canonical_home(canonical_home)
    }

    pub fn from_canonical_home(canonical_home: PathBuf) -> Result<Self, DolgoraeHomeError> {
        if !canonical_home.is_absolute() {
            return Err(DolgoraeHomeError::new(
                &canonical_home,
                "canonical HOME is not absolute",
            ));
        }
        Ok(Self {
            root: canonical_home.join(DOLGORAE_HOME_DIRECTORY),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn workspace_root(&self, workspace_id: &str) -> PathBuf {
        self.root.join("workspaces").join(workspace_id)
    }

    #[must_use]
    pub fn operator_root(&self) -> PathBuf {
        self.root.join("operator")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_home() -> PathBuf {
        let root = std::env::temp_dir().join(format!("dolgorae-home-test-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn derives_one_private_home_root() {
        let home = test_home();
        let paths = DolgoraeHome::from_canonical_home(home.clone()).unwrap();
        assert_eq!(paths.root(), home.join(".dolgorae"));
        assert_eq!(
            paths.workspace_root("workspace"),
            home.join(".dolgorae/workspaces/workspace")
        );
        assert_eq!(paths.operator_root(), home.join(".dolgorae/operator"));
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn rejects_relative_canonical_home() {
        let error = DolgoraeHome::from_canonical_home(PathBuf::from("relative")).unwrap_err();
        assert_eq!(error.path, PathBuf::from("relative"));
    }
}
