use anyhow::{Result, bail};

/// Parse a size such as `2GB`, `2g`, `2048MB`, `512m` into megabytes.
pub fn parse_size_mb(input: &str) -> Result<u64> {
    let s = input.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    if num.is_empty() {
        bail!("invalid size {input:?}: expected a number like 2GB or 2048MB");
    }
    let n: u64 = num.parse()?;
    match unit.trim() {
        "g" | "gb" | "gib" => Ok(n * 1024),
        "m" | "mb" | "mib" | "" => Ok(n),
        other => bail!("invalid size unit {other:?} in {input:?}: use MB or GB"),
    }
}

/// An image reference such as `android:35`, `android:36.1` or `android:35-google_apis`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub api: u32,
    /// Minor SDK version (`36.1`). `None` matches any, preferring the plain major.
    pub minor: Option<u32>,
    pub flavour: Option<String>,
}

impl std::fmt::Display for ImageRef {
    /// The `android:<api>[.minor][-flavour]` form that `ImageRef::parse` accepts.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "android:{}", self.api)?;
        if let Some(m) = self.minor {
            write!(f, ".{m}")?;
        }
        if let Some(fl) = &self.flavour {
            write!(f, "-{fl}")?;
        }
        Ok(())
    }
}

/// Short flavour names: `playstore` is Google's `google_apis_playstore` image (and
/// `playstore_ps16k` its 16 KB-page variant). Full names are passed through unchanged.
fn expand_flavour(f: &str) -> String {
    if f == "playstore" || f.starts_with("playstore_") {
        format!("google_apis_{f}")
    } else {
        f.to_string()
    }
}

impl ImageRef {
    pub fn parse(input: &str) -> Result<Self> {
        let Some(tag) = input.strip_prefix("android:") else {
            bail!(
                "unsupported image {input:?}: only base images like `android:35` are supported \
                 in this release (built images arrive with `dorker build`)"
            );
        };
        let (api, flavour) = match tag.split_once('-') {
            Some((api, flavour)) if !flavour.is_empty() => (api, Some(expand_flavour(flavour))),
            _ => (tag, None),
        };
        let bad = || anyhow::anyhow!("invalid Android API level in {input:?}");
        let (major, minor) = match api.split_once('.') {
            Some((a, b)) => (a, Some(b.parse::<u32>().map_err(|_| bad())?)),
            None => (api, None),
        };
        let api: u32 = major.parse().map_err(|_| bad())?;
        Ok(Self {
            api,
            minor,
            flavour,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playstore_is_a_short_name_for_the_google_play_flavour() {
        let r = ImageRef::parse("android:35-playstore").unwrap();
        assert_eq!(r.flavour.as_deref(), Some("google_apis_playstore"));
        assert_eq!(r.to_string(), "android:35-google_apis_playstore");
        assert_eq!(
            ImageRef::parse("android:37-playstore_ps16k")
                .unwrap()
                .flavour
                .as_deref(),
            Some("google_apis_playstore_ps16k")
        );
        // Full names and other flavours are untouched.
        assert_eq!(
            ImageRef::parse("android:35-google_apis_playstore")
                .unwrap()
                .flavour
                .as_deref(),
            Some("google_apis_playstore")
        );
        assert_eq!(
            ImageRef::parse("android:35-aosp_atd")
                .unwrap()
                .flavour
                .as_deref(),
            Some("aosp_atd")
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size_mb("2GB").unwrap(), 2048);
        assert_eq!(parse_size_mb("2g").unwrap(), 2048);
        assert_eq!(parse_size_mb("512MB").unwrap(), 512);
        assert_eq!(parse_size_mb("1024").unwrap(), 1024);
        assert!(parse_size_mb("abc").is_err());
        assert!(parse_size_mb("2TB").is_err());
    }

    #[test]
    fn image_refs_round_trip_through_display() {
        for s in [
            "android:35",
            "android:36.1",
            "android:35-google_apis",
            "android:36.1-google_apis_playstore",
        ] {
            assert_eq!(ImageRef::parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn image_refs() {
        let r = ImageRef::parse("android:35").unwrap();
        assert_eq!((r.api, r.flavour), (35, None));
        let r = ImageRef::parse("android:35-google_apis").unwrap();
        assert_eq!(r.flavour.as_deref(), Some("google_apis"));
        let r = ImageRef::parse("android:36.1-google_apis_playstore").unwrap();
        assert_eq!((r.api, r.minor), (36, Some(1)));
        assert!(ImageRef::parse("android:36.x").is_err());
        assert!(ImageRef::parse("myapp:latest").is_err());
        assert!(ImageRef::parse("android:abc").is_err());
    }
}
