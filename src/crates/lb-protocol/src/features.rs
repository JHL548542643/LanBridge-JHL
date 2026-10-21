//! Feature capability bitmap exchanged in Hello.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Features(pub u32);

impl Features {
    pub const FILE: u32 = 1 << 0;
    pub const CLIPBOARD: u32 = 1 << 1;
    pub const RESUME: u32 = 1 << 2;

    /// M1 control-only.
    pub const M1_CONTROL: Features = Features(0);

    /// M2 file transfer (+ resume).
    pub const M2_FILE: Features = Features(Self::FILE | Self::RESUME);

    /// M3 adds clipboard.
    pub const M3_CLIPBOARD: Features = Features(Self::FILE | Self::RESUME | Self::CLIPBOARD);

    pub fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    pub fn intersect(self, other: Features) -> Features {
        Features(self.0 & other.0)
    }

    pub fn with(self, bit: u32) -> Features {
        Features(self.0 | bit)
    }
}
