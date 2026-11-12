//! Firmware patch manifests and safe image patching.
//!
//! Mirrors the dde4-toolkit's `patches` module so both toolkits behave the
//! same way: a patch is a JSON manifest with explicit `before`/`after` bytes.
//! The image is never patched blindly — every changed range must match the
//! expected `before` bytes — and applying or reverting then corrects the flash
//! checksums (both TDI V3.1 slots and the cold-start program chain; see
//! `crate::checksum`).
//!
//! The four manifests shipped in `patches/` are compiled in, so the CLI and
//! GUI work from a fresh clone. `DDE3_PATCH_DIR` and a `patches/` directory
//! next to the binary (or the working directory) add more; a file with the
//! same name as a built-in manifest overrides it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::checksum::IMAGE_SIZE;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchState {
    Available,
    Applied,
    Conflict(String),
    Invalid(String),
}

impl PatchState {
    pub fn label(&self) -> &str {
        match self {
            PatchState::Available => "available",
            PatchState::Applied => "applied",
            PatchState::Conflict(_) => "conflict",
            PatchState::Invalid(_) => "invalid",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            PatchState::Conflict(s) | PatchState::Invalid(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Change {
    pub what: String,
    pub offset: usize,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchSource {
    Builtin,
    File(PathBuf),
}

impl PatchSource {
    pub fn label(&self) -> String {
        match self {
            PatchSource::Builtin => "built in".into(),
            PatchSource::File(p) => p.display().to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Patch {
    pub name: String,
    pub version: u32,
    pub description: String,
    pub verified: String,
    pub hardware_tested: bool,
    /// File name of the flashable image the builder writes (`output.path`).
    pub output: Option<String>,
    pub target_sha256: Option<String>,
    pub checksum: String,
    pub checksum_note: String,
    pub sectors: Vec<u32>,
    pub changes: Vec<Change>,
    pub source: PatchSource,
}

#[derive(Deserialize)]
struct RawManifest {
    #[serde(alias = "patch")]
    name: Option<String>,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    description: String,
    #[serde(default)]
    verified: String,
    #[serde(default)]
    hardware_tested: bool,
    input: Option<RawInput>,
    output: Option<RawOutput>,
    #[serde(default)]
    changes: Vec<RawChange>,
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    checksum_note: String,
    #[serde(default)]
    sectors_touched: Vec<u32>,
}

#[derive(Deserialize)]
struct RawInput {
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct RawOutput {
    path: Option<String>,
}

#[derive(Deserialize)]
struct RawChange {
    #[serde(default)]
    what: Option<String>,
    file_offset: String,
    #[serde(default)]
    before: String,
    after: String,
    #[serde(default)]
    length: Option<usize>,
}

impl Patch {
    pub fn parse(json: &str, source: PatchSource) -> Result<Self> {
        let raw: RawManifest = serde_json::from_str(json).context("invalid patch JSON")?;
        if raw.changes.is_empty() {
            bail!("manifest has no changes");
        }
        let mut changes = Vec::with_capacity(raw.changes.len());
        for (i, c) in raw.changes.iter().enumerate() {
            let offset = parse_offset(&c.file_offset)
                .with_context(|| format!("change {i}: bad file_offset {:?}", c.file_offset))?;
            let before = parse_bytes(&c.before)
                .with_context(|| format!("change {i}: bad before bytes"))?;
            let after =
                parse_bytes(&c.after).with_context(|| format!("change {i}: bad after bytes"))?;
            if before.len() != after.len() {
                bail!(
                    "change {i}: before is {} bytes, after is {} bytes",
                    before.len(),
                    after.len()
                );
            }
            if after.is_empty() {
                bail!("change {i}: empty change");
            }
            if let Some(n) = c.length {
                if n != after.len() {
                    bail!("change {i}: length {n} but after is {} bytes", after.len());
                }
            }
            if offset + after.len() > IMAGE_SIZE {
                bail!("change {i}: range 0x{offset:X} is outside the 256 KB image");
            }
            changes.push(Change {
                what: c.what.clone().unwrap_or_else(|| format!("change {i}")),
                offset,
                before,
                after,
            });
        }
        let name = raw.name.unwrap_or_else(|| "unnamed patch".into());
        Ok(Self {
            name,
            version: raw.version,
            description: raw.description,
            verified: raw.verified,
            hardware_tested: raw.hardware_tested,
            output: raw.output.and_then(|o| o.path),
            target_sha256: raw.input.and_then(|i| i.sha256),
            checksum: raw.checksum,
            checksum_note: raw.checksum_note,
            sectors: raw.sectors_touched,
            changes,
            source,
        })
    }

    pub fn state(&self, image: &[u8]) -> PatchState {
        if image.len() != IMAGE_SIZE {
            return PatchState::Invalid("a patch needs a 256 KB full image".into());
        }
        let mut before = 0usize;
        let mut after = 0usize;
        for c in &self.changes {
            if c.offset + c.after.len() > image.len() {
                return PatchState::Invalid(format!(
                    "{} at 0x{:X} is outside the image",
                    c.what, c.offset
                ));
            }
            if image[c.offset..c.offset + c.after.len()] == c.after {
                after += 1;
            } else if image[c.offset..c.offset + c.before.len()] == c.before {
                before += 1;
            } else {
                return PatchState::Conflict(format!(
                    "{} at 0x{:X}: neither the expected original nor patched bytes are present",
                    c.what, c.offset
                ));
            }
        }
        if after == self.changes.len() {
            PatchState::Applied
        } else if before == self.changes.len() {
            PatchState::Available
        } else {
            PatchState::Conflict(format!(
                "partially applied: {after} patched, {before} original, {} total",
                self.changes.len()
            ))
        }
    }

    /// Does the loaded image hash match the manifest's `input.sha256`?
    /// Manifests without that field match any image.
    pub fn target_matches(&self, image: &[u8]) -> bool {
        match &self.target_sha256 {
            Some(h) => sha256(image) == *h,
            None => true,
        }
    }

    /// Apply to a full image and correct the flash checksums (TDI code +
    /// calibration, then the cold-start program chain). The image is only
    /// modified when every `before` range matches.
    pub fn apply(&self, image: &mut Vec<u8>) -> Result<()> {
        match self.state(image) {
            PatchState::Applied => return Ok(()),
            PatchState::Available => {}
            PatchState::Conflict(s) => bail!("cannot apply {}: {s}", self.name),
            PatchState::Invalid(s) => bail!("cannot apply {}: {s}", self.name),
        }
        for c in &self.changes {
            image[c.offset..c.offset + c.after.len()].copy_from_slice(&c.after);
        }
        *image = crate::checksum::correct(image);
        Ok(())
    }

    /// Revert to the original bytes and correct the flash checksums.
    pub fn revert(&self, image: &mut Vec<u8>) -> Result<()> {
        match self.state(image) {
            PatchState::Available => return Ok(()),
            PatchState::Applied => {}
            PatchState::Conflict(s) => bail!("cannot revert {}: {s}", self.name),
            PatchState::Invalid(s) => bail!("cannot revert {}: {s}", self.name),
        }
        for c in &self.changes {
            image[c.offset..c.offset + c.before.len()].copy_from_slice(&c.before);
        }
        *image = crate::checksum::correct(image);
        Ok(())
    }

    pub fn changed_bytes(&self) -> usize {
        self.changes.iter().map(|c| c.after.len()).sum()
    }
}

pub fn sha256(image: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(image);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_offset(s: &str) -> Result<usize> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    usize::from_str_radix(t, 16).with_context(|| format!("not a hex offset: {s}"))
}

fn parse_bytes(s: &str) -> Result<Vec<u8>> {
    let t: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '_')
        .collect();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(&t);
    if !t.len().is_multiple_of(2) {
        bail!("odd number of hex digits");
    }
    let mut out = Vec::with_capacity(t.len() / 2);
    for i in (0..t.len()).step_by(2) {
        out.push(u8::from_str_radix(&t[i..i + 2], 16)?);
    }
    Ok(out)
}

/// The manifests shipped in `patches/`, compiled in so the CLI and GUI work
/// from a fresh clone (and on a machine without the analysis tree).
const BUILTIN_MANIFESTS: [&str; 4] = [
    include_str!("../patches/e46_baud125000.json"),
    include_str!("../patches/e46_baud38400.json"),
    include_str!("../patches/e46_immo_off_gate.json"),
    include_str!("../patches/e46_immo_off_full.json"),
];

pub fn builtins() -> (Vec<Patch>, Vec<String>) {
    let mut patches = Vec::new();
    let mut warnings = Vec::new();
    for json in BUILTIN_MANIFESTS {
        match Patch::parse(json, PatchSource::Builtin) {
            Ok(p) => patches.push(p),
            Err(e) => warnings.push(format!("built-in manifest: {e}")),
        }
    }
    (patches, warnings)
}

fn external_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(p) = std::env::var("DDE3_PATCH_DIR") {
        dirs.push(PathBuf::from(p));
    }
    dirs.push(PathBuf::from("patches"));
    dirs.push(PathBuf::from("../patches"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("patches"));
        }
    }
    dirs
}

/// The built-in manifests plus every valid JSON manifest from the patch
/// directories. Invalid manifests are returned as warnings, not fatal errors.
pub fn load_all() -> (Vec<Patch>, Vec<String>) {
    let (mut patches, mut warnings) = builtins();
    let mut seen_dirs = Vec::new();
    for dir in external_dirs() {
        let Ok(canon) = std::fs::canonicalize(&dir) else {
            continue;
        };
        if seen_dirs.contains(&canon) {
            continue;
        }
        seen_dirs.push(canon.clone());
        let Ok(entries) = std::fs::read_dir(&canon) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match Patch::parse(&text, PatchSource::File(path.clone())) {
                Ok(p) => {
                    if let Some(existing) = patches.iter_mut().find(|x| x.name == p.name) {
                        *existing = p;
                    } else {
                        patches.push(p);
                    }
                }
                Err(e) => warnings.push(format!("{}: {e}", path.display())),
            }
        }
    }
    patches.sort_by(|a, b| a.name.cmp(&b.name));
    (patches, warnings)
}

/// Load one manifest chosen by the user.
pub fn load_file(path: &Path) -> Result<Patch> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    Patch::parse(&text, PatchSource::File(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> String {
        r#"{
            "patch": "test_patch",
            "version": 1,
            "description": "unit test",
            "input": {"sha256": "00"},
            "changes": [
                {"what": "one", "file_offset": "0x100", "length": 2,
                 "before": "0000", "after": "aabb"}
            ],
            "checksum": "auto"
        }"#
        .into()
    }

    fn patches_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("patches")
    }

    #[test]
    fn apply_and_revert_round_trip() {
        let p = Patch::parse(&manifest(), PatchSource::Builtin).unwrap();
        let mut img = vec![0u8; IMAGE_SIZE];
        assert_eq!(p.state(&img), PatchState::Available);
        p.apply(&mut img).unwrap();
        assert_eq!(p.state(&img), PatchState::Applied);
        assert_eq!(&img[0x100..0x102], &[0xAA, 0xBB]);
        p.revert(&mut img).unwrap();
        assert_eq!(p.state(&img), PatchState::Available);
        assert_eq!(&img[0x100..0x102], &[0x00, 0x00]);
    }

    #[test]
    fn conflict_is_refused() {
        let p = Patch::parse(&manifest(), PatchSource::Builtin).unwrap();
        let mut img = vec![0u8; IMAGE_SIZE];
        img[0x100] = 0x11;
        assert!(matches!(p.state(&img), PatchState::Conflict(_)));
        assert!(p.apply(&mut img).is_err());
    }

    #[test]
    fn partial_calibration_image_is_invalid() {
        let p = Patch::parse(&manifest(), PatchSource::Builtin).unwrap();
        let img = vec![0u8; 0xC000];
        assert!(matches!(p.state(&img), PatchState::Invalid(_)));
    }

    #[test]
    fn builtin_manifests_parse() {
        let (patches, warnings) = builtins();
        assert!(
            warnings.is_empty(),
            "built-in manifests must parse: {warnings:?}"
        );
        let mut names: Vec<&str> = patches.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "edc15m_baud_0x91_125000",
                "edc15m_baud_0x91_38400",
                "edc15m_immo_off_full",
                "edc15m_immo_off_gate",
            ]
        );
        let outputs: Vec<&str> = patches
            .iter()
            .filter_map(|p| p.output.as_deref())
            .collect();
        assert_eq!(outputs.len(), 4, "every manifest names its output image");
    }

    /// Every shipped manifest, applied to the shipped reference image, must
    /// reproduce the shipped output image byte for byte — checksums included.
    #[test]
    fn builtin_patches_reproduce_the_shipped_images() {
        let dir = patches_dir();
        let orig = std::fs::read(dir.join("e46_orig.bin")).expect("reference image");
        let (patches, warnings) = load_all();
        assert!(warnings.is_empty(), "no manifest warnings: {warnings:?}");
        let mut checked = 0;
        for p in &patches {
            let Some(out_name) = p.output.as_deref() else {
                continue;
            };
            let expected = std::fs::read(dir.join(out_name)).expect("shipped output image");
            assert!(
                p.target_matches(&orig),
                "{}: reference image hash mismatch",
                p.name
            );
            let mut image = orig.clone();
            p.apply(&mut image)
                .unwrap_or_else(|e| panic!("{}: {e}", p.name));
            assert_eq!(image, expected, "{} must reproduce {out_name}", p.name);
            p.revert(&mut image)
                .unwrap_or_else(|e| panic!("{}: revert {e}", p.name));
            assert_eq!(image, orig, "{} revert must restore the reference", p.name);
            checked += 1;
        }
        assert_eq!(checked, 4, "all four shipped images must be checked");
    }

}
