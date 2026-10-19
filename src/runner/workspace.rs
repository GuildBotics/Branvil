//! The per-job work directory on the runner's machine: checking out the
//! source, collecting artifacts, and the dependency cache. Everything inside
//! the work directory was written by the job, so nothing here follows a
//! symbolic link: links are skipped (artifacts) or copied as links (cache).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ci::pipeline::{ArtifactDef, CacheDef, JobDef, check_relative};

use super::zip::ZipWriter;

/// Checks the commit out of its Git bundle (`GET …/source`) into the empty
/// directory `work`: a clone detached at the bundle's `HEAD`, with its
/// history and tags, and then without the remote that names the bundle. Git
/// runs with `global_config` (written empty) and writes no reflog, so nothing
/// of this machine (a path, its user) reaches the job. Git never runs in the
/// work directory once the job has had it.
pub async fn checkout(bundle: &Path, work: &Path, global_config: &Path) -> io::Result<()> {
    fs::write(global_config, "")?;
    let mut clone = crate::git::base_command(global_config);
    clone
        // Inside the VM the files have another owner and other inodes than
        // the index records: comparing only times, sizes, and modes keeps
        // the index clean there (`git diff-index --quiet HEAD`).
        .args([
            "-c",
            "core.logAllRefUpdates=false",
            "clone",
            "--quiet",
            "-c",
            "core.checkStat=minimal",
            "--",
        ])
        .arg(bundle)
        .arg(work);
    let mut unlink = crate::git::base_command(global_config);
    unlink
        .arg("-C")
        .arg(work)
        .args(["remote", "remove", "origin"]);
    for mut git in [clone, unlink] {
        let out = git
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| io::Error::other(format!("cannot run git: {e}")))?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "git: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
    }
    Ok(())
}

/// Fails when any component of `path` below `root` is a symbolic link or is
/// missing.
pub fn no_links_below(root: &Path, rel: &str) -> io::Result<PathBuf> {
    let mut p = root.to_path_buf();
    for c in rel.trim_end_matches('/').split('/') {
        p.push(c);
        let m = fs::symlink_metadata(&p)?;
        if m.file_type().is_symlink() {
            return Err(io::Error::other(format!(
                "{rel} goes through a symbolic link"
            )));
        }
    }
    Ok(p)
}

/// Every regular file under `path` (itself when it is a file), relative to
/// `root`, `/`-separated. Symbolic links are skipped and reported.
fn files_under(
    root: &Path,
    path: &Path,
    out: &mut Vec<(String, PathBuf)>,
    skipped: &mut Vec<String>,
) -> io::Result<()> {
    let m = fs::symlink_metadata(path)?;
    let rel = path
        .strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    if m.file_type().is_symlink() {
        skipped.push(rel);
    } else if m.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            files_under(root, &e, out, skipped)?;
        }
    } else if m.is_file() {
        out.push((rel, path.to_path_buf()));
    }
    Ok(())
}

/// A zipped artifact ready for upload.
pub struct Packed {
    pub name: String,
    pub data: Vec<u8>,
    pub files: usize,
}

/// Zips one artifact's paths. `budget` is what is left of the attempt's
/// artifact allowance; it shrinks by what this artifact takes. Notes for the
/// log go to `notes`.
pub fn pack(
    work: &Path,
    a: &ArtifactDef,
    budget: &mut u64,
    notes: &mut Vec<String>,
) -> io::Result<Option<Packed>> {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for p in &a.paths {
        if check_relative(p).is_err() {
            notes.push(format!(
                "artifact {}: {p} is not a relative path; skipped",
                a.name
            ));
            continue;
        }
        match no_links_below(work, p) {
            Ok(full) => files_under(work, &full, &mut files, &mut skipped)?,
            Err(e) => notes.push(format!("artifact {}: {p}: {e}; skipped", a.name)),
        }
    }
    for s in skipped {
        notes.push(format!(
            "artifact {}: {s} is a symbolic link; skipped",
            a.name
        ));
    }
    if files.is_empty() {
        notes.push(format!("artifact {}: no files found", a.name));
        return Ok(None);
    }
    let total: u64 = files
        .iter()
        .map(|(_, p)| fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();
    if total > *budget {
        notes.push(format!(
            "artifact {}: {total} bytes exceed the remaining artifact allowance of {} bytes; not uploaded",
            a.name, *budget
        ));
        return Ok(None);
    }
    *budget -= total;
    let mut z = ZipWriter::new(io::Cursor::new(Vec::new()));
    for (rel, path) in &files {
        // Opened without following a link that replaced the file meanwhile
        // is not expressible portably; the job has ended, so nothing writes
        // the directory any more.
        z.add(rel, fs::File::open(path)?)?;
    }
    Ok(Some(Packed {
        name: a.name.clone(),
        data: z.finish()?.into_inner(),
        files: files.len(),
    }))
}

// ---------------------------------------------------------------- cache

/// The cache key of a job in a scope: repository, job, the configuration
/// digest of the image it runs on (a new version of a tag does not reuse
/// what the previous one built), the user it runs as (the files keep the
/// owner the VM saw), cached paths, the content of the key files, and the
/// trust scope.
pub fn cache_key(
    repo: &str,
    job: &JobDef,
    image_digest: &str,
    cache: &CacheDef,
    scope: &str,
    work: &Path,
) -> String {
    let user = super::executor::JOB_USER_ID.to_string();
    let mut h = Vec::new();
    for part in [repo, &job.name, image_digest, &user, scope] {
        h.extend_from_slice(part.as_bytes());
        h.push(0);
    }
    for p in &cache.paths {
        h.extend_from_slice(p.as_bytes());
        h.push(0);
    }
    for k in &cache.key_files {
        h.extend_from_slice(k.as_bytes());
        h.push(0);
        match no_links_below(work, k).and_then(fs::read) {
            Ok(content) => h.extend_from_slice(&crate::util::sha256(&content)),
            Err(_) => h.extend_from_slice(b"<missing>"),
        }
        h.push(0);
    }
    crate::util::hex(&crate::util::sha256(&h))
}

/// Copies a tree without following symbolic links (links are recreated as
/// links on Unix and skipped elsewhere). Returns the bytes copied.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<u64> {
    let m = fs::symlink_metadata(from)?;
    // Never write through a link at the destination (the work directory's
    // content comes from the commit and the job): replace the link itself.
    if fs::symlink_metadata(to).is_ok_and(|d| d.file_type().is_symlink()) {
        fs::remove_file(to)?;
    }
    if m.file_type().is_symlink() {
        #[cfg(unix)]
        {
            let target = fs::read_link(from)?;
            let _ = fs::remove_file(to);
            std::os::unix::fs::symlink(target, to)?;
        }
        return Ok(0);
    }
    if m.is_dir() {
        fs::create_dir_all(to)?;
        let mut total = 0;
        for e in fs::read_dir(from)?.flatten() {
            total += copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(total)
    } else if m.is_file() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to)
    } else {
        Ok(0)
    }
}

fn tree_size(path: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(path) else {
        return 0;
    };
    if m.is_dir() {
        fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        m.len()
    }
}

const USED: &str = ".branvil-used";

fn record_use(entry: &Path, used: SystemTime) -> io::Result<()> {
    fs::File::create(entry.join(USED))?.set_modified(used)
}

/// Creates the missing parent directories of `rel` below `root`, refusing a
/// symbolic link anywhere on the way.
fn make_parents(root: &Path, rel: &str) -> io::Result<()> {
    let mut p = root.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for c in &parts[..parts.len().saturating_sub(1)] {
        p.push(c);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::other(format!(
                    "{rel} goes through a symbolic link"
                )));
            }
            Ok(_) => {}
            Err(_) => fs::create_dir(&p)?,
        }
    }
    Ok(())
}

/// Restores the first cache entry found among `keys` into the work
/// directory. Returns the key used.
pub fn restore(
    cache_root: &Path,
    keys: &[String],
    cache: &CacheDef,
    work: &Path,
) -> io::Result<Option<String>> {
    for key in keys {
        let entry = cache_root.join(key);
        if !entry.is_dir() {
            continue;
        }
        for p in &cache.paths {
            let src = entry.join("paths").join(p.trim_end_matches('/'));
            if fs::symlink_metadata(&src).is_ok() {
                let rel = p.trim_end_matches('/');
                make_parents(work, rel)?;
                copy_tree(&src, &work.join(rel))?;
            }
        }
        record_use(&entry, SystemTime::now())?;
        return Ok(Some(key.clone()));
    }
    Ok(None)
}

/// Saves the cached paths of a successful job under `key`, then removes the
/// least recently used entries beyond `max_bytes`.
pub fn save(
    cache_root: &Path,
    key: &str,
    cache: &CacheDef,
    work: &Path,
    max_bytes: u64,
) -> io::Result<u64> {
    save_with_clock(cache_root, key, cache, work, max_bytes, SystemTime::now)
}

fn save_with_clock(
    cache_root: &Path,
    key: &str,
    cache: &CacheDef,
    work: &Path,
    max_bytes: u64,
    now: impl FnOnce() -> SystemTime,
) -> io::Result<u64> {
    fs::create_dir_all(cache_root)?;
    let tmp = cache_root.join(format!(".{key}.{}", crate::util::random_secret::<6>()));
    let mut total = 0;
    for p in &cache.paths {
        if let Ok(src) = no_links_below(work, p) {
            total += copy_tree(&src, &tmp.join("paths").join(p.trim_end_matches('/')))?;
        }
    }
    fs::create_dir_all(&tmp)?;
    record_use(&tmp, now())?;
    let entry = cache_root.join(key);
    let _ = fs::remove_dir_all(&entry);
    fs::rename(&tmp, &entry)?;
    evict(cache_root, max_bytes, &entry)?;
    Ok(total)
}

fn evict(cache_root: &Path, max_bytes: u64, saved_entry: &Path) -> io::Result<()> {
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for e in fs::read_dir(cache_root)?.flatten() {
        let path = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let used = fs::metadata(path.join(USED))
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push((used, tree_size(&path), path));
    }
    // A save is the latest use when timestamps tie (e.g. on a coarse
    // filesystem). Other ties are broken by key, independently of size.
    entries.sort_by(|a, b| (a.0, a.2 == saved_entry, &a.2).cmp(&(b.0, b.2 == saved_entry, &b.2)));
    let mut total: u64 = entries.iter().map(|e| e.1).sum();
    for (_, size, path) in entries {
        if total <= max_bytes {
            break;
        }
        fs::remove_dir_all(&path)?;
        total -= size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The machine's Git attributes never change the checkout. Git finds
    /// them through the environment, so the checkout runs in a child test
    /// process whose `XDG_CONFIG_HOME` and `HOME` hold `* text eol=crlf`.
    #[tokio::test]
    async fn the_checkout_ignores_the_machines_git_attributes() {
        if std::env::var_os("BRANVIL_TEST_CHILD").is_some() {
            let dir = PathBuf::from(std::env::var_os("BRANVIL_TEST_DIR").unwrap());
            let work = dir.join("work");
            checkout(&dir.join("b.bundle"), &work, &dir.join("gitconfig"))
                .await
                .unwrap();
            assert_eq!(fs::read(work.join("f")).unwrap(), b"a\n");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for attributes in ["xdg/git/attributes", "home/.config/git/attributes"] {
            fs::create_dir_all(d.join(attributes).parent().unwrap()).unwrap();
            fs::write(d.join(attributes), "* text eol=crlf\n").unwrap();
        }
        fs::write(d.join("empty"), "").unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(d)
                .args(["-c", "credential.helper="])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", d.join("empty"))
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("XDG_CONFIG_HOME", d.join("xdg"))
                .env("HOME", d.join("home"))
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        fs::create_dir(d.join("r")).unwrap();
        fs::write(d.join("r/f"), "a\n").unwrap();
        git(&["-C", "r", "init", "-q"]);
        git(&["-C", "r", "add", "f"]);
        git(&["-C", "r", "commit", "-qm", "f"]);
        git(&["-C", "r", "bundle", "create", "-q", "../b.bundle", "HEAD"]);
        // Without the isolation, the attributes apply.
        git(&["clone", "-q", "b.bundle", "plain"]);
        assert_eq!(fs::read(d.join("plain/f")).unwrap(), b"a\r\n");
        super::super::pass_in_child(
            &format!(
                "{}::the_checkout_ignores_the_machines_git_attributes",
                module_path!()
            ),
            &[
                ("BRANVIL_TEST_DIR", d.as_os_str()),
                ("XDG_CONFIG_HOME", d.join("xdg").as_os_str()),
                ("HOME", d.join("home").as_os_str()),
            ],
        );
    }

    #[test]
    fn artifacts_skip_links_and_stay_inside_the_work_directory() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        fs::create_dir_all(work.join("out/sub")).unwrap();
        fs::write(work.join("out/report.txt"), "ok").unwrap();
        fs::write(work.join("out/sub/x.txt"), "x").unwrap();
        fs::write(dir.path().join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("secret.txt"), work.join("out/leak.txt"))
                .unwrap();
            std::os::unix::fs::symlink(dir.path(), work.join("outside")).unwrap();
        }
        let def = ArtifactDef {
            name: "report".into(),
            paths: vec!["out/".into(), "outside/secret.txt".into()],
        };
        let mut budget = 1 << 20;
        let mut notes = Vec::new();
        let packed = pack(&work, &def, &mut budget, &mut notes).unwrap().unwrap();
        let names = super::super::zip::names(&packed.data);
        assert_eq!(names, vec!["out/report.txt", "out/sub/x.txt"]);
        #[cfg(unix)]
        {
            assert!(
                notes.iter().any(|n| n.contains("out/leak.txt")),
                "{notes:?}"
            );
            assert!(
                notes.iter().any(|n| n.contains("outside/secret.txt")),
                "{notes:?}"
            );
        }
        let mut tiny = 1;
        assert!(pack(&work, &def, &mut tiny, &mut notes).unwrap().is_none());
    }

    #[test]
    fn cache_round_trip_and_eviction() {
        const IMAGE: &str =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let root = dir.path().join("cache");
        fs::create_dir_all(work.join("target")).unwrap();
        fs::write(work.join("Cargo.lock"), "v1").unwrap();
        fs::write(work.join("target/a.o"), "object").unwrap();
        let job = JobDef {
            name: "t".into(),
            image: "alpine".into(),
            commands: vec!["true".into()],
            timeout_secs: None,
            artifacts: vec![],
            cache: None,
        };
        let cache = CacheDef {
            paths: vec!["target".into()],
            key_files: vec!["Cargo.lock".into()],
        };
        let k1 = cache_key("acme/demo", &job, IMAGE, &cache, "protected", &work);
        assert_ne!(
            k1,
            cache_key("acme/demo", &job, IMAGE, &cache, "branch:x", &work),
            "scope is in the key"
        );
        assert_ne!(
            k1,
            cache_key(
                "acme/demo",
                &job,
                &IMAGE.replace('1', "2"),
                &cache,
                "protected",
                &work
            ),
            "the image's digest is in the key"
        );
        save(&root, &k1, &cache, &work, 1 << 20).unwrap();
        let fresh = dir.path().join("fresh");
        fs::create_dir_all(&fresh).unwrap();
        fs::write(fresh.join("Cargo.lock"), "v1").unwrap();
        assert_eq!(
            cache_key("acme/demo", &job, IMAGE, &cache, "protected", &fresh),
            k1
        );
        assert_eq!(
            restore(&root, std::slice::from_ref(&k1), &cache, &fresh).unwrap(),
            Some(k1.clone())
        );
        assert_eq!(
            fs::read_to_string(fresh.join("target/a.o")).unwrap(),
            "object"
        );
        fs::write(fresh.join("Cargo.lock"), "v2").unwrap();
        let k2 = cache_key("acme/demo", &job, IMAGE, &cache, "protected", &fresh);
        assert_ne!(k1, k2, "key files are in the key");
        // A limit that fits only one entry keeps the newest.
        save(&root, &k2, &cache, &fresh, 8).unwrap();
        assert!(!root.join(&k1).exists());
    }

    #[test]
    fn cache_eviction_breaks_timestamp_ties_by_key_not_size() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let used = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        for (key, size) in [("a", 4), ("b", 1), ("c", 2)] {
            let entry = root.join(key);
            fs::create_dir(&entry).unwrap();
            fs::write(entry.join("data"), vec![0; size]).unwrap();
            fs::File::create(entry.join(USED))
                .unwrap()
                .set_modified(used)
                .unwrap();
        }
        evict(root, 6, &root.join("c")).unwrap();
        assert!(!root.join("a").exists(), "equal timestamps evict by key");
        assert!(root.join("b").exists());
        assert!(root.join("c").exists());
    }

    #[test]
    fn cache_save_after_restore_keeps_the_save_when_timestamps_tie() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let work = dir.path().join("work");
        let fresh = dir.path().join("fresh");
        let cache = CacheDef {
            paths: vec!["target".into()],
            key_files: vec![],
        };
        fs::create_dir_all(work.join("target")).unwrap();
        fs::create_dir(&fresh).unwrap();
        fs::write(work.join("target/object"), b"object").unwrap();
        save(&root, "z-old", &cache, &work, 100).unwrap();
        assert_eq!(
            restore(&root, &["z-old".into()], &cache, &fresh).unwrap(),
            Some("z-old".into())
        );
        // Fix the restored entry's use time so the test never depends on
        // how far the wall clock advances between restore and save.
        let restored_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::open(root.join("z-old").join(USED))
            .unwrap()
            .set_modified(restored_at)
            .unwrap();

        // Simulate a coarse clock: the save has exactly the restore's
        // timestamp, and its key sorts before the restored entry's key.
        assert_eq!(
            save_with_clock(&root, "a-new", &cache, &fresh, 6, || restored_at).unwrap(),
            6
        );
        assert!(!root.join("z-old").exists());
        assert_eq!(
            fs::read(root.join("a-new/paths/target/object")).unwrap(),
            b"object"
        );
        assert_eq!(
            fs::metadata(root.join("a-new").join(USED))
                .unwrap()
                .modified()
                .unwrap(),
            restored_at,
            "save explicitly records the same clock as restore"
        );

        // The saved entry is still evicted when it cannot fit the budget.
        evict(&root, 5, &root.join("a-new")).unwrap();
        assert!(!root.join("a-new").exists());
    }

    #[test]
    fn cache_eviction_orders_distinct_timestamps_before_keys_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (key, size, seconds) in [("a", 1, 3), ("b", 2, 2), ("c", 4, 1)] {
            let entry = root.join(key);
            fs::create_dir(&entry).unwrap();
            fs::write(entry.join("data"), vec![0; size]).unwrap();
            record_use(
                &entry,
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
            )
            .unwrap();
        }
        evict(root, 6, &root.join("c")).unwrap();
        assert!(root.join("a").exists());
        assert!(root.join("b").exists());
        assert!(
            !root.join("c").exists(),
            "timestamp precedes save preference"
        );
    }
}
