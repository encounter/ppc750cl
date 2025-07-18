use crate::generated::{
    parse_basic, parse_defs, parse_simplified, parse_uses, Arguments, Extension, Opcode, EMPTY_ARGS,
};
use core::{
    fmt::{self, Display, Formatter, LowerHex},
    hash::{Hash, Hasher},
    ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not},
};

/// A PowerPC instruction.
#[derive(Default, Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ins {
    pub code: u32,
    pub op: Opcode,
}

impl Ins {
    /// Create a new instruction from its raw code.
    #[inline]
    pub fn new(code: u32, extensions: Extensions) -> Self {
        Self { code, op: Opcode::detect(code, extensions) }
    }

    /// Parse the instruction into a simplified mnemonic, if any match.
    #[inline]
    pub fn parse_simplified(self, out: &mut ParsedIns) {
        parse_simplified(out, self)
    }

    /// Returns the simplified form of the instruction, if any match.
    #[inline]
    pub fn simplified(self) -> ParsedIns {
        let mut out = ParsedIns::new();
        parse_simplified(&mut out, self);
        out
    }

    /// Parse the instruction into its basic form.
    #[inline]
    pub fn parse_basic(self, out: &mut ParsedIns) {
        parse_basic(out, self)
    }

    /// Returns the basic form of the instruction.
    #[inline]
    pub fn basic(self) -> ParsedIns {
        let mut out = ParsedIns::new();
        parse_basic(&mut out, self);
        out
    }

    /// Emits all registers defined by the instruction into the given argument list.
    #[inline]
    pub fn parse_defs(self, out: &mut Arguments) {
        parse_defs(out, self)
    }

    /// Returns all registers defined by the instruction.
    #[inline]
    pub fn defs(self) -> Arguments {
        let mut out = Arguments::default();
        parse_defs(&mut out, self);
        out
    }

    /// Emits all registers used by the instruction into the given argument list.
    #[inline]
    pub fn parse_uses(self, out: &mut Arguments) {
        parse_uses(out, self)
    }

    /// Returns all registers used by the instruction.
    #[inline]
    pub fn uses(self) -> Arguments {
        let mut out = Arguments::default();
        parse_uses(&mut out, self);
        out
    }

    /// Returns the relative branch offset of the instruction, if any.
    pub fn branch_offset(&self) -> Option<i32> {
        match self.op {
            Opcode::B => Some(self.field_li()),
            Opcode::Bc => Some(self.field_bd() as i32),
            _ => None,
        }
    }

    /// Returns the absolute branch destination of the instruction, if any.
    pub fn branch_dest(&self, addr: u32) -> Option<u32> {
        self.branch_offset().and_then(|offset| {
            if self.field_aa() {
                Some(offset as u32)
            } else {
                addr.checked_add_signed(offset)
            }
        })
    }

    /// Whether the instruction is any kind of branch.
    pub fn is_branch(&self) -> bool {
        matches!(self.op, Opcode::B | Opcode::Bc | Opcode::Bcctr | Opcode::Bclr)
    }

    /// Whether the instruction is a direct branch.
    pub fn is_direct_branch(&self) -> bool {
        matches!(self.op, Opcode::B | Opcode::Bc)
    }

    /// Whether the instruction is an unconditional branch.
    pub fn is_unconditional_branch(&self) -> bool {
        match self.op {
            Opcode::B => true,
            Opcode::Bc | Opcode::Bcctr | Opcode::Bclr => {
                self.field_bo() == 20 && self.field_bi() == 0
            }
            _ => false,
        }
    }

    /// Whether the instruction is a conditional branch.
    pub fn is_conditional_branch(&self) -> bool {
        self.is_branch() && !self.is_unconditional_branch()
    }

    /// Whether the instruction is a branch with link. (blr)
    #[inline]
    pub fn is_blr(&self) -> bool {
        self.code == 0x4e800020
    }
}

impl Hash for Ins {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.code.hash(state);
    }
}

impl Hash for Opcode {
    /// Opcode enum discriminants are not stable.
    /// Instead, hash the mnemonic string.
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.mnemonic().hash(state);
    }
}

macro_rules! field_arg_no_display {
    ($name:ident, $typ:ident) => {
        #[derive(Debug, Copy, Clone, Hash, Ord, PartialOrd, Eq, PartialEq)]
        pub struct $name(pub $typ);
        impl From<$name> for Argument {
            fn from(x: $name) -> Argument {
                Argument::$name(x)
            }
        }
        impl From<$typ> for $name {
            fn from(x: $typ) -> $name {
                $name(x)
            }
        }
    };
}

macro_rules! field_arg {
    ($name:ident, $typ:ident) => {
        field_arg_no_display!($name, $typ);
        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
    ($name:ident, $typ:ident, $format:literal) => {
        field_arg_no_display!($name, $typ);
        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, $format, self.0)
            }
        }
    };
    ($name:ident, $typ:ident, $format:literal, $format_arg:expr) => {
        field_arg_no_display!($name, $typ);
        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, $format, $format_arg(self.0))
            }
        }
    };
}

// General-purpose register.
field_arg!(GPR, u8, "r{}");
// Floating-point register (direct or paired-singles mode).
field_arg!(FPR, u8, "f{}");
// Segment register.
field_arg!(SR, u8);
// Special-purpose register.
field_arg_no_display!(SPR, u16);
impl Display for SPR {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            1 => "XER",
            8 => "LR",
            9 => "CTR",
            18 => "DSISR",
            19 => "DAR",
            22 => "DEC",
            25 => "SDR1",
            26 => "SRR0",
            27 => "SRR1",
            272 => "SPRG0",
            273 => "SPRG1",
            274 => "SPRG2",
            275 => "SPRG3",
            282 => "EAR",
            287 => "PVR",
            528 => "IBAT0U",
            529 => "IBAT0L",
            530 => "IBAT1U",
            531 => "IBAT1L",
            532 => "IBAT2U",
            533 => "IBAT2L",
            534 => "IBAT3U",
            535 => "IBAT3L",
            536 => "DBAT0U",
            537 => "DBAT0L",
            538 => "DBAT1U",
            539 => "DBAT1L",
            540 => "DBAT2U",
            541 => "DBAT2L",
            542 => "DBAT3U",
            543 => "DBAT3L",
            912 => "GQR0",
            913 => "GQR1",
            914 => "GQR2",
            915 => "GQR3",
            916 => "GQR4",
            917 => "GQR5",
            918 => "GQR6",
            919 => "GQR7",
            920 => "HID2",
            921 => "WPAR",
            922 => "DMA_U",
            923 => "DMA_L",
            936 => "UMMCR0",
            937 => "UPMC1",
            938 => "UPMC2",
            939 => "USIA",
            940 => "UMMCR1",
            941 => "UPMC3",
            942 => "UPMC4",
            943 => "USDA",
            952 => "MMCR0",
            953 => "PMC1",
            954 => "PMC2",
            955 => "SIA",
            956 => "MMCR1",
            957 => "PMC3",
            958 => "PMC4",
            959 => "SDA",
            1008 => "HID0",
            1009 => "HID1",
            1010 => "IABR",
            1013 => "DABR",
            1017 => "L2CR",
            1019 => "ICTC",
            1020 => "THRM1",
            1021 => "THRM2",
            1022 => "THRM3",
            _ => return write!(f, "{}", self.0),
        })
    }
}
// Condition register field.
field_arg!(CRField, u8, "cr{}");
// Condition register bit (index + condition case).
field_arg_no_display!(CRBit, u8);
impl Display for CRBit {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let cr = self.0 >> 2;
        let cc = self.0 & 3;
        if cr != 0 {
            write!(f, "{}", CRField(cr))?;
        }
        const CR_NAMES: [&str; 4] = ["lt", "gt", "eq", "un"];
        f.write_str(CR_NAMES[cc as usize])
    }
}
// Paired-single graphics quantization register
field_arg!(GQR, u8, "qr{}");
// Unsigned immediate.
field_arg!(Uimm, u16, "{:#x}");
// Signed immediate.
field_arg!(Simm, i16, "{:#x}", SignedHexLiteral);
// Offset for indirect memory reference.
field_arg!(Offset, i16, "{:#x}", SignedHexLiteral);
// Branch destination.
field_arg!(BranchDest, i32, "{:#x}", SignedHexLiteral);
impl From<i16> for BranchDest {
    fn from(x: i16) -> BranchDest {
        BranchDest(x as i32)
    }
}
// Unsigned opaque argument.
field_arg!(OpaqueU, u16);
impl From<u8> for OpaqueU {
    fn from(x: u8) -> OpaqueU {
        OpaqueU(x as u16)
    }
}
// Vector register.
field_arg!(VR, u8, "v{}");

#[derive(Debug, Default, Copy, Clone, Eq, Hash, PartialEq)]
pub enum Argument {
    #[default]
    None,
    GPR(GPR),
    FPR(FPR),
    SR(SR),
    SPR(SPR),
    CRField(CRField),
    CRBit(CRBit),
    GQR(GQR),
    Uimm(Uimm),
    Simm(Simm),
    Offset(Offset),
    BranchDest(BranchDest),
    OpaqueU(OpaqueU),
    VR(VR),
}

impl Display for Argument {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Argument::None => Ok(()),
            Argument::GPR(x) => x.fmt(f),
            Argument::FPR(x) => x.fmt(f),
            Argument::SR(x) => x.fmt(f),
            Argument::SPR(x) => x.fmt(f),
            Argument::CRField(x) => x.fmt(f),
            Argument::CRBit(x) => x.fmt(f),
            Argument::GQR(x) => x.fmt(f),
            Argument::Uimm(x) => x.fmt(f),
            Argument::Simm(x) => x.fmt(f),
            Argument::Offset(x) => x.fmt(f),
            Argument::BranchDest(x) => x.fmt(f),
            Argument::OpaqueU(x) => x.fmt(f),
            Argument::VR(x) => x.fmt(f),
        }
    }
}

/// A parsed PowerPC instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedIns {
    pub mnemonic: &'static str,
    pub args: Arguments,
}

impl Default for ParsedIns {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl ParsedIns {
    /// An empty parsed instruction.
    #[inline]
    pub const fn new() -> Self {
        Self { mnemonic: "<illegal>", args: EMPTY_ARGS }
    }

    /// Returns an iterator over the arguments of the instruction,
    /// stopping at the first [Argument::None].
    #[inline]
    pub fn args_iter(&self) -> impl Iterator<Item = &Argument> {
        self.args.iter().take_while(|x| !matches!(x, Argument::None))
    }
}

impl Display for ParsedIns {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.mnemonic)?;
        let mut writing_offset = false;
        for (i, argument) in self.args_iter().enumerate() {
            if i == 0 {
                write!(f, " ")?;
            } else if !writing_offset {
                write!(f, ", ")?;
            }
            write!(f, "{argument}")?;
            if let Argument::Offset(_) = argument {
                write!(f, "(")?;
                writing_offset = true;
            } else if writing_offset {
                write!(f, ")")?;
                writing_offset = false;
            }
        }
        Ok(())
    }
}

pub struct SignedHexLiteral<T>(pub T);

impl LowerHex for SignedHexLiteral<i16> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.0 < 0 {
            write!(f, "-")?;
            LowerHex::fmt(&-(self.0 as i32), f)
        } else {
            LowerHex::fmt(&self.0, f)
        }
    }
}

impl LowerHex for SignedHexLiteral<i32> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.0 < 0 {
            write!(f, "-")?;
            LowerHex::fmt(&-(self.0 as i64), f)
        } else {
            LowerHex::fmt(&self.0, f)
        }
    }
}

pub struct InsIter<'a> {
    address: u32,
    extensions: Extensions,
    data: &'a [u8],
}

impl<'a> InsIter<'a> {
    #[inline]
    pub fn new(data: &'a [u8], address: u32, extensions: Extensions) -> Self {
        Self { address, extensions, data }
    }

    #[inline]
    pub fn address(&self) -> u32 {
        self.address
    }

    #[inline]
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    #[inline]
    pub fn extensions(&self) -> Extensions {
        self.extensions
    }
}

impl Iterator for InsIter<'_> {
    type Item = (u32, Ins);

    fn next(&mut self) -> Option<Self::Item> {
        if self.data.len() < 4 {
            return None;
        }

        // SAFETY: The slice is guaranteed to be at least 4 bytes long.
        let chunk = unsafe { *(self.data.as_ptr() as *const [u8; 4]) };
        let ins = Ins::new(u32::from_be_bytes(chunk), self.extensions);
        let addr = self.address;
        self.address += 4;
        self.data = &self.data[4..];
        Some((addr, ins))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Extensions(u32);

impl Extensions {
    /// Creates an empty set of extensions.
    #[inline]
    pub const fn none() -> Self {
        Self(0)
    }

    /// The set of extensions used by the PowerPC 750CXe (Gekko) / 750CL (Broadway) CPUs
    /// used in the GameCube and Wii respectively.
    #[inline]
    pub const fn gekko_broadway() -> Self {
        Self::from_bitmask(Extension::PairedSingles.bitmask())
    }

    /// The set of extensions used by the PowerPC Xenon CPU used in the Xbox 360.
    #[inline]
    pub const fn xenon() -> Self {
        Self::from_bitmask(
            Extension::Ppc64.bitmask() | Extension::AltiVec.bitmask() | Extension::Vmx128.bitmask(),
        )
    }

    /// Checks if the given extension (and all required extensions) are enabled.
    #[inline]
    pub const fn contains(&self, ext: Extension) -> bool {
        let bitmask = ext.bitmask();
        (self.0 & bitmask) == bitmask
    }

    /// Checks if the given set of extensions are enabled.
    #[inline]
    pub const fn contains_all(&self, other: Extensions) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Enables the given extension. Implicitly enables all required extensions.
    #[inline]
    pub const fn insert(&mut self, ext: Extension) {
        self.0 |= ext.bitmask();
    }

    /// Disables the given extension.
    #[inline]
    pub const fn remove(&mut self, ext: Extension) {
        // Instead of using bitmask, which includes required extensions,
        // we only clear the bit for the specific extension.
        self.0 &= !(1 << (ext as u32));
    }

    /// Enables or disables the given extension.
    #[inline]
    pub const fn set(&mut self, ext: Extension, value: bool) {
        if value {
            self.insert(ext);
        } else {
            self.remove(ext);
        }
    }

    /// Returns whether the extensions set is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Returns the raw bitmask of the extensions.
    #[inline]
    pub const fn bitmask(&self) -> u32 {
        self.0
    }

    /// Creates a set of extensions from a raw bitmask.
    #[inline]
    pub const fn from_bitmask(bitmask: u32) -> Self {
        Self(bitmask)
    }

    /// Creates a set of extensions from a single extension (and its required extensions).
    #[inline]
    pub const fn from_extension(ext: Extension) -> Self {
        Self(ext.bitmask())
    }
}

impl Default for Extensions {
    #[inline]
    fn default() -> Self {
        Self::none()
    }
}

impl From<Extension> for Extensions {
    #[inline]
    fn from(ext: Extension) -> Self {
        Self::from_extension(ext)
    }
}

impl BitOr for Extensions {
    type Output = Extensions;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOr<Extension> for Extensions {
    type Output = Extensions;

    #[inline]
    fn bitor(self, rhs: Extension) -> Self::Output {
        Self(self.0 | rhs.bitmask())
    }
}

impl BitOrAssign<Extension> for Extensions {
    #[inline]
    fn bitor_assign(&mut self, rhs: Extension) {
        self.0 |= rhs.bitmask();
    }
}

impl BitAnd for Extensions {
    type Output = Extensions;

    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAnd<Extension> for Extensions {
    type Output = Extensions;

    #[inline]
    fn bitand(self, rhs: Extension) -> Self::Output {
        Self(self.0 & rhs.bitmask())
    }
}

impl BitAndAssign<Extension> for Extensions {
    #[inline]
    fn bitand_assign(&mut self, rhs: Extension) {
        self.0 &= rhs.bitmask();
    }
}

impl Not for Extensions {
    type Output = Extensions;

    #[inline]
    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

impl BitOr for Extension {
    type Output = Extensions;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Extensions(self.bitmask() | rhs.bitmask())
    }
}

impl Hash for Extension {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bitmask().hash(state);
    }
}

impl Display for Extension {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
