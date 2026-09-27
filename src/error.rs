use std::fmt;

pub const RENEW_HINT: &str = "Open Codex (run `codex` or the Codex app) once so it renews your login, or run `codex login` to sign in again, then retry.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Other,
    Auth,
    Quota,
    Moderation,
    Usage,
}

impl Kind {
    pub fn exit_code(self) -> i32 {
        match self {
            Kind::Other => 1,
            Kind::Auth => 2,
            Kind::Quota => 3,
            Kind::Moderation => 4,
            Kind::Usage => 64,
        }
    }
}

#[derive(Debug)]
pub struct Error {
    pub kind: Kind,
    pub message: String,
}

impl Error {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
    pub fn other(message: impl Into<String>) -> Self {
        Self::new(Kind::Other, message)
    }
    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(Kind::Auth, message)
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(Kind::Usage, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
