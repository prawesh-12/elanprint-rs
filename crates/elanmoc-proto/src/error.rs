/// Everything response parsing can fail with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    #[error("{command}: expected {wanted} bytes, got {got}")]
    UnexpectedLength {
        command: &'static str,
        wanted: usize,
        got: usize,
    },
    #[error("{command}: device returned status 0x{code:02x}")]
    Device { command: &'static str, code: u8 },
}
