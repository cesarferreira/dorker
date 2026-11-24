//! `dorker push` and `dorker pull <registry>/<name>:<tag>`: share built images through an OCI
//! registry (GitHub Container Registry, a local `registry:2`, ECR, Artifactory, ...).
//!
//! An image travels as an OCI artifact. Its config blob is the image's `image.json`, and its
//! AVD home is one tar stream, compressed with zstd and cut into parts of at most
//! [`PART_SIZE`] bytes (one layer each, so no single upload is huge). Only the AVD is sent:
//! the Android system image comes from Google's SDK on the receiving machine, exactly as for a
//! local build.
//!
//! The tag points to an OCI index with one manifest per CPU architecture. A Mac (arm64) and a
//! Linux machine (x86_64) each push their own build of the same Dorkerfile under one name, and
//! `pull` picks the one this machine can run.

use crate::errors;
use crate::images::{self, ImageMeta};
use crate::runtime;
use crate::sdk::{self, Sdk};
use crate::store::Store;
use crate::style::Style;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.dorker.image.config.v1+json";
pub const PART_MEDIA_TYPE: &str = "application/vnd.dorker.image.part.v1.tar+zstd";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const DOCKER_LIST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
/// Manifest annotation: total size of the unpacked AVD, for the free-space check on pull.
const UNPACKED_SIZE_KEY: &str = "dev.dorker.unpacked-size";

/// Largest compressed part, so each upload stays well under registry request limits.
pub const PART_SIZE: u64 = 512 * 1024 * 1024;

// ---------- references ----------

/// `registry/repository:tag`, e.g. `ghcr.io/acme/gearhead:18.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub registry: String,
    pub repository: String,
    pub tag: String,
}

impl Reference {
    /// `Ok(None)` when `s` is not a registry reference (a local name like `myapp:ready`).
    /// Like Docker, a first path part with a `.` or a `:`, or `localhost`, names a registry.
    pub fn parse(s: &str) -> Result<Option<Self>> {
        let Some((host, rest)) = s.split_once('/') else {
            return Ok(None);
        };
        if !(host.contains('.') || host.contains(':') || host == "localhost") {
            return Ok(None);
        }
        if rest.contains('@') {
            bail!("{s:?}: pulling by digest is not supported; use a tag");
        }
        let (repository, tag) = match rest.rsplit_once(':') {
            Some((r, t)) => (r, t),
            None => (rest, "latest"),
        };
        let host_ok = host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
        let repo_ok = !repository.is_empty()
            && repository.split('/').all(|part| {
                !part.is_empty()
                    && part.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
                    })
            });
        let tag_ok = !tag.is_empty()
            && tag.len() <= 128
            && tag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !host_ok || !repo_ok || !tag_ok {
            bail!(
                "invalid registry reference {s:?}: use `registry/repository[:tag]` with a lowercase repository, e.g. ghcr.io/your-org/gearhead:18.0"
            );
        }
        Ok(Some(Self {
            registry: host.to_string(),
            repository: repository.to_string(),
            tag: tag.to_string(),
        }))
    }

    /// The local tag a pulled or pushed image gets, so `dorker run <reference>` finds it.
    pub fn local_tag(&self) -> String {
        format!("{}/{}:{}", self.registry, self.repository, self.tag)
    }

    /// Plain HTTP only for a registry on this machine (a test `registry:2`), like Docker.
    fn base_url(&self) -> String {
        let host = self.registry.split(':').next().unwrap_or_default();
        let scheme = if matches!(host, "localhost" | "127.0.0.1") {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}", self.registry)
    }
}

impl std::fmt::Display for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.local_tag())
    }
}

/// OCI architecture name for an Android ABI.
pub fn arch_of_abi(abi: &str) -> &str {
    match abi {
        "arm64-v8a" => "arm64",
        "x86_64" => "amd64",
        "x86" => "386",
        "armeabi-v7a" => "arm",
        other => other,
    }
}

// ---------- index handling (pure, unit-tested) ----------

fn is_index(v: &Value) -> bool {
    matches!(
        v.get("mediaType").and_then(Value::as_str),
        Some(INDEX_MEDIA_TYPE | DOCKER_LIST_MEDIA_TYPE)
    ) || (v.get("manifests").is_some() && v.get("layers").is_none())
}

fn entry_arch(d: &Value) -> Option<&str> {
    d.pointer("/platform/architecture").and_then(Value::as_str)
}

/// The index after adding `entry` for `arch`: any older entry for that architecture is
/// replaced, entries for other architectures are kept. A plain manifest at the tag (not an
/// index) is replaced.
fn merge_index(existing: Option<&Value>, entry: Value, arch: &str) -> Value {
    let mut manifests: Vec<Value> = existing
        .filter(|v| is_index(v))
        .and_then(|v| v.get("manifests"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|d| entry_arch(d) != Some(arch))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    manifests.push(entry);
    manifests.sort_by(|a, b| entry_arch(a).cmp(&entry_arch(b)));
    json!({
        "schemaVersion": 2,
        "mediaType": INDEX_MEDIA_TYPE,
        "manifests": manifests,
    })
}

/// The index entry for `arch`.
fn select_from_index<'a>(index: &'a Value, arch: &str) -> Option<&'a Value> {
    index
        .get("manifests")?
        .as_array()?
        .iter()
        .find(|d| entry_arch(d) == Some(arch))
}

fn index_arches(index: &Value) -> Vec<String> {
    index
        .get("manifests")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(entry_arch).map(String::from).collect())
        .unwrap_or_default()
}

pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------- authentication ----------

#[derive(Clone)]
struct Credentials {
    user: String,
    secret: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Credentials({}, ***)", self.user)
    }
}

/// A parsed `WWW-Authenticate` header: scheme and its `key="value"` parameters.
#[derive(Debug, PartialEq, Eq)]
struct Challenge {
    scheme: String,
    params: BTreeMap<String, String>,
}

fn parse_challenge(header: &str) -> Option<Challenge> {
    let header = header.trim();
    let (scheme, rest) = header.split_once(' ').unwrap_or((header, ""));
    let mut params = BTreeMap::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=').collect();
        if key.is_empty() {
            break;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => value.extend(chars.next()),
                    '"' => break,
                    c => value.push(c),
                }
            }
        } else {
            value = chars.by_ref().take_while(|c| *c != ',').collect();
        }
        params.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    Some(Challenge {
        scheme: scheme.to_ascii_lowercase(),
        params,
    })
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Where credentials come from, first match wins:
/// 1. `DORKER_REGISTRY_TOKEN` (and `DORKER_REGISTRY_USER`),
/// 2. for ghcr.io, `GITHUB_TOKEN` / `GH_TOKEN` (GitHub Actions),
/// 3. `docker login` (`~/.docker/config.json`, including credential helpers),
/// 4. for ghcr.io, the GitHub CLI (`gh auth token`).
///
/// None of them means an anonymous pull, which works for public images.
fn credentials(registry: &str) -> Option<Credentials> {
    let ghcr = registry == "ghcr.io";
    if let Some(secret) = env_nonempty("DORKER_REGISTRY_TOKEN") {
        let user = env_nonempty("DORKER_REGISTRY_USER").unwrap_or_else(|| "dorker".into());
        crate::log::vlog!("registry credentials: DORKER_REGISTRY_TOKEN");
        return Some(Credentials { user, secret });
    }
    if ghcr {
        if let Some(secret) = env_nonempty("GITHUB_TOKEN").or_else(|| env_nonempty("GH_TOKEN")) {
            let user = env_nonempty("GITHUB_ACTOR").unwrap_or_else(|| "dorker".into());
            crate::log::vlog!("registry credentials: GITHUB_TOKEN/GH_TOKEN");
            return Some(Credentials { user, secret });
        }
    }
    if let Some(c) = docker_credentials(registry) {
        crate::log::vlog!("registry credentials: docker config");
        return Some(c);
    }
    if ghcr {
        if let Some(c) = gh_credentials() {
            crate::log::vlog!("registry credentials: gh auth token");
            return Some(c);
        }
    }
    None
}

fn docker_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DOCKER_CONFIG").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("config.json"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".docker").join("config.json"))
}

fn docker_credentials(registry: &str) -> Option<Credentials> {
    let text = fs::read_to_string(docker_config_path()?).ok()?;
    docker_credentials_from(&text, registry, &run_credential_helper)
}

/// Pure part of [`docker_credentials`], with the credential-helper call injected.
fn docker_credentials_from(
    config: &str,
    registry: &str,
    helper: &dyn Fn(&str, &str) -> Option<Credentials>,
) -> Option<Credentials> {
    let v: Value = serde_json::from_str(config).ok()?;
    let keys = [
        registry.to_string(),
        format!("https://{registry}"),
        format!("http://{registry}"),
    ];
    if let Some(h) = keys
        .iter()
        .find_map(|k| v.pointer(&format!("/credHelpers/{}", k.replace('/', "~1"))))
        .and_then(Value::as_str)
    {
        return helper(h, registry);
    }
    let auth = keys.iter().find_map(|k| {
        v.get("auths")?
            .get(k.as_str())?
            .get("auth")?
            .as_str()
            .map(String::from)
    });
    if let Some(auth) = auth {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(auth.trim())
            .ok()?;
        let (user, secret) = String::from_utf8(decoded)
            .ok()?
            .split_once(':')
            .map(|(u, s)| (u.to_string(), s.to_string()))?;
        return Some(Credentials { user, secret });
    }
    let store = v.get("credsStore").and_then(Value::as_str)?;
    helper(store, registry)
}

fn run_credential_helper(helper: &str, registry: &str) -> Option<Credentials> {
    if !helper
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return None;
    }
    let mut child = std::process::Command::new(format!("docker-credential-{helper}"))
        .arg("get")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(registry.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(Credentials {
        user: v.get("Username")?.as_str()?.to_string(),
        secret: v.get("Secret")?.as_str()?.to_string(),
    })
}

/// The GitHub login `gh` is signed in as, if any.
fn gh_login() -> Option<String> {
    std::process::Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

/// Where a local image could go on ghcr.io: same name and tag, under `owner`.
fn suggested_reference(image: &str, owner: Option<&str>) -> String {
    let (repo, tag) = images::split_tag(&images::normalize_tag(image).unwrap_or(image.into()));
    let name = repo.rsplit('/').next().unwrap_or(&repo).to_string();
    format!("ghcr.io/{}/{name}:{tag}", owner.unwrap_or("your-org"))
}

fn gh_credentials() -> Option<Credentials> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let secret = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if !out.status.success() || secret.is_empty() {
        return None;
    }
    let user = gh_login().unwrap_or_else(|| "dorker".into());
    Some(Credentials { user, secret })
}

fn auth_hint(r: &Reference, push: bool) -> String {
    let scope = if push {
        "write:packages"
    } else {
        "read:packages"
    };
    if r.registry == "ghcr.io" {
        format!(
            "Log in with one of:\n  gh auth refresh -s {scope}      (dorker uses `gh auth token` for ghcr.io)\n  docker login ghcr.io\n  export DORKER_REGISTRY_TOKEN=<a token with {scope}>"
        )
    } else {
        format!(
            "Log in with `docker login {}`, or set DORKER_REGISTRY_USER and DORKER_REGISTRY_TOKEN.",
            r.registry
        )
    }
}

// ---------- the registry client ----------

type Response = ureq::http::Response<ureq::Body>;

struct Client {
    agent: ureq::Agent,
    r: Reference,
    push: bool,
    /// Value of the `Authorization` header, once known.
    authorization: Option<String>,
}

impl Client {
    /// Connect and authenticate for pull (and push) on `r.repository`.
    fn connect(r: &Reference, push: bool) -> Result<Self> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .user_agent(concat!("dorker/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut c = Self {
            agent,
            r: r.clone(),
            push,
            authorization: None,
        };
        let ping = c
            .agent
            .get(format!("{}/v2/", r.base_url()))
            .call()
            .with_context(|| format!("connecting to {}", r.registry))?;
        if ping.status() == 401 {
            let header = ping
                .headers()
                .get("www-authenticate")
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .to_string();
            c.authorization = c.authenticate(&header)?;
        }
        Ok(c)
    }

    fn authenticate(&self, header: &str) -> Result<Option<String>> {
        let creds = credentials(&self.r.registry);
        let basic = creds.as_ref().map(|c| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{}:{}", c.user, c.secret))
            )
        });
        let Some(ch) = parse_challenge(header) else {
            return Ok(basic);
        };
        if ch.scheme == "basic" {
            return Ok(basic);
        }
        if ch.scheme != "bearer" {
            bail!(
                "{} asks for unsupported authentication {header:?}",
                self.r.registry
            );
        }
        let realm = ch
            .params
            .get("realm")
            .ok_or_else(|| anyhow!("{} sent no token realm", self.r.registry))?;
        let actions = if self.push { "pull,push" } else { "pull" };
        let mut url = format!(
            "{realm}{}scope={}",
            if realm.contains('?') { "&" } else { "?" },
            percent_encode(&format!("repository:{}:{actions}", self.r.repository))
        );
        if let Some(service) = ch.params.get("service") {
            url.push_str(&format!("&service={}", percent_encode(service)));
        }
        let mut req = self.agent.get(&url);
        if let Some(b) = &basic {
            req = req.header("Authorization", b);
        }
        let mut resp = req.call().context("requesting a registry token")?;
        if !resp.status().is_success() {
            let what = if creds.is_some() {
                "rejected the credentials"
            } else {
                "requires credentials"
            };
            bail!(
                "{} {what} (HTTP {})\n\n{}",
                self.r.registry,
                resp.status().as_u16(),
                auth_hint(&self.r, self.push)
            );
        }
        let v: Value = resp
            .body_mut()
            .read_json()
            .context("reading the registry token")?;
        let token = v
            .get("token")
            .or_else(|| v.get("access_token"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{} returned no token", self.r.registry))?;
        Ok(Some(format!("Bearer {token}")))
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v2/{}/{path}", self.r.base_url(), self.r.repository)
    }

    fn get(&self, url: &str, accept: Option<&str>) -> Result<Response> {
        let mut req = self.agent.get(url);
        if let Some(a) = &self.authorization {
            req = req.header("Authorization", a);
        }
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        req.call().with_context(|| format!("GET {url}"))
    }

    fn head(&self, url: &str) -> Result<Response> {
        let mut req = self.agent.head(url);
        if let Some(a) = &self.authorization {
            req = req.header("Authorization", a);
        }
        req.call().with_context(|| format!("HEAD {url}"))
    }

    /// Turn an unexpected status into an error that says what to do.
    fn fail(&self, mut resp: Response, what: &str) -> anyhow::Error {
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| {
                v.pointer("/errors/0/message")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .unwrap_or_else(|| body.trim().chars().take(300).collect());
        match status {
            401 | 403 => anyhow!(
                "{what}: {} denied access to {} (HTTP {status}: {detail})\n\n{}",
                self.r.registry,
                self.r.repository,
                auth_hint(&self.r, self.push)
            ),
            _ => anyhow!("{what}: HTTP {status} from {}: {detail}", self.r.registry),
        }
    }

    /// The manifest or index at `reference` (a tag or digest): `None` if there is none.
    fn manifest(&self, reference: &str) -> Result<Option<(Vec<u8>, Value)>> {
        let accept = [
            INDEX_MEDIA_TYPE,
            MANIFEST_MEDIA_TYPE,
            DOCKER_LIST_MEDIA_TYPE,
            "application/vnd.docker.distribution.manifest.v2+json",
        ]
        .join(", ");
        let mut resp = self.get(&self.url(&format!("manifests/{reference}")), Some(&accept))?;
        if resp.status() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(self.fail(resp, &format!("reading {}", self.r)));
        }
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_vec()?;
        let v: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} returned a manifest that is not JSON", self.r.registry))?;
        Ok(Some((bytes, v)))
    }

    fn put_manifest(&self, reference: &str, media_type: &str, bytes: &[u8]) -> Result<()> {
        let mut req = self
            .agent
            .put(self.url(&format!("manifests/{reference}")))
            .header("Content-Type", media_type);
        if let Some(a) = &self.authorization {
            req = req.header("Authorization", a);
        }
        let resp = req.send(bytes).context("uploading a manifest")?;
        if !resp.status().is_success() {
            return Err(self.fail(resp, "uploading a manifest"));
        }
        Ok(())
    }

    fn blob_exists(&self, digest: &str) -> Result<bool> {
        let resp = self.head(&self.url(&format!("blobs/{digest}")))?;
        Ok(resp.status().is_success())
    }

    /// Upload one blob (monolithic: start a session, then PUT the whole body).
    fn put_blob(&self, digest: &str, body: impl ureq::AsSendBody) -> Result<()> {
        let mut req = self.agent.post(self.url("blobs/uploads/"));
        if let Some(a) = &self.authorization {
            req = req.header("Authorization", a);
        }
        let resp = req.send_empty().context("starting an upload")?;
        if resp.status() != 202 {
            return Err(self.fail(resp, "starting an upload"));
        }
        let location = resp
            .headers()
            .get("location")
            .and_then(|h| h.to_str().ok())
            .ok_or_else(|| anyhow!("{} gave no upload location", self.r.registry))?;
        let mut url = if location.starts_with("http://") || location.starts_with("https://") {
            location.to_string()
        } else {
            format!("{}{location}", self.r.base_url())
        };
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&format!("digest={}", percent_encode(digest)));
        let mut req = self
            .agent
            .put(&url)
            .header("Content-Type", "application/octet-stream");
        if let Some(a) = &self.authorization {
            req = req.header("Authorization", a);
        }
        let resp = req.send(body).context("uploading")?;
        if resp.status() != 201 && !resp.status().is_success() {
            return Err(self.fail(resp, "uploading"));
        }
        Ok(())
    }

    fn get_blob(&self, digest: &str) -> Result<Response> {
        let resp = self.get(&self.url(&format!("blobs/{digest}")), None)?;
        if !resp.status().is_success() {
            return Err(self.fail(resp, &format!("downloading {digest}")));
        }
        Ok(resp)
    }

    fn get_small_blob(&self, digest: &str) -> Result<Vec<u8>> {
        let bytes = self
            .get_blob(digest)?
            .into_body()
            .into_with_config()
            .limit(4 * 1024 * 1024)
            .read_to_vec()?;
        if sha256_digest(&bytes) != digest {
            bail!(
                "{digest} from {} does not match its digest",
                self.r.registry
            );
        }
        Ok(bytes)
    }
}

// ---------- packing ----------

/// One compressed part on disk.
#[derive(Debug, Clone)]
pub struct Part {
    pub path: PathBuf,
    pub digest: String,
    pub size: u64,
}

/// Writes a byte stream into numbered part files of at most `max` bytes, hashing each.
struct SplitWriter {
    dir: PathBuf,
    max: u64,
    parts: Vec<Part>,
    current: Option<(File, Sha256, u64)>,
}

impl SplitWriter {
    fn new(dir: &Path, max: u64) -> Self {
        Self {
            dir: dir.to_path_buf(),
            max: max.max(1),
            parts: vec![],
            current: None,
        }
    }

    fn close_current(&mut self) -> io::Result<()> {
        if let Some((mut f, h, size)) = self.current.take() {
            f.flush()?;
            let n = self.parts.len();
            self.parts.push(Part {
                path: self.dir.join(format!("part-{n:04}")),
                digest: format!("sha256:{}", hex(&h.finalize())),
                size,
            });
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<Vec<Part>> {
        self.close_current()?;
        Ok(self.parts)
    }
}

impl Write for SplitWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.current.is_none() {
            let n = self.parts.len();
            let f = File::create(self.dir.join(format!("part-{n:04}")))?;
            self.current = Some((f, Sha256::new(), 0));
        }
        let max = self.max;
        let (f, h, size) = self.current.as_mut().expect("opened above");
        let room = (max - *size) as usize;
        let n = buf.len().min(room);
        f.write_all(&buf[..n])?;
        h.update(&buf[..n]);
        *size += n as u64;
        if *size >= max {
            self.close_current()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.current {
            Some((f, _, _)) => f.flush(),
            None => Ok(()),
        }
    }
}

/// Files never worth shipping: emulator lock files of the instance the image came from.
fn skip_file(name: &str) -> bool {
    name.ends_with(".lock")
}

/// Tar `src` (an image's AVD home), compress it with zstd and split it into parts in `out`.
/// Returns the parts and the unpacked size in bytes.
pub fn pack(src: &Path, out: &Path, part_size: u64) -> Result<(Vec<Part>, u64)> {
    let split = SplitWriter::new(out, part_size);
    let mut enc = zstd::stream::Encoder::new(split, 3)?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    // Multithreaded compression; ignore failure (a zstd built without threads).
    let _ = enc.multithread(threads);
    enc.include_checksum(true)?;
    let mut tar = tar::Builder::new(enc);
    tar.follow_symlinks(false);
    let mut unpacked = 0u64;
    add_dir(&mut tar, src, Path::new(""), &mut unpacked)?;
    let enc = tar.into_inner().context("finishing the archive")?;
    let split = enc.finish().context("finishing compression")?;
    let parts = split.finish()?;
    Ok((parts, unpacked))
}

fn add_dir<W: Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    rel: &Path,
    unpacked: &mut u64,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        let path = e.path();
        let rel_path = rel.join(&name);
        let ft = e.file_type()?;
        if ft.is_dir() {
            tar.append_dir(&rel_path, &path)?;
            add_dir(tar, &path, &rel_path, unpacked)?;
        } else if ft.is_file() {
            if skip_file(&name.to_string_lossy()) {
                continue;
            }
            *unpacked += e.metadata()?.len();
            tar.append_path_with_name(&path, &rel_path)
                .with_context(|| format!("adding {}", path.display()))?;
        } else {
            bail!(
                "{} is not a regular file or folder; dorker cannot push it",
                path.display()
            );
        }
    }
    Ok(())
}

/// Unpack a tar stream into `dest`, refusing anything but plain files and folders inside it.
/// Runs of zeros are written as holes, so sparse disk images stay sparse.
pub fn unpack<R: Read>(r: R, dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(r);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.into_owned();
        if rel.as_os_str().is_empty()
            || !rel.components().all(|c| matches!(c, Component::Normal(_)))
        {
            if rel.components().all(|c| matches!(c, Component::CurDir)) {
                continue;
            }
            bail!("refusing archive entry {}", rel.display());
        }
        let path = dest.join(&rel);
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
        let mtime = entry.header().mtime().unwrap_or(0);
        if kind.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        // Sparse files (holes recorded, not stored) read back with their holes as zeros.
        if !(kind.is_file() || kind.is_gnu_sparse()) {
            bail!("refusing archive entry {} ({kind:?})", rel.display());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        let len = write_sparse(&mut entry, &mut f)?;
        f.set_len(len)?;
        let _ = f.set_times(
            fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(mtime)),
        );
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o600))?;
        }
    }
    Ok(())
}

/// Copy `r` to `f`, seeking over 4 KiB blocks of zeros instead of writing them.
fn write_sparse<R: Read>(r: &mut R, f: &mut File) -> Result<u64> {
    const BLOCK: usize = 4096;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        // Fill the buffer (short reads would misalign blocks, which is harmless but slower).
        let mut filled = 0;
        while filled < buf.len() {
            let n = r.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            return Ok(total);
        }
        let data = &buf[..filled];
        let mut i = 0;
        while i < data.len() {
            let zero = |j: usize| data[j..(j + BLOCK).min(data.len())].iter().all(|b| *b == 0);
            let start = i;
            let is_zero = zero(i);
            while i < data.len() && zero(i) == is_zero {
                i = (i + BLOCK).min(data.len());
            }
            if is_zero {
                f.seek(SeekFrom::Current((i - start) as i64))?;
            } else {
                f.write_all(&data[start..i])?;
            }
        }
        total += filled as u64;
    }
}

/// Reads parts one after another from the registry, checking each one's digest at its end.
struct PartsReader<'a> {
    client: &'a Client,
    layers: Vec<(String, u64)>,
    next: usize,
    current: Option<(Box<dyn Read + 'a>, Sha256, String)>,
    on_part: &'a dyn Fn(usize, usize, u64),
}

impl Read for PartsReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.current.is_none() {
                let Some((digest, size)) = self.layers.get(self.next).cloned() else {
                    return Ok(0);
                };
                (self.on_part)(self.next + 1, self.layers.len(), size);
                self.next += 1;
                let resp = self.client.get_blob(&digest).map_err(io::Error::other)?;
                let reader = resp.into_body().into_with_config().limit(u64::MAX).reader();
                self.current = Some((Box::new(reader), Sha256::new(), digest));
            }
            let (r, h, digest) = self.current.as_mut().expect("set above");
            let n = r.read(buf)?;
            if n > 0 {
                h.update(&buf[..n]);
                return Ok(n);
            }
            let (_, h, digest) = (r, h.clone(), digest.clone());
            let got = format!("sha256:{}", hex(&h.finalize()));
            if got != digest {
                return Err(io::Error::other(format!(
                    "downloaded part does not match its digest (expected {digest}, got {got})"
                )));
            }
            self.current = None;
        }
    }
}

// ---------- push ----------

/// A folder of temporary files (a push's parts, a pull's partial image). It is removed when
/// dropped, and also on Ctrl-C or SIGTERM, so an interrupted transfer leaves nothing behind.
struct TempDir(PathBuf);

impl TempDir {
    fn create(path: PathBuf) -> Result<Self> {
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
        runtime::remove_on_interrupt(&path);
        Ok(Self(path))
    }

    /// Keep the folder (it was renamed into place).
    fn keep(self) {
        runtime::forget_on_interrupt(&self.0);
        std::mem::forget(self);
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        runtime::forget_on_interrupt(&self.0);
    }
}

/// Where a push packs its parts: `--tmp-dir`, else `DORKER_TMP`, else the config file's `tmp`,
/// else `<home>/tmp`. Pure apart from its inputs, so it is unit-tested.
fn push_tmp_root(
    flag: Option<&Path>,
    env: Option<&std::ffi::OsStr>,
    config: Option<&Path>,
    store_root: &Path,
) -> PathBuf {
    flag.map(Path::to_path_buf)
        .or_else(|| env.filter(|v| !v.is_empty()).map(PathBuf::from))
        .or_else(|| config.map(Path::to_path_buf))
        .unwrap_or_else(|| store_root.join("tmp"))
}

/// Remove `push-<id>-<pid>` folders left by pushes that were killed (`kill -9`, a crash).
fn remove_stale_push_dirs(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix("push-")
            .and_then(|rest| rest.rsplit_once('-'))
            .and_then(|(_, pid)| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !crate::store::pid_alive(pid) {
            crate::log::vlog!("removing a stale push folder {}", e.path().display());
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// `dorker push <image> [<reference>]`.
pub fn push(
    image: &str,
    target: Option<&str>,
    tmp_dir: Option<&Path>,
    json_out: bool,
) -> Result<i32> {
    let cfg = errors::with(errors::CONFIG);
    let target = target.unwrap_or(image);
    let r = Reference::parse(target).map_err(cfg)?.ok_or_else(|| {
        cfg(anyhow!(
            "{target:?} is not a registry reference\n\nName where to push it, e.g.:\n  dorker push {image} {}",
            suggested_reference(image, gh_login().as_deref())
        ))
    })?;
    let store = Store::open()?;
    let mut meta = images::find(&store, image).map_err(cfg)?;
    let st = Style::stderr();
    let arch = arch_of_abi(&meta.abi).to_string();
    let step = |n: usize, text: &str| eprintln!("{} {text}", st.cyan(&format!("[{n}/4]")));

    let avd = images::image_dir(&store, &meta.id).join("avd");
    let (_, allocated) = images::dir_usage(&avd);
    let tmp_root = push_tmp_root(
        tmp_dir,
        std::env::var_os("DORKER_TMP").as_deref(),
        crate::config::current().tmp.as_deref(),
        &store.root(),
    );
    fs::create_dir_all(&tmp_root)
        .with_context(|| format!("creating the push folder {}", tmp_root.display()))?;
    remove_stale_push_dirs(&tmp_root);
    runtime::ensure_free_space(&tmp_root, allocated, "pack the image for upload")?;
    crate::log::vlog!("packing into {}", tmp_root.display());
    let tmp = TempDir::create(tmp_root.join(format!("push-{}-{}", meta.id, std::process::id())))?;

    step(
        1,
        &format!(
            "Packing image {} ({}, {arch}, {})",
            meta.id,
            meta.system_image,
            images::human(allocated)
        ),
    );
    let (parts, unpacked) = pack(&avd, &tmp.0, PART_SIZE)?;
    let compressed: u64 = parts.iter().map(|p| p.size).sum();

    step(2, &format!("Connecting to {}", r.registry));
    let client = Client::connect(&r, true)?;

    step(
        3,
        &format!(
            "Uploading {} in {} part(s)",
            images::human(compressed),
            parts.len()
        ),
    );
    for (i, p) in parts.iter().enumerate() {
        let label = format!(
            "  part {}/{} ({})",
            i + 1,
            parts.len(),
            images::human(p.size)
        );
        if client.blob_exists(&p.digest)? {
            eprintln!("{label}: already in the registry");
            continue;
        }
        eprintln!("{label}");
        let f = File::open(&p.path)?;
        client.put_blob(&p.digest, f)?;
    }
    let mut shipped = meta.clone();
    shipped.tags = vec![];
    shipped.last_used = None;
    shipped.source = None;
    let config = serde_json::to_vec_pretty(&shipped)?;
    let config_digest = sha256_digest(&config);
    if !client.blob_exists(&config_digest)? {
        client.put_blob(&config_digest, config.as_slice())?;
    }
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": parts.iter().map(|p| json!({
            "mediaType": PART_MEDIA_TYPE,
            "digest": p.digest,
            "size": p.size,
        })).collect::<Vec<_>>(),
        "annotations": {
            UNPACKED_SIZE_KEY: unpacked.to_string(),
            "dev.dorker.system-image": meta.system_image,
            "dev.dorker.emulator": meta.emulator_version,
        },
    });
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest = sha256_digest(&manifest_bytes);
    client.put_manifest(&manifest_digest, MANIFEST_MEDIA_TYPE, &manifest_bytes)?;

    step(4, &format!("Tagging {r} ({arch})"));
    let entry = json!({
        "mediaType": MANIFEST_MEDIA_TYPE,
        "digest": manifest_digest,
        "size": manifest_bytes.len(),
        "platform": { "os": "android", "architecture": arch },
    });
    // Two machines (say a Mac and Linux CI) may push their architecture at the same time:
    // read, merge, write, then check that ours survived, and try again if it did not.
    let mut tagged = false;
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500 + 700 * attempt));
        }
        let existing = client.manifest(&r.tag)?.map(|(_, v)| v);
        let index = merge_index(existing.as_ref(), entry.clone(), &arch);
        client.put_manifest(&r.tag, INDEX_MEDIA_TYPE, &serde_json::to_vec(&index)?)?;
        let now = client.manifest(&r.tag)?.map(|(_, v)| v);
        if now
            .as_ref()
            .and_then(|v| select_from_index(v, &arch))
            .and_then(|d| d.get("digest"))
            .and_then(Value::as_str)
            == Some(manifest_digest.as_str())
        {
            tagged = true;
            break;
        }
    }
    if !tagged {
        bail!("another push kept replacing {r}; try again");
    }
    images::apply_tag(&store, &mut meta, &r.local_tag())?;

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "reference": r.local_tag(),
                "architecture": arch,
                "digest": manifest_digest,
                "image": meta.id,
                "compressed_bytes": compressed,
                "unpacked_bytes": unpacked,
            }))?
        );
    } else {
        println!("{r}@{manifest_digest}");
    }
    eprintln!(
        "Pushed {} as {r} ({arch}). Teammates on the same architecture run it with:\n  dorker run {r}",
        meta.id
    );
    Ok(0)
}

// ---------- pull ----------

/// `dorker pull <reference>`: download the image for this machine's architecture.
pub fn pull_cmd(r: &Reference) -> Result<i32> {
    let id = pull(r)?;
    println!("{id}");
    Ok(0)
}

/// Download `r` into the image store (if it is not there already) and tag it `r`.
/// Returns the local image id.
pub fn pull(r: &Reference) -> Result<String> {
    let store = Store::open()?;
    let st = Style::stderr();
    let arch = arch_of_abi(sdk::host_abi()).to_string();

    eprintln!("Pulling {r} ({arch})");
    let client = Client::connect(r, false)?;
    let (bytes, top) = client.manifest(&r.tag)?.ok_or_else(|| {
        errors::with(errors::CONFIG)(anyhow!(
            "{r} not found\n\nCheck the name and tag, and that you can read the package."
        ))
    })?;
    let (manifest_digest, manifest) = if is_index(&top) {
        let d = select_from_index(&top, &arch).ok_or_else(|| {
            let have = index_arches(&top);
            errors::with(errors::CONFIG)(anyhow!(
                "{r} has no image for {arch} (this machine); it has: {}\n\nBuild the same Dorkerfile on a{} machine and `dorker push` it to {r}.",
                if have.is_empty() { "none".into() } else { have.join(", ") },
                if arch == "arm64" { "n Apple Silicon" } else { "n x86_64 Linux" },
            ))
        })?;
        let digest = d
            .get("digest")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{r}: index entry without a digest"))?
            .to_string();
        let (b, v) = client
            .manifest(&digest)?
            .ok_or_else(|| anyhow!("{r}: manifest {digest} is missing"))?;
        if sha256_digest(&b) != digest {
            bail!("{r}: manifest {digest} does not match its digest");
        }
        (digest, v)
    } else {
        (sha256_digest(&bytes), top)
    };
    if manifest
        .pointer("/config/mediaType")
        .and_then(Value::as_str)
        != Some(CONFIG_MEDIA_TYPE)
    {
        bail!("{r} is not a dorker image (pushed by something else?)");
    }
    let id = manifest_digest
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect::<String>();
    let source = format!("{r}@{manifest_digest}");

    if let Ok(mut meta) = images::find(&store, &id).filter_id(&id) {
        images::apply_tag(&store, &mut meta, &r.local_tag())?;
        eprintln!("{r} is up to date (image {id})");
        return Ok(id);
    }

    let config_digest = manifest
        .pointer("/config/digest")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{r}: manifest without a config"))?;
    let mut meta: ImageMeta = serde_json::from_slice(&client.get_small_blob(config_digest)?)
        .with_context(|| format!("{r}: unreadable image config"))?;
    if arch_of_abi(&meta.abi) != arch {
        bail!(
            "{r} is a {} image; this machine needs {arch}",
            arch_of_abi(&meta.abi)
        );
    }
    let layers: Vec<(String, u64)> = manifest
        .get("layers")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("{r}: manifest without layers"))?
        .iter()
        .map(|l| {
            let digest = l.get("digest").and_then(Value::as_str);
            let size = l.get("size").and_then(Value::as_u64).unwrap_or(0);
            match (digest, l.get("mediaType").and_then(Value::as_str)) {
                (Some(d), Some(PART_MEDIA_TYPE)) if d.starts_with("sha256:") => {
                    Ok((d.to_string(), size))
                }
                _ => Err(anyhow!("{r}: unexpected layer {l}")),
            }
        })
        .collect::<Result<_>>()?;
    let compressed: u64 = layers.iter().map(|l| l.1).sum();
    let unpacked = manifest
        .pointer(&format!(
            "/annotations/{}",
            UNPACKED_SIZE_KEY.replace('/', "~1")
        ))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    runtime::ensure_free_space(&store.root(), unpacked, "pull the image")?;

    eprintln!(
        "Downloading {} in {} part(s), {} unpacked",
        images::human(compressed),
        layers.len(),
        images::human(unpacked)
    );
    let partial = store.images_dir().join(format!(".{id}.partial"));
    let guard = TempDir::create(partial.clone())?;
    let progress = |i: usize, n: usize, size: u64| {
        eprintln!("  part {i}/{n} ({})", images::human(size));
    };
    let reader = PartsReader {
        client: &client,
        layers,
        next: 0,
        current: None,
        on_part: &progress,
    };
    let avd = partial.join("avd");
    let mut dec = zstd::stream::Decoder::new(reader)?;
    unpack(&mut dec, &avd).with_context(|| format!("unpacking {r}"))?;
    // Read to the very end, so every part's digest is checked (tar stops at its end marker).
    io::copy(&mut dec.finish(), &mut io::sink())?;

    crate::commit::remove_locks(&avd);
    runtime::write_avd_ini(&avd, meta.api)?;
    meta.id = id.clone();
    meta.tags = vec![];
    meta.last_used = None;
    meta.source = Some(source);
    fs::write(
        partial.join("image.json"),
        serde_json::to_vec_pretty(&meta)?,
    )?;
    let final_dir = images::image_dir(&store, &id);
    fs::rename(&partial, &final_dir).context("publishing the image")?;
    guard.keep();
    images::apply_tag(&store, &mut meta, &r.local_tag())?;
    eprintln!("{} {r} is image {id}", st.green("Pulled"));
    compatibility_notes(&meta);
    Ok(id)
}

/// Say up front when this machine will cold boot the image instead of restoring its snapshot.
fn compatibility_notes(meta: &ImageMeta) {
    let Ok(sdk) = Sdk::locate() else {
        return;
    };
    let Some(sys) = sdk.find_package(&meta.system_image) else {
        eprintln!(
            "note: it needs {}, which is not installed; `dorker run` offers to install it",
            meta.system_image
        );
        return;
    };
    if meta.snapshot.is_none() {
        return;
    }
    let rev = sdk.system_image_revision(&sys).unwrap_or_default();
    let emu = sdk.emulator_version().unwrap_or_default();
    if rev != meta.system_image_revision {
        eprintln!(
            "note: it was built on system image {} (you have {rev}); instances will cold boot",
            meta.system_image_revision
        );
    } else if emu != meta.emulator_version {
        eprintln!(
            "note: it was built with emulator {} (you have {emu}); instances will cold boot \
             instead of restoring its snapshot. Update the emulator to match for fast starts.",
            meta.emulator_version
        );
    }
}

/// `images::find` matches id prefixes; a pulled image must match its exact id.
trait FilterId {
    fn filter_id(self, id: &str) -> Result<ImageMeta>;
}

impl FilterId for Result<ImageMeta> {
    fn filter_id(self, id: &str) -> Result<ImageMeta> {
        self.and_then(|m| {
            if m.id == id {
                Ok(m)
            } else {
                bail!("not this image")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Dir;

    #[test]
    fn references() {
        let r = Reference::parse("ghcr.io/acme/gearhead:18.0")
            .unwrap()
            .unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "acme/gearhead");
        assert_eq!(r.tag, "18.0");
        assert_eq!(r.base_url(), "https://ghcr.io");
        let r = Reference::parse("localhost:5000/team/aa").unwrap().unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.tag, "latest");
        assert_eq!(r.base_url(), "http://localhost:5000");
        assert_eq!(r.local_tag(), "localhost:5000/team/aa:latest");
        // Local names are not registry references.
        assert_eq!(Reference::parse("myapp:ready").unwrap(), None);
        assert_eq!(Reference::parse("team/myapp:ready").unwrap(), None);
        assert_eq!(Reference::parse("android:35").unwrap(), None);
        assert!(Reference::parse("ghcr.io/Acme/x").is_err());
        assert!(Reference::parse("ghcr.io/acme/x@sha256:00").is_err());
        assert!(Reference::parse("ghcr.io/").is_err());
    }

    #[test]
    fn local_tags_of_references_are_valid_image_names() {
        for s in ["ghcr.io/acme/gearhead:18.0", "localhost:5000/team/aa"] {
            let r = Reference::parse(s).unwrap().unwrap();
            assert_eq!(
                images::normalize_tag(&r.local_tag()).unwrap(),
                r.local_tag()
            );
        }
    }

    #[test]
    fn push_folder_precedence() {
        let store = Path::new("/store");
        let flag = Path::new("/flag");
        let conf = Path::new("/conf");
        let env = std::ffi::OsStr::new("/env");
        assert_eq!(
            push_tmp_root(None, None, None, store),
            Path::new("/store/tmp")
        );
        assert_eq!(push_tmp_root(None, None, Some(conf), store), conf);
        assert_eq!(
            push_tmp_root(None, Some(env), Some(conf), store),
            Path::new("/env")
        );
        assert_eq!(
            push_tmp_root(Some(flag), Some(env), Some(conf), store),
            flag
        );
        // An empty DORKER_TMP is ignored.
        let empty = std::ffi::OsStr::new("");
        assert_eq!(push_tmp_root(None, Some(empty), Some(conf), store), conf);
    }

    #[test]
    fn stale_push_folders_of_dead_processes_are_removed() {
        let d = Dir::new();
        // A pid that cannot be running (above the usual pid_max), our own, and an unrelated folder.
        let dead = d.path().join("push-abc-4294967");
        let ours = d.path().join(format!("push-abc-{}", std::process::id()));
        let other = d.path().join("keep-me");
        for p in [&dead, &ours, &other] {
            fs::create_dir_all(p).unwrap();
        }
        remove_stale_push_dirs(d.path());
        assert!(!dead.exists());
        assert!(ours.exists());
        assert!(other.exists());
    }

    #[test]
    fn push_suggestions_keep_the_name_and_tag() {
        assert_eq!(
            suggested_reference("gearhead:37-dev", Some("cesar")),
            "ghcr.io/cesar/gearhead:37-dev"
        );
        assert_eq!(
            suggested_reference("team/app", None),
            "ghcr.io/your-org/app:latest"
        );
    }

    #[test]
    fn abis_map_to_oci_architectures() {
        assert_eq!(arch_of_abi("arm64-v8a"), "arm64");
        assert_eq!(arch_of_abi("x86_64"), "amd64");
    }

    fn entry(arch: &str, digest: &str) -> Value {
        json!({"mediaType": MANIFEST_MEDIA_TYPE, "digest": digest, "size": 1,
               "platform": {"os": "android", "architecture": arch}})
    }

    #[test]
    fn index_merge_keeps_other_architectures_and_replaces_ours() {
        let first = merge_index(None, entry("amd64", "sha256:a"), "amd64");
        let both = merge_index(Some(&first), entry("arm64", "sha256:b"), "arm64");
        assert_eq!(index_arches(&both), vec!["amd64", "arm64"]);
        let again = merge_index(Some(&both), entry("amd64", "sha256:c"), "amd64");
        assert_eq!(index_arches(&again), vec!["amd64", "arm64"]);
        assert_eq!(
            select_from_index(&again, "amd64").unwrap()["digest"],
            "sha256:c"
        );
        assert_eq!(
            select_from_index(&again, "arm64").unwrap()["digest"],
            "sha256:b"
        );
        assert!(select_from_index(&again, "386").is_none());
        // A plain manifest at the tag is replaced, not merged.
        let plain = json!({"schemaVersion": 2, "mediaType": MANIFEST_MEDIA_TYPE, "layers": []});
        let replaced = merge_index(Some(&plain), entry("arm64", "sha256:d"), "arm64");
        assert_eq!(index_arches(&replaced), vec!["arm64"]);
        assert!(is_index(&replaced));
    }

    #[test]
    fn challenges() {
        let c = parse_challenge(
            r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:a/b:pull""#,
        )
        .unwrap();
        assert_eq!(c.scheme, "bearer");
        assert_eq!(c.params["realm"], "https://ghcr.io/token");
        assert_eq!(c.params["service"], "ghcr.io");
        assert_eq!(c.params["scope"], "repository:a/b:pull");
        let c = parse_challenge(r#"Basic realm="Registry Realm""#).unwrap();
        assert_eq!(c.scheme, "basic");
        assert_eq!(c.params["realm"], "Registry Realm");
        assert_eq!(
            percent_encode("repository:a/b:pull,push"),
            "repository%3Aa%2Fb%3Apull%2Cpush"
        );
    }

    #[test]
    fn docker_config_credentials() {
        let none = |_: &str, _: &str| None;
        let auth = base64::engine::general_purpose::STANDARD.encode("me:secret");
        let cfg = format!(r#"{{"auths": {{"ghcr.io": {{"auth": "{auth}"}}}}}}"#);
        let c = docker_credentials_from(&cfg, "ghcr.io", &none).unwrap();
        assert_eq!((c.user.as_str(), c.secret.as_str()), ("me", "secret"));
        assert!(docker_credentials_from(&cfg, "other.io", &none).is_none());
        // A credential helper for the registry wins; credsStore is the fallback.
        let helper = |h: &str, reg: &str| {
            Some(Credentials {
                user: h.into(),
                secret: reg.into(),
            })
        };
        let cfg = r#"{"credHelpers": {"ghcr.io": "gh"}, "credsStore": "osxkeychain"}"#;
        assert_eq!(
            docker_credentials_from(cfg, "ghcr.io", &helper)
                .unwrap()
                .user,
            "gh"
        );
        assert_eq!(
            docker_credentials_from(cfg, "x.io", &helper).unwrap().user,
            "osxkeychain"
        );
    }

    #[test]
    fn pack_and_unpack_round_trip_across_parts() {
        let d = Dir::new();
        let src = d.path().join("avd");
        fs::create_dir_all(src.join("dorker.avd/snapshots/ready")).unwrap();
        fs::write(src.join("dorker.ini"), "path=/old\n").unwrap();
        // Incompressible bytes (xorshift), so the compressed stream spans several small parts.
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let noisy: Vec<u8> = (0..300_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        fs::write(src.join("dorker.avd/snapshots/ready/ram.bin"), &noisy).unwrap();
        let mut sparse = vec![0u8; 64 * 1024];
        sparse[10_000] = 7;
        sparse.extend(vec![0u8; 20_000]);
        fs::write(src.join("dorker.avd/userdata.img"), &sparse).unwrap();
        fs::write(src.join("dorker.avd/multiinstance.lock"), "x").unwrap();
        // A truly sparse file (a hole on disk), which tar stores as a GNU sparse entry.
        {
            let mut f = File::create(src.join("dorker.avd/hole.img")).unwrap();
            f.set_len(8 * 1024 * 1024).unwrap();
            f.seek(SeekFrom::Start(5 * 1024 * 1024)).unwrap();
            f.write_all(b"data in the middle").unwrap();
        }

        let out = d.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let (parts, unpacked) = pack(&src, &out, 64 * 1024).unwrap();
        assert!(
            parts.len() > 1,
            "expected several parts, got {}",
            parts.len()
        );
        assert!(parts.iter().all(|p| p.size <= 64 * 1024));
        assert_eq!(
            unpacked,
            (noisy.len() + sparse.len() + "path=/old\n".len() + 8 * 1024 * 1024) as u64
        );
        for p in &parts {
            assert_eq!(sha256_digest(&fs::read(&p.path).unwrap()), p.digest);
        }

        let mut stream = Vec::new();
        for p in &parts {
            stream.extend(fs::read(&p.path).unwrap());
        }
        let dest = d.path().join("dest");
        unpack(
            zstd::stream::Decoder::new(stream.as_slice()).unwrap(),
            &dest,
        )
        .unwrap();
        assert_eq!(
            fs::read(dest.join("dorker.avd/snapshots/ready/ram.bin")).unwrap(),
            noisy
        );
        assert_eq!(
            fs::read(dest.join("dorker.avd/userdata.img")).unwrap(),
            sparse
        );
        assert!(!dest.join("dorker.avd/multiinstance.lock").exists());
        let hole = fs::read(dest.join("dorker.avd/hole.img")).unwrap();
        assert_eq!(hole.len(), 8 * 1024 * 1024);
        assert_eq!(&hole[5 * 1024 * 1024..][..18], b"data in the middle");
        assert!(hole[..5 * 1024 * 1024].iter().all(|b| *b == 0));
    }

    #[test]
    fn unpack_refuses_paths_outside_the_destination() {
        let mut data = Vec::new();
        {
            let mut b = tar::Builder::new(&mut data);
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            // `append_data` validates paths, so write the raw name.
            h.as_old_mut().name[..9].copy_from_slice(b"../escape");
            h.set_cksum();
            b.append(&h, &b"x"[..]).unwrap();
            b.finish().unwrap();
        }
        let d = Dir::new();
        let err = unpack(data.as_slice(), &d.path().join("dest")).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert!(!d.path().join("escape").exists());
    }
}
