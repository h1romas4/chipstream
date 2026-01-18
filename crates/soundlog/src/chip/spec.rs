//! Chip specifications and write-operation payload types.
//!
//! This module defines the `Chip` enum listing supported sound chip
//! backends and a set of small `*Spec` structs describing the payload
//! for chip-specific register or memory writes (for example register
//! index and value, or memory offset and value). These types are used
//! by the tracer and parser code to represent chip write commands in a
//! structured, type-safe way.
//!
//! Each `*Spec` type documents the fields required to perform the
//! corresponding chip operation (for example, `Ym2413Spec` contains a
//! register and a value). The `Chip` enum enumerates the hardware
//! devices supported by this crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Chip {
    Sn76489,
    Ym2413,
    Ym2612,
    Ym2151,
    SegaPcm,
    Rf5c68,
    Ym2203,
    Ym2608,
    Ym2610b,
    Ym3812,
    Ym3526,
    Y8950,
    Ymf262,
    Ymf278b,
    Ymf271,
    Scc1,
    Ymz280b,
    Rf5c164,
    Pwm,
    Ay8910,
    GbDmg,
    NesApu,
    MultiPcm,
    Upd7759,
    Okim6258,
    Okim6295,
    K051649,
    K054539,
    Huc6280,
    C140,
    K053260,
    Pokey,
    Qsound,
    Scsp,
    WonderSwan,
    Vsu,
    Saa1099,
    Es5503,
    Es5506U8,
    Es5506U16,
    X1010,
    C352,
    Ga20,
    Mikey,
    GameGearPsg,
}

/// PSG (SN76489/SN76496) write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PsgSpec {
    pub value: u8,
}

/// YM2413 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2413Spec {
    pub register: u8,
    pub value: u8,
}

/// YM2612 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2612Spec {
    pub port: u8,
    pub register: u8,
    pub value: u8,
}

/// YM2151 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2151Spec {
    pub register: u8,
    pub value: u8,
}

/// Sega PCM memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SegaPcmSpec {
    pub offset: u16,
    pub value: u8,
}

/// RF5C68 memory write specification (8-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c68U8Spec {
    pub offset: u8,
    pub value: u8,
}

/// RF5C68 memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c68U16Spec {
    pub offset: u16,
    pub value: u8,
}

/// RF5C68 memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c68Spec {
    pub offset: u16,
    pub value: u8,
}

/// YM2203 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2203Spec {
    pub register: u8,
    pub value: u8,
}

/// YM2608 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2608Spec {
    pub port: u8,
    pub register: u8,
    pub value: u8,
}

/// YM2610 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2610Spec {
    pub port: u8,
    pub register: u8,
    pub value: u8,
}

/// YM3812 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym3812Spec {
    pub register: u8,
    pub value: u8,
}

/// YM3526 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym3526Spec {
    pub register: u8,
    pub value: u8,
}

/// Y8950 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Y8950Spec {
    pub register: u8,
    pub value: u8,
}

/// YMF262 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymf262Spec {
    pub port: u8,
    pub register: u8,
    pub value: u8,
}

/// YMF278B write specification (includes port selection)..
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymf278bSpec {
    pub port: u8,
    pub register: u8,
    pub value: u8,
}

/// YMF271 write specification (includes port selection)..
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymf271Spec {
    pub port: u8,
    pub register: u8,
    pub value: u8,
}

/// SCC1 write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scc1Spec {
    pub port: u8,
    pub register: u8,
    pub value: u8,
}

/// YMZ280B register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymz280bSpec {
    pub register: u8,
    pub value: u8,
}

/// RF5C164 register write specification (8-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c164U8Spec {
    pub offset: u8,
    pub value: u8,
}

/// RF5C164 register write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c164U16Spec {
    pub offset: u16,
    pub value: u8,
}

/// PWM register write specification (24-bit value in lower bits).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PwmSpec {
    pub register: u8,
    /// lower 24 bits are used
    pub value: u32,
}

/// AY-8910 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ay8910Spec {
    pub register: u8,
    pub value: u8,
}

/// GameBoy DMG register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GbDmgSpec {
    pub register: u8,
    pub value: u8,
}

/// NES APU register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NesApuSpec {
    pub register: u8,
    pub value: u8,
}

/// MultiPCM register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MultiPcmSpec {
    pub register: u8,
    pub value: u8,
}

/// MultiPCM bank write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MultiPcmBankSpec {
    pub channel: u8,
    pub bank_offset: u16,
}

/// uPD7759 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Upd7759Spec {
    pub register: u8,
    pub value: u8,
}

/// OKIM6258 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Okim6258Spec {
    pub register: u8,
    pub value: u8,
}

/// OKIM6295 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Okim6295Spec {
    pub register: u8,
    pub value: u8,
}

/// K051649 register write specification (16-bit register index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct K051649Spec {
    pub register: u16,
    pub value: u8,
}

/// K054539 register write specification (16-bit register index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct K054539Spec {
    pub register: u16,
    pub value: u8,
}

/// HuC6280 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Huc6280Spec {
    pub register: u8,
    pub value: u8,
}

/// C140 register write specification (16-bit register index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct C140Spec {
    pub register: u16,
    pub value: u8,
}

/// K053260 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct K053260Spec {
    pub register: u8,
    pub value: u8,
}

/// Pokey register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PokeySpec {
    pub register: u8,
    pub value: u8,
}

/// QSound register write specification (16-bit value).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QsoundSpec {
    pub register: u8,
    pub value: u16,
}

/// SCSP memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScspSpec {
    pub offset: u16,
    pub value: u8,
}

/// WonderSwan memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WonderSwanSpec {
    pub offset: u16,
    pub value: u8,
}

/// VSU memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VsuSpec {
    pub offset: u16,
    pub value: u8,
}

/// SAA1099 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Saa1099Spec {
    pub register: u8,
    pub value: u8,
}

/// ES5503 register write specification (16-bit register index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Es5503Spec {
    pub register: u16,
    pub value: u8,
}

/// ES5506 (8-bit variant) register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Es5506U8Spec {
    pub register: u8,
    pub value: u8,
}

/// ES5506 (16-bit variant) register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Es5506U16Spec {
    pub register: u8,
    pub value: u16,
}

/// X1-010 memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct X1010Spec {
    pub offset: u16,
    pub value: u8,
}

/// C352 register write specification (16-bit register and 16-bit value).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct C352Spec {
    pub register: u16,
    pub value: u16,
}

/// GA20 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ga20Spec {
    pub register: u8,
    pub value: u8,
}

/// Mikey register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MikeySpec {
    pub register: u8,
    pub value: u8,
}

/// Game Gear PSG write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameGearPsgSpec {
    pub value: u8,
}
