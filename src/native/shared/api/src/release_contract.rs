//! Whole-package admission identity. A declaration is not binary attestation.
//! The immutable server policy must come from a separately verified startup
//! catalog. Four role declarations are checked before reducing to its digest.
use serde::{Deserialize, Serialize};

pub const RELEASE_SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReleaseIdentity {
    release_id: String,
    compat_id: String,
    manifest_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError { Required, Malformed, Mixed, ReleaseMismatch, CompatMismatch, CatalogMismatch }
impl AdmissionError {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Required => "PACKAGE_IDENTITY_REQUIRED",
            Self::Malformed => "PACKAGE_IDENTITY_INVALID",
            Self::Mixed => "PACKAGE_MIXED",
            Self::ReleaseMismatch => "RELEASE_MISMATCH",
            Self::CompatMismatch => "COMPAT_MISMATCH",
            Self::CatalogMismatch => "PACKAGE_CATALOG_MISMATCH",
        }
    }
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.reason()) }
}
impl std::error::Error for AdmissionError {}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
impl ReleaseIdentity {
    pub fn new(schema: u32, release: &str, compat: &str, digest: &str) -> Result<Self, AdmissionError> {
        if schema != RELEASE_SCHEMA || !valid_id(release) || !valid_id(compat)
            || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(AdmissionError::Malformed);
        }
        Ok(Self { release_id: release.into(), compat_id: compat.into(), manifest_sha256: digest.into() })
    }
    /// Order is launcher, main DLL, gameplay DLL, server. Official still declares
    /// the packaged gameplay role; it does not need to load the gameplay DLL.
    pub fn from_components(schema: u32, release: &str, compat: &str, digest: &str,
        components: [Option<(&str, &str)>; 4]) -> Result<Self, AdmissionError> {
        let identity = Self::new(schema, release, compat, digest)?;
        if components.iter().any(|role| *role != Some((release, compat))) { return Err(AdmissionError::Mixed); }
        Ok(identity)
    }
    pub fn admit(&self, declared: Option<&Self>) -> Result<(), AdmissionError> {
        let declared = declared.ok_or(AdmissionError::Required)?;
        if self.release_id != declared.release_id { return Err(AdmissionError::ReleaseMismatch); }
        if self.compat_id != declared.compat_id { return Err(AdmissionError::CompatMismatch); }
        if self.manifest_sha256 != declared.manifest_sha256 { return Err(AdmissionError::CatalogMismatch); }
        Ok(())
    }
    pub fn storage_key(&self) -> String {
        format!("1|{}|{}|{}", self.release_id, self.compat_id, self.manifest_sha256)
    }
    /// Legacy NULL is unknown, not permission to upgrade the same nonce.
    pub fn matches_stored_run(&self, stored: Option<&str>) -> bool { stored == Some(self.storage_key().as_str()) }
}
impl TryFrom<String> for ReleaseIdentity {
    type Error = AdmissionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let parts: Vec<_> = value.split('|').collect();
        if parts.len() != 4 || parts[0] != "1" { return Err(AdmissionError::Malformed); }
        Self::new(RELEASE_SCHEMA, parts[1], parts[2], parts[3])
    }
}
impl From<ReleaseIdentity> for String {
    fn from(value: ReleaseIdentity) -> Self { value.storage_key() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(r: &str, c: &str, d: char) -> ReleaseIdentity { ReleaseIdentity::new(1,r,c,&d.to_string().repeat(64)).unwrap() }
    #[test] fn strict_admission_distinguishes_all_tuple_fields() {
        let current=identity("release-2","compat-1",'a');
        assert_eq!(current.admit(None),Err(AdmissionError::Required));
        assert_eq!(current.admit(Some(&identity("release-1","compat-1",'a'))),Err(AdmissionError::ReleaseMismatch));
        assert_eq!(current.admit(Some(&identity("release-2","compat-2",'a'))),Err(AdmissionError::CompatMismatch));
        assert_eq!(current.admit(Some(&identity("release-2","compat-1",'b'))),Err(AdmissionError::CatalogMismatch));
        assert_eq!(current.admit(Some(&current)),Ok(()));
    }
    #[test] fn all_four_roles_must_be_present_and_match_even_in_official_package() {
        let roles=[Some(("release-2","compat-1"));4];
        assert!(ReleaseIdentity::from_components(1,"release-2","compat-1",&"a".repeat(64),roles).is_ok());
        for index in 0..4 { for replacement in [None,Some(("release-1","compat-1")),Some(("release-2","other"))] {
            let mut mixed=roles;mixed[index]=replacement;
            assert_eq!(ReleaseIdentity::from_components(1,"release-2","compat-1",&"a".repeat(64),mixed),Err(AdmissionError::Mixed));
        }}
    }
    #[test] fn malformed_or_unbounded_identities_never_become_typed() {
        for (schema,r,c,d) in [(0,"release-2","compat-1","a".repeat(64)),(1,"","compat-1","a".repeat(64)),
            (1,"r|spoof","compat-1","a".repeat(64)),(1,"release-2"," compat-1","a".repeat(64)),
            (1,"release-2","compat-1","A".repeat(64)),(1,"release-2","compat-1","a".repeat(63))] {
            assert_eq!(ReleaseIdentity::new(schema,r,c,&d),Err(AdmissionError::Malformed));
        }
        assert!(ReleaseIdentity::new(1,&"a".repeat(65),"c",&"a".repeat(64)).is_err());
        assert!(ReleaseIdentity::try_from(format!("1|r|c|{}|extra","a".repeat(64))).is_err());
    }
    #[test] fn stored_identity_cannot_change_or_upgrade_legacy_null() {
        let current=identity("release-2","compat-1",'a');
        assert!(!current.matches_stored_run(None));
        assert!(!current.matches_stored_run(Some(&identity("release-1","compat-1",'a').storage_key())));
        assert!(!current.matches_stored_run(Some(&identity("release-2","compat-1",'b').storage_key())));
        assert!(current.matches_stored_run(Some(&current.storage_key())));
        assert_eq!(ReleaseIdentity::try_from(current.storage_key()).unwrap(),current);
    }
}
