use std::fmt;

/// Why an operation failed.
///
/// The discriminants are part of the C ABI: they match `wt_status` in
/// `crates/host-capi/include/watoots.h` one for one, so the C API can return
/// `kind as i32` without a translation table that could drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A caller passed something this API cannot accept.
    InvalidArgument = 1,
    /// A named thing — a file, an export — does not exist.
    NotFound = 2,
    /// The manifest could not be read, parsed, or expanded.
    Manifest = 3,
    /// The component imports something the manifest does not grant.
    PermissionDenied = 4,
    /// The component could not be compiled or instantiated.
    Load = 5,
    /// The guest trapped.
    Trap = 6,
    /// The guest exceeded a configured limit: memory, fuel, or the deadline.
    LimitExceeded = 7,
    /// A bug on our side.
    Internal = 8,
}

impl ErrorKind {
    /// The kind a [`ErrorKind::name`] spelling refers to.
    ///
    /// Replay needs this: a trace records a failure by its stable name, and
    /// reproducing that failure means turning the name back into a kind.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "WT_ERR_INVALID_ARGUMENT" => Self::InvalidArgument,
            "WT_ERR_NOT_FOUND" => Self::NotFound,
            "WT_ERR_MANIFEST" => Self::Manifest,
            "WT_ERR_PERMISSION_DENIED" => Self::PermissionDenied,
            "WT_ERR_LOAD" => Self::Load,
            "WT_ERR_TRAP" => Self::Trap,
            "WT_ERR_LIMIT_EXCEEDED" => Self::LimitExceeded,
            "WT_ERR_INTERNAL" => Self::Internal,
            _ => return None,
        })
    }

    /// Stable spelling, matching the C enumerator name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::InvalidArgument => "WT_ERR_INVALID_ARGUMENT",
            Self::NotFound => "WT_ERR_NOT_FOUND",
            Self::Manifest => "WT_ERR_MANIFEST",
            Self::PermissionDenied => "WT_ERR_PERMISSION_DENIED",
            Self::Load => "WT_ERR_LOAD",
            Self::Trap => "WT_ERR_TRAP",
            Self::LimitExceeded => "WT_ERR_LIMIT_EXCEEDED",
            Self::Internal => "WT_ERR_INTERNAL",
        }
    }
}

/// An error with a code the C API can return and a message a human can read.
///
/// Deliberately not an enum with payloads: everything crossing the C boundary
/// has to collapse to `(code, string)` anyway, so the Rust type is shaped that
/// way from the start rather than being flattened at the edge.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    /// Build an error.
    ///
    /// Public because host functions need it: an application serving an
    /// interface to a plugin has to be able to fail, and the failure has to
    /// carry the same shape as ours so the C API can return it unchanged.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// An error for a caller that passed something unusable.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    /// An error for something that does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    /// An error for a bug on our side.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// The category of failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// A human-readable explanation. Never empty.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_discriminants_match_the_c_header() {
        // If these drift, the C API starts reporting the wrong status. The
        // header is the other half of this pair; keep them in step.
        assert_eq!(ErrorKind::InvalidArgument as i32, 1);
        assert_eq!(ErrorKind::NotFound as i32, 2);
        assert_eq!(ErrorKind::Manifest as i32, 3);
        assert_eq!(ErrorKind::PermissionDenied as i32, 4);
        assert_eq!(ErrorKind::Load as i32, 5);
        assert_eq!(ErrorKind::Trap as i32, 6);
        assert_eq!(ErrorKind::LimitExceeded as i32, 7);
        assert_eq!(ErrorKind::Internal as i32, 8);
    }

    #[test]
    fn every_kind_round_trips_through_its_name() {
        for kind in [
            ErrorKind::InvalidArgument,
            ErrorKind::NotFound,
            ErrorKind::Manifest,
            ErrorKind::PermissionDenied,
            ErrorKind::Load,
            ErrorKind::Trap,
            ErrorKind::LimitExceeded,
            ErrorKind::Internal,
        ] {
            assert_eq!(ErrorKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(ErrorKind::from_name("WT_ERR_NONSENSE"), None);
    }

    #[test]
    fn error_displays_its_message() {
        let err = Error::new(ErrorKind::Manifest, "unknown key: fs.exec");
        assert_eq!(err.to_string(), "unknown key: fs.exec");
        assert_eq!(err.kind(), ErrorKind::Manifest);
        assert_eq!(err.kind().name(), "WT_ERR_MANIFEST");
    }
}
