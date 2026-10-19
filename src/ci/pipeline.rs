//! The pipeline definition, `.branvil/ci.toml`, read from the commit that runs
//! it (see `docs/ci.md`). The schema is small and validated strictly: anything
//! unknown or out of range makes the whole pipeline invalid, which shows as a
//! failing `pipeline` check, never as success.

use serde::{Deserialize, Serialize};

use std::path::Path;

use super::queue::PipelineState;
use crate::error::{AppError, AppResult};
use crate::git::Git;
use crate::util::valid_name;

/// Where the pipeline lives in a commit.
pub const PIPELINE_PATH: &str = ".branvil/ci.toml";
/// Largest pipeline file read.
pub const MAX_PIPELINE_BYTES: usize = 64 << 10;

const MAX_JOBS: usize = 32;
const MAX_COMMANDS: usize = 64;
const MAX_COMMAND_BYTES: usize = 8 << 10;
const MAX_SCRIPT_BYTES: usize = 64 << 10;
const MAX_ARTIFACTS: usize = 16;
const MAX_PATHS: usize = 32;
const MAX_CACHE_PATHS: usize = 8;

/// Reads the pipeline of commit `sha` in the repository at `dir`. Only a
/// pipeline path the commit's tree does not have reads as `Absent`; any Git
/// failure (a missing commit, an unreadable tree or blob) is an error, never a
/// pipeline (the service's registration is retried by the reconciler).
pub async fn read(git: &Git, dir: &Path, sha: &str) -> AppResult<PipelineState> {
    let failed = |what: &str, stderr: &str| {
        AppError::internal(format!(
            "{what} {PIPELINE_PATH} of {sha}: {}",
            stderr.trim()
        ))
    };
    let out = git
        .run(Some(dir), &["ls-tree", "-z", sha, "--", PIPELINE_PATH])
        .await?;
    if !out.ok() {
        return Err(failed("listing", &out.stderr));
    }
    // `<mode> <type> <object>\t<path>`, or nothing when the path is not there.
    let entry = out.text();
    let mut fields = entry.trim_end_matches('\0').split(['\t', ' ']);
    let (kind, object) = match (fields.next(), fields.next(), fields.next()) {
        (None | Some(""), ..) => return Ok(PipelineState::Absent),
        (Some(_), Some(kind), Some(object)) => (kind, object.to_string()),
        _ => return Err(failed("listing", &format!("unexpected entry {entry:?}"))),
    };
    if kind != "blob" {
        return Ok(PipelineState::Invalid {
            message: format!("{} is not a file.", PIPELINE_PATH),
        });
    }
    let out = git.run(Some(dir), &["cat-file", "blob", &object]).await?;
    if !out.ok() {
        return Err(failed("reading", &out.stderr));
    }
    if out.stdout.len() > MAX_PIPELINE_BYTES {
        return Ok(PipelineState::Invalid {
            message: format!(
                "{} is larger than {} bytes.",
                PIPELINE_PATH, MAX_PIPELINE_BYTES
            ),
        });
    }
    Ok(match String::from_utf8(out.stdout) {
        Ok(text) => match parse(&text) {
            Ok(jobs) => PipelineState::Jobs { jobs },
            Err(message) => PipelineState::Invalid { message },
        },
        Err(_) => PipelineState::Invalid {
            message: format!("{} is not UTF-8.", PIPELINE_PATH),
        },
    })
}

/// One job as recorded with its attempts (the snapshot taken when the run was
/// registered; retries reuse it).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct JobDef {
    pub name: String,
    pub image: String,
    pub commands: Vec<String>,
    pub timeout_secs: u64,
    pub cpus: u32,
    pub memory_mib: u64,
    pub artifacts: Vec<ArtifactDef>,
    pub cache: Option<CacheDef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDef {
    pub name: String,
    /// Files or directories, relative to the work directory.
    pub paths: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CacheDef {
    /// Directories (relative to the work directory) restored before and saved
    /// after the job.
    pub paths: Vec<String>,
    /// Files whose content is part of the cache key (`Cargo.lock`, ...).
    #[serde(default)]
    pub key_files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSchema {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    jobs: Vec<JobSchema>,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobSchema {
    name: String,
    image: String,
    commands: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_cpus")]
    cpus: u32,
    #[serde(default = "default_memory")]
    memory_mib: u64,
    #[serde(default)]
    artifacts: Vec<ArtifactDef>,
    cache: Option<CacheDef>,
}

fn default_timeout() -> u64 {
    600
}
fn default_cpus() -> u32 {
    1
}
fn default_memory() -> u64 {
    1024
}

/// Parses and validates a pipeline. `Err` is the message shown on the
/// failing `pipeline` check.
pub fn parse(text: &str) -> Result<Vec<JobDef>, String> {
    let file: FileSchema =
        toml::from_str(text).map_err(|e| format!("{PIPELINE_PATH} is not valid: {e}"))?;
    if file.version != 1 {
        return Err(format!(
            "{PIPELINE_PATH}: version {} is not supported (this Branvil reads version 1).",
            file.version
        ));
    }
    if file.jobs.is_empty() {
        return Err(format!("{PIPELINE_PATH} defines no [[jobs]]."));
    }
    if file.jobs.len() > MAX_JOBS {
        return Err(format!(
            "{PIPELINE_PATH} defines more than {MAX_JOBS} jobs."
        ));
    }
    let mut jobs = Vec::new();
    for j in file.jobs {
        let job = validate_job(j)?;
        if jobs
            .iter()
            .any(|x: &JobDef| crate::util::fold(&x.name) == crate::util::fold(&job.name))
        {
            return Err(format!("Two jobs are named {}.", job.name));
        }
        jobs.push(job);
    }
    Ok(jobs)
}

fn validate_job(j: JobSchema) -> Result<JobDef, String> {
    let name = j.name;
    let at = |what: &str| format!("Job {name}: {what}");
    if !valid_name(&name) || name == "pipeline" {
        return Err(format!(
            "'{name}' is not a valid job name ([A-Za-z0-9_.-], at most 64, not 'pipeline')."
        ));
    }
    if !valid_image(&j.image) {
        return Err(at(&format!(
            "'{}' is not an image reference (name[:tag][@sha256:digest]).",
            j.image
        )));
    }
    if j.commands.is_empty() || j.commands.len() > MAX_COMMANDS {
        return Err(at(&format!(
            "commands must have 1 to {MAX_COMMANDS} entries."
        )));
    }
    let mut total = 0;
    for c in &j.commands {
        if c.trim().is_empty() || c.len() > MAX_COMMAND_BYTES || c.contains('\0') {
            return Err(at(&format!(
                "every command must be non-empty, without NUL, at most {MAX_COMMAND_BYTES} bytes."
            )));
        }
        total += c.len();
    }
    if total > MAX_SCRIPT_BYTES {
        return Err(at(&format!(
            "the commands are longer than {MAX_SCRIPT_BYTES} bytes together."
        )));
    }
    if !(1..=86_400).contains(&j.timeout_secs) {
        return Err(at("timeout_secs must be between 1 and 86400."));
    }
    if !(1..=64).contains(&j.cpus) {
        return Err(at("cpus must be between 1 and 64."));
    }
    if !(128..=65_536).contains(&j.memory_mib) {
        return Err(at("memory_mib must be between 128 and 65536."));
    }
    if j.artifacts.len() > MAX_ARTIFACTS {
        return Err(at(&format!("at most {MAX_ARTIFACTS} artifacts.")));
    }
    let mut names: Vec<String> = Vec::new();
    for a in &j.artifacts {
        if !valid_name(&a.name) {
            return Err(at(&format!("'{}' is not a valid artifact name.", a.name)));
        }
        if names.contains(&crate::util::fold(&a.name)) {
            return Err(at(&format!("two artifacts are named {}.", a.name)));
        }
        names.push(crate::util::fold(&a.name));
        if a.paths.is_empty() || a.paths.len() > MAX_PATHS {
            return Err(at(&format!(
                "artifact {} needs 1 to {MAX_PATHS} paths.",
                a.name
            )));
        }
        for p in &a.paths {
            check_relative(p).map_err(|m| at(&m))?;
        }
    }
    if let Some(c) = &j.cache {
        if c.paths.is_empty() || c.paths.len() > MAX_CACHE_PATHS {
            return Err(at(&format!("cache needs 1 to {MAX_CACHE_PATHS} paths.")));
        }
        if c.key_files.len() > MAX_CACHE_PATHS {
            return Err(at(&format!(
                "cache takes at most {MAX_CACHE_PATHS} key_files."
            )));
        }
        for p in c.paths.iter().chain(&c.key_files) {
            check_relative(p).map_err(|m| at(&m))?;
        }
    }
    Ok(JobDef {
        name,
        image: j.image,
        commands: j.commands,
        timeout_secs: j.timeout_secs,
        cpus: j.cpus,
        memory_mib: j.memory_mib,
        artifacts: j.artifacts,
        cache: j.cache,
    })
}

/// A path inside the work directory: relative, `/`-separated, no `.`/`..`
/// components, no empty components except a trailing `/`.
pub fn check_relative(path: &str) -> Result<(), String> {
    let trimmed = path.strip_suffix('/').unwrap_or(path);
    let ok = !trimmed.is_empty()
        && trimmed.len() <= 512
        && !trimmed.starts_with('/')
        && !trimmed.contains('\\')
        && !trimmed.contains('\0')
        && !trimmed.contains(':')
        && trimmed
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "'{path}' is not a relative path inside the work directory."
        ))
    }
}

/// An OCI reference: `[registry/]name[:tag][@sha256:<64 hex>]`, conservative
/// characters only, never starting with `-` (it is passed as an argument).
pub fn valid_image(image: &str) -> bool {
    if image.is_empty() || image.len() > 256 || image.starts_with('-') {
        return false;
    }
    if !image
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b))
    {
        return false;
    }
    match image.split_once('@') {
        Some((name, digest)) => {
            !name.is_empty()
                && digest
                    .strip_prefix("sha256:")
                    .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "docker.io/library/alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8";

    #[test]
    fn a_minimal_pipeline_gets_defaults() {
        let jobs = parse(&format!(
            "[[jobs]]\nname = \"test\"\nimage = \"{DIGEST}\"\ncommands = [\"true\"]\n"
        ))
        .unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].timeout_secs, 600);
        assert_eq!(jobs[0].cpus, 1);
        assert_eq!(jobs[0].memory_mib, 1024);
        assert!(jobs[0].artifacts.is_empty());
    }

    #[test]
    fn everything_unknown_or_out_of_range_is_refused() {
        let job = |extra: &str| {
            format!("[[jobs]]\nname = \"t\"\nimage = \"alpine\"\ncommands = [\"true\"]\n{extra}")
        };
        assert!(parse("").is_err(), "no jobs");
        assert!(parse("version = 2\n[[jobs]]\nname=\"t\"\nimage=\"a\"\ncommands=[\"x\"]").is_err());
        assert!(parse(&job("privileged = true\n")).is_err(), "unknown key");
        assert!(parse(&job("timeout_secs = 0\n")).is_err());
        assert!(parse(&job("cpus = 0\n")).is_err());
        assert!(parse(&job("memory_mib = 64\n")).is_err());
        assert!(
            parse(&job("artifacts = [{name = \"r\", paths = [\"../etc\"]}]\n")).is_err(),
            "artifact path escapes"
        );
        assert!(parse(&job("artifacts = [{name = \"r\", paths = [\"/etc\"]}]\n")).is_err());
        assert!(parse(&job("artifacts = [{name = \"a/b\", paths = [\"x\"]}]\n")).is_err());
        assert!(parse(&job("cache = {paths = []}\n")).is_err());
        assert!(
            parse("[[jobs]]\nname=\"t\"\nimage=\"-x\"\ncommands=[\"x\"]").is_err(),
            "image looking like an option"
        );
        assert!(parse("[[jobs]]\nname=\"t\"\nimage=\"a\"\ncommands=[]").is_err());
        assert!(
            parse(
                "[[jobs]]\nname=\"t\"\nimage=\"a\"\ncommands=[\"x\"]\n[[jobs]]\nname=\"T\"\nimage=\"a\"\ncommands=[\"x\"]"
            )
            .is_err(),
            "duplicate names"
        );
        assert!(parse("[[jobs]]\nname=\"pipeline\"\nimage=\"a\"\ncommands=[\"x\"]").is_err());
        assert!(
            parse(&job(
                "artifacts = [{name = \"r\", paths = [\"out/\"]}]\ncache = {paths = [\"target\"], key_files = [\"Cargo.lock\"]}\n"
            ))
            .is_ok()
        );
    }

    #[test]
    fn image_references() {
        assert!(valid_image("alpine"));
        assert!(valid_image("docker.io/library/alpine:3.22"));
        assert!(valid_image(DIGEST));
        assert!(!valid_image("alpine@sha256:abc"));
        assert!(!valid_image("alpine latest"));
        assert!(!valid_image("--privileged"));
        assert!(!valid_image(""));
    }
}
