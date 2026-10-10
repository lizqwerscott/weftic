use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use path_clean::PathClean;

use crate::permissions::Role;

#[derive(Debug, PartialEq, Eq)]
pub enum Confinement {
    Confined,
    /// 预留:暂无消费者,之后可能用于"在 root 之外再放行若干根"。
    #[allow(dead_code)]
    ExtraRoots(Vec<PathBuf>),
    Unconfined,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WorkspacePolicy {
    root: PathBuf,
    confinement: Confinement,
}

impl WorkspacePolicy {
    pub fn new(root: PathBuf, role: Role) -> Self {
        Self {
            root,
            confinement: match role {
                Role::Owner => Confinement::Unconfined,
                Role::Member => Confinement::Confined,
            },
        }
    }
}

pub struct Workspace {
    root: PathBuf,
    allowed_roots: Vec<PathBuf>,
    unconfined: bool,
}

impl Workspace {
    pub fn new(policy: &WorkspacePolicy) -> Result<Self> {
        let root = policy.root.as_path();

        if !root.exists() {
            fs::create_dir_all(root)
                .with_context(|| format!("creating workspace root {}", root.display()))?;
        }

        if !root.is_dir() {
            bail!("workspace root is not a directory: {}", root.display());
        }

        let root = root
            .canonicalize()
            .with_context(|| format!("resolving workspace root {}", root.display()))?;

        let (allowed_roots, unconfined) = match &policy.confinement {
            Confinement::Confined => (vec![root.clone()], false),
            Confinement::ExtraRoots(extra) => {
                let mut roots = vec![root.clone()];
                for path in extra {
                    let extra = path
                        .canonicalize()
                        .with_context(|| format!("resolving workspace root {}", path.display()))?;
                    roots.push(extra);
                }
                (dedupe_roots(roots), false)
            }
            Confinement::Unconfined => (vec![root.clone()], true),
        };

        Ok(Self {
            root,
            allowed_roots,
            unconfined,
        })
    }

    pub fn resolve(&self, raw: &str) -> Result<PathBuf> {
        let raw_path = Path::new(raw);

        let candidate = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            self.root.join(raw_path)
        };

        let candidate = candidate.clean();

        let mut ancestor: &Path = &candidate;
        let mut missing: Vec<&OsStr> = Vec::new();

        while !ancestor.exists() {
            let name = ancestor.file_name().ok_or_else(|| {
                anyhow::anyhow!("no existing ancestor for {}", candidate.display())
            })?;
            missing.push(name);
            ancestor = ancestor.parent().ok_or_else(|| {
                anyhow::anyhow!("no existing ancestor for {}", candidate.display())
            })?;
        }

        let canonical_ancestor = ancestor.canonicalize()?;

        let mut final_path = canonical_ancestor;
        for comp in missing.iter().rev() {
            final_path.push(comp);
        }

        if !self.unconfined
            && !self
                .allowed_roots
                .iter()
                .any(|allowed| final_path.starts_with(allowed))
        {
            return Err(anyhow::anyhow!(
                "path escapes workspace: {}",
                final_path.display()
            ));
        }

        Ok(final_path)
    }

    pub fn cwd(&self) -> &Path {
        &self.root
    }
}

fn dedupe_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut sorted = roots;
    sorted.sort();
    sorted.dedup();

    let mut res: Vec<PathBuf> = Vec::new();

    'outer: for p in sorted {
        for r in &res {
            if p.starts_with(r) {
                continue 'outer;
            }
        }
        res.push(p);
    }

    res
}

/// A unique, non-colliding directory under the system temp dir, for tests that
/// need a legal workspace base for `Workspace::new` to create roots under.
#[cfg(test)]
pub(crate) fn temp_base(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!("weftic-{tag}-{}-{n}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn confined(root: &Path) -> Workspace {
        Workspace::new(&WorkspacePolicy::new(root.to_path_buf(), Role::Member))
            .expect("confined workspace")
    }

    fn unconfined(root: &Path) -> Workspace {
        Workspace::new(&WorkspacePolicy::new(root.to_path_buf(), Role::Owner))
            .expect("unconfined workspace")
    }

    #[test]
    fn relative_paths_resolve_under_the_root() {
        let ws = confined(&temp_base("ws-rel"));

        assert_eq!(
            ws.resolve("sub/file.txt").unwrap(),
            ws.cwd().join("sub/file.txt")
        );
    }

    #[test]
    fn an_absolute_path_inside_the_root_is_allowed() {
        let ws = confined(&temp_base("ws-abs"));
        let inside = ws.cwd().join("inside.txt");

        assert_eq!(ws.resolve(inside.to_str().unwrap()).unwrap(), inside);
    }

    #[test]
    fn a_dot_dot_escape_is_rejected() {
        let ws = confined(&temp_base("ws-dotdot"));

        let error = ws.resolve("../outside.txt").unwrap_err().to_string();

        assert_eq!(
            error,
            format!(
                "path escapes workspace: {}",
                ws.cwd().parent().unwrap().join("outside.txt").display()
            )
        );
    }

    #[test]
    fn an_absolute_path_outside_the_root_is_rejected() {
        let ws = confined(&temp_base("ws-outside"));

        let error = ws.resolve("/etc/hosts").unwrap_err().to_string();

        assert!(
            error.contains("escapes workspace"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unconfined_allows_paths_outside_the_root() {
        let ws = unconfined(&temp_base("ws-open"));

        assert_eq!(ws.resolve("/etc/hosts").unwrap(), Path::new("/etc/hosts"));
    }
}
