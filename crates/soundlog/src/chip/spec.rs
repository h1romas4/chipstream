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
/// Sound chip supported by the register-write API.
pub enum Chip {
    /// SN76489/SN76496 programmable sound generator.
    Sn76489,
    /// YM2413 FM Operator Type-LL synthesizer.
    Ym2413,
    /// YM2612 FM synthesizer.
    Ym2612,
    /// YM2151 FM synthesizer.
    Ym2151,
    /// Sega PCM playback chip.
    SegaPcm,
    /// Ricoh RF5C68 PCM chip.
    Rf5c68,
    /// YM2203 FM and programmable sound generator.
    Ym2203,
    /// YM2608 FM, PSG, and ADPCM synthesizer.
    Ym2608,
    /// YM2610B FM and ADPCM synthesizer.
    Ym2610b,
    /// YM3812 OPL2 FM synthesizer.
    Ym3812,
    /// YM3526 OPL FM synthesizer.
    Ym3526,
    /// Yamaha Y8950 FM and ADPCM synthesizer.
    Y8950,
    /// YMF262 OPL3 FM synthesizer.
    Ymf262,
    /// YMF278B OPL4 FM and PCM synthesizer.
    Ymf278b,
    /// YMF271 OPX synthesizer.
    Ymf271,
    /// YMZ280B PCM chip.
    Ymz280b,
    /// Ricoh RF5C164 PCM chip.
    Rf5c164,
    /// Sega PWM audio device.
    Pwm,
    /// AY-3-8910 programmable sound generator.
    Ay8910,
    /// Game Boy DMG audio processing unit.
    GbDmg,
    /// Nintendo Entertainment System audio processing unit.
    NesApu,
    /// Yamaha MultiPCM chip.
    MultiPcm,
    /// Yamaha uPD7759 ADPCM chip.
    Upd7759,
    /// OKI MSM6258 ADPCM chip.
    Okim6258,
    /// OKI MSM6295 ADPCM chip.
    Okim6295,
    /// Konami K051649 SCC sound chip.
    K051649,
    /// Konami K054539 PCM chip.
    K054539,
    /// Hudson Soft HuC6280 audio unit.
    Huc6280,
    /// Namco C140 PCM chip.
    C140,
    /// Konami K053260 PCM chip.
    K053260,
    /// Atari POKEY sound chip.
    Pokey,
    /// Capcom QSound chip.
    Qsound,
    /// Sega Saturn SCSP sound chip.
    Scsp,
    /// WonderSwan audio processing unit.
    WonderSwan,
    /// Virtual Boy VSU audio processing unit.
    Vsu,
    /// Philips/SAA SAA1099 sound chip.
    Saa1099,
    /// Ensoniq ES5503 DOC sound chip.
    Es5503,
    /// Ensoniq ES5506 with 8-bit register writes.
    Es5506U8,
    /// Ensoniq ES5506 with 16-bit register writes.
    Es5506U16,
    /// Setia X1-010 PCM chip.
    X1010,
    /// Namco C352 PCM chip.
    C352,
    /// Irem GA20 PCM chip.
    Ga20,
    /// Atari Lynx Mikey sound chip.
    Mikey,
}

/// PSG (SN76489/SN76496) write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PsgSpec {
    /// Raw PSG latch or data byte.
    pub value: u8,
}

/// YM2413 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2413Spec {
    /// YM2413 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YM2612 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2612Spec {
    /// YM2612 port number.
    pub port: u8,
    /// Register address on the selected port.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YM2151 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2151Spec {
    /// YM2151 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// Sega PCM memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SegaPcmSpec {
    /// Byte offset in the chip's register or memory address space.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// RF5C68 memory write specification (8-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c68U8Spec {
    /// 8-bit register or memory offset.
    pub offset: u8,
    /// Value written at the offset.
    pub value: u8,
}

/// RF5C68 memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c68U16Spec {
    /// 16-bit register or memory offset.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// RF5C68 memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c68Spec {
    /// 16-bit register or memory offset.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// YM2203 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2203Spec {
    /// YM2203 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YM2608 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2608Spec {
    /// YM2608 port number.
    pub port: u8,
    /// Register address on the selected port.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YM2610 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym2610Spec {
    /// YM2610 port number.
    pub port: u8,
    /// Register address on the selected port.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YM3812 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym3812Spec {
    /// YM3812 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YM3526 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ym3526Spec {
    /// YM3526 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// Y8950 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Y8950Spec {
    /// Y8950 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YMF262 write specification (includes port selection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymf262Spec {
    /// YMF262 port number.
    pub port: u8,
    /// Register address on the selected port.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YMF278B write specification (includes port selection)..
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymf278bSpec {
    /// YMF278B port number.
    pub port: u8,
    /// Register address on the selected port.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YMF271 write specification (includes port selection)..
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymf271Spec {
    /// YMF271 port number.
    pub port: u8,
    /// Register address on the selected port.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// SCC1 write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scc1Spec {
    /// SCC1 port number.
    pub port: u8,
    /// Register address on the selected port.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// YMZ280B register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ymz280bSpec {
    /// YMZ280B register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// RF5C164 register write specification (8-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c164U8Spec {
    /// 8-bit register or memory offset.
    pub offset: u8,
    /// Value written at the offset.
    pub value: u8,
}

/// RF5C164 register write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rf5c164U16Spec {
    /// 16-bit register or memory offset.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// PWM register write specification (24-bit value in lower bits).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PwmSpec {
    /// PWM register address.
    pub register: u8,
    /// lower 24 bits are used
    pub value: u32,
}

/// AY-8910 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ay8910Spec {
    /// AY-3-8910 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// GameBoy DMG register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GbDmgSpec {
    /// Game Boy sound register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// NES APU register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NesApuSpec {
    /// NES APU register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// MultiPCM register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MultiPcmSpec {
    /// MultiPCM register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// MultiPCM bank write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MultiPcmBankSpec {
    /// MultiPCM channel number.
    pub channel: u8,
    /// Bank offset assigned to the channel.
    pub bank_offset: u16,
}

/// uPD7759 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Upd7759Spec {
    /// uPD7759 register or command address.
    pub register: u8,
    /// Value written to the register or command port.
    pub value: u8,
}

/// OKIM6258 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Okim6258Spec {
    /// OKIM6258 register or command address.
    pub register: u8,
    /// Value written to the register or command port.
    pub value: u8,
}

/// OKIM6295 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Okim6295Spec {
    /// OKIM6295 register or command address.
    pub register: u8,
    /// Value written to the register or command port.
    pub value: u8,
}

/// K054539 register write specification (16-bit register index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct K054539Spec {
    /// 16-bit K054539 register address.
    pub register: u16,
    /// Value written to the register.
    pub value: u8,
}

/// HuC6280 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Huc6280Spec {
    /// HuC6280 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// C140 register write specification (16-bit register index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct C140Spec {
    /// 16-bit C140 register address.
    pub register: u16,
    /// Value written to the register.
    pub value: u8,
}

/// K053260 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct K053260Spec {
    /// K053260 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// Pokey register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PokeySpec {
    /// POKEY register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// QSound register write specification (16-bit value).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QsoundSpec {
    /// QSound register address.
    pub register: u8,
    /// 16-bit value written to the register.
    pub value: u16,
}

/// SCSP memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScspSpec {
    /// 16-bit SCSP memory offset.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// WonderSwan memory write specification (16-bit offset).
/// This spec corresponds to the VGM opcode that writes a value to a 16-bit
/// memory offset (mm ll).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WonderSwanSpec {
    /// 16-bit WonderSwan memory offset.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// WonderSwan register write specification (8-bit register).
/// This spec corresponds to the alternate VGM opcode that writes a value to
/// a single 8-bit register address (aa).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WonderSwanRegSpec {
    /// 8-bit WonderSwan register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// VSU memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VsuSpec {
    /// 16-bit VSU memory offset.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// SAA1099 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Saa1099Spec {
    /// SAA1099 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// ES5503 register write specification (16-bit register index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Es5503Spec {
    /// 16-bit ES5503 register address.
    pub register: u16,
    /// Value written to the register.
    pub value: u8,
}

/// ES5506 (8-bit variant) register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Es5506U8Spec {
    /// 8-bit ES5506 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// ES5506 (16-bit variant) register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Es5506U16Spec {
    /// ES5506 register address.
    pub register: u8,
    /// 16-bit value written to the register.
    pub value: u16,
}

/// X1-010 memory write specification (16-bit offset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct X1010Spec {
    /// 16-bit X1-010 memory offset.
    pub offset: u16,
    /// Value written at the offset.
    pub value: u8,
}

/// C352 register write specification (16-bit register and 16-bit value).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct C352Spec {
    /// 16-bit C352 register address.
    pub register: u16,
    /// 16-bit value written to the register.
    pub value: u16,
}

/// GA20 register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ga20Spec {
    /// GA20 register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// Mikey register write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MikeySpec {
    /// Mikey register address.
    pub register: u8,
    /// Value written to the register.
    pub value: u8,
}

/// Game Gear PSG write specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameGearPsgSpec {
    /// Raw Game Gear PSG latch or data byte.
    pub value: u8,
}
