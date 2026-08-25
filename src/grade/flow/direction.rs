/// Analyze encryption and decryption separately so one real side cannot bless a custom other side.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::grade) enum OpDir {
    Encrypt,
    Decrypt,
}

impl OpDir {
    pub(super) fn has_method(self, name: &str) -> bool {
        self.method_names().contains(&name)
    }

    /// The authenticated operation names for this direction. A name is never evidence on
    /// its own: the call must use a crypto-derived receiver or path, and its result must
    /// provably flow into the returned value.
    fn method_names(self) -> &'static [&'static str] {
        match self {
            OpDir::Encrypt => &[
                "encrypt",
                "encrypt_in_place_detached",
                "seal",
                "seal_in_place_append_tag",
            ],
            OpDir::Decrypt => &[
                "decrypt",
                "decrypt_in_place_detached",
                "open",
                "open_in_place",
            ],
        }
    }
}
