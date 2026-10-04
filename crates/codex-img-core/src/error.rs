use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Other, Usage }
#[derive(Debug)]
pub struct Error { pub kind: Kind, pub message: String }
impl Error {
    pub fn other(message: impl Into<String>) -> Self { Self { kind: Kind::Other, message: message.into() } }
    pub fn usage(message: impl Into<String>) -> Self { Self { kind: Kind::Usage, message: message.into() } }
}
impl fmt::Display for Error { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) } }
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
