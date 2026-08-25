use super::*;
use std::path::Component;

/// All file paths under one retained `openat::Dir` root, sorted, capped during
/// descriptor-relative traversal at [`MAX_INVENTORY_ITEMS`] entries /
/// [`MAX_INVENTORY_BYTES`] of path bytes / [`MAX_INVENTORY_DEPTH`] levels. A
/// symlink entry is listed as itself and never followed. Each traversed directory is opened
/// descriptor-relative with `openat::Dir` (no-follow `openat` on every component),
/// and only a **descriptor** of each subdirectory is queued; a path is **never**
/// re-opened, so a concurrent rename of a scanned directory onto a symlink cannot redirect a
/// later re-open outside the root. Traversal **stops** when a cap is reached and it does
/// not collect the whole tree and truncate afterwards, so a hostile workspace with an
/// unbounded number of files cannot force an unbounded inventory or an unbounded directory
/// scan; when a cap is hit [`Inventory::truncated`] is set.
pub fn inventory(root: &Path) -> Inventory {
    inventory_inner(root, |_, _| {})
}

pub fn inventory_cap(root: &crate::tools::WorkspaceCap) -> Inventory {
    let root_dir = match root.root_dir().try_clone() {
        Ok(dir) => dir,
        Err(_) => return InventoryState::default().finish(false),
    };
    inventory_from_dir(Path::new(""), root_dir, |_, _| {})
}

pub(super) fn inventory_inner<F>(root: &Path, before_descend: F) -> Inventory
where
    F: FnMut(&Path, &str),
{
    let root_dir = match openat::Dir::open(root) {
        Ok(dir) => dir,
        Err(_) => return InventoryState::default().finish(false),
    };
    inventory_from_dir(root, root_dir, before_descend)
}

fn inventory_from_dir<F>(root: &Path, root_dir: openat::Dir, mut before_descend: F) -> Inventory
where
    F: FnMut(&Path, &str),
{
    let mut state = InventoryState::default();
    let mut stack = vec![(PathBuf::new(), root_dir, 0)];
    while let Some((prefix, dir, depth)) = stack.pop() {
        let entries = match dir.list_self() {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let mut cursor = InventoryCursor {
                root,
                prefix: &prefix,
                dir: &dir,
                depth,
                stack: &mut stack,
                before_descend: &mut before_descend,
            };
            if state.add_entry(name, entry.simple_type(), &mut cursor) {
                return state.finish(true);
            }
        }
    }
    state.finish(false)
}

#[derive(Default)]
struct InventoryState {
    files: Vec<String>,
    bytes: usize,
    entries: usize,
}

struct InventoryCursor<'a, F> {
    root: &'a Path,
    prefix: &'a Path,
    dir: &'a openat::Dir,
    depth: usize,
    stack: &'a mut Vec<(PathBuf, openat::Dir, usize)>,
    before_descend: &'a mut F,
}

impl InventoryState {
    fn add_entry<F>(
        &mut self,
        name: String,
        entry_type: Option<openat::SimpleType>,
        cursor: &mut InventoryCursor<'_, F>,
    ) -> bool
    where
        F: FnMut(&Path, &str),
    {
        self.entries = self.entries.saturating_add(1);
        if self.entries > MAX_INVENTORY_ENTRIES {
            return true;
        }
        if matches!(name.as_str(), "target" | ".git" | "__pycache__") {
            return false;
        }
        if self.files.len() >= MAX_INVENTORY_ITEMS {
            return true;
        }
        let rel = inventory_relative(cursor.prefix, &name);
        if self.bytes.saturating_add(rel.len()) > MAX_INVENTORY_BYTES {
            return true;
        }
        let is_dir = entry_type == Some(openat::SimpleType::Dir);
        if is_dir && cursor.depth + 1 >= MAX_INVENTORY_DEPTH {
            return true;
        }
        if is_dir {
            (cursor.before_descend)(cursor.root, &rel);
            if let Ok(child) = cursor.dir.sub_dir(&name) {
                cursor
                    .stack
                    .push((cursor.prefix.join(&name), child, cursor.depth + 1));
            }
        }
        self.bytes = self.bytes.saturating_add(rel.len());
        self.files.push(rel);
        false
    }

    fn finish(mut self, truncated: bool) -> Inventory {
        self.files.sort();
        Inventory {
            files: self.files,
            truncated,
        }
    }
}

fn inventory_relative(prefix: &Path, name: &str) -> String {
    if prefix.as_os_str().is_empty() {
        name.to_string()
    } else {
        prefix.join(name).to_string_lossy().to_string()
    }
}

/// Fraction of `required` files (relative to `root`) that are present **as regular files**:
/// a missing file or a symlink never counts.
pub fn score_present(root: &Path, required: &[&str]) -> f64 {
    if required.is_empty() {
        return 0.0;
    }
    let found = required
        .iter()
        .filter(|f| is_regular_no_follow(root, f))
        .count();
    found as f64 / required.len() as f64
}

/// Capability-based variant used after an agent run, so grading never resolves the workspace
/// pathname again.
pub fn score_present_cap(root: &crate::tools::WorkspaceCap, required: &[&str]) -> f64 {
    if required.is_empty() {
        return 0.0;
    }
    let found = required
        .iter()
        .filter(|relative| is_regular_at(root.root_dir(), relative))
        .count();
    found as f64 / required.len() as f64
}

fn is_regular_at(root: &openat::Dir, relative: &str) -> bool {
    let path = Path::new(relative);
    if path.is_absolute() {
        return false;
    }
    let components: Vec<Component> = path.components().collect();
    if components.is_empty() {
        return false;
    }
    let Ok(mut current) = root.try_clone() else {
        return false;
    };
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return false;
        };
        if index + 1 == components.len() {
            return current
                .metadata(*name)
                .map(|metadata| metadata.simple_type() == openat::SimpleType::File)
                .unwrap_or(false);
        }
        let Ok(child) = current.sub_dir(*name) else {
            return false;
        };
        current = child;
    }
    false
}

/// Whether `rel` names a real regular file under `root`, with no symlink on any component
/// and no absolute/`..` path.
pub fn is_regular_no_follow(root: &Path, rel: &str) -> bool {
    use std::path::Component;
    let p = Path::new(rel);
    if p.is_absolute() {
        return false;
    }
    let comps: Vec<Component> = p.components().collect();
    if comps.is_empty() {
        return false;
    }
    let mut cur = root.to_path_buf();
    for (i, c) in comps.iter().enumerate() {
        let Component::Normal(os) = c else {
            return false;
        };
        cur.push(os);
        let Ok(meta) = std::fs::symlink_metadata(&cur) else {
            return false;
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            return false;
        }
        if i + 1 == comps.len() {
            return ft.is_file();
        }
        if !ft.is_dir() {
            return false;
        }
    }
    false
}
